//! The MCP server agents use to search, read and write a Knode space.
//!
//! [`KnodeServer`] answers the [Model Context Protocol](https://modelcontextprotocol.io) with
//! seven tools, `search`, `read_node`, `write_node`, `link_nodes`, `recent`, `history` and
//! `undo` (see [`tools`]),
//! on top of a [`Backend`]: [`ServiceBackend`] through Knode Service, or [`DirectBackend`],
//! which opens the space itself when the service cannot run. Writes are recorded as made by the agent, named after the client
//! that connected, such as `claude-code`.
//!
//! ```no_run
//! use knode_mcp::{DirectBackend, run_stdio};
//!
//! run_stdio(DirectBackend::new("/Users/example/Knode/Work.knode"))?;
//! # Ok::<(), std::io::Error>(())
//! ```

#![warn(missing_docs)]

pub mod backend;
#[cfg(unix)]
mod service;
pub mod tools;

use std::io;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;

pub use backend::{Backend, BackendError, BackendResult, DirectBackend};
#[cfg(unix)]
pub use service::ServiceBackend;

/// The name the server gives itself to clients.
pub const SERVER_NAME: &str = "knode";

/// The agent name used when a client does not say who it is.
pub const UNKNOWN_AGENT: &str = "mcp-client";

/// What the server tells agents when they connect.
pub const INSTRUCTIONS: &str = "Knode is the user's long-term memory, shared by all their AI \
agents and their Knode app. Every node is Markdown with a title. Search it before answering \
questions about earlier work, decisions, preferences or project context. Save what is worth \
remembering beyond this conversation (decisions and their reasons, findings, plans, summaries) \
with write_node, one topic per node, and link related nodes with [[Title]] in the text or with \
link_nodes. Search first and update an existing node instead of creating a duplicate. history \
shows who changed what; undo takes back a change of your own.";

/// The version of this crate.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// An MCP server over a [`Backend`].
#[derive(Debug)]
pub struct KnodeServer<B> {
    backend: Arc<B>,
}

impl<B> Clone for KnodeServer<B> {
    fn clone(&self) -> Self {
        KnodeServer {
            backend: Arc::clone(&self.backend),
        }
    }
}

impl<B: Backend> KnodeServer<B> {
    /// A server over `backend`.
    pub fn new(backend: B) -> Self {
        KnodeServer {
            backend: Arc::new(backend),
        }
    }

    /// The backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }
}

/// The tools as the MCP SDK lists them.
pub fn mcp_tools() -> Vec<Tool> {
    tools::definitions()
        .into_iter()
        .map(|def| {
            let object = |value: Value| match value {
                Value::Object(map) => Arc::new(map),
                _ => Arc::new(Default::default()),
            };
            Tool::new(def.name, def.description, object(def.input_schema))
                .with_title(def.title)
                .with_raw_output_schema(object(def.output_schema))
                .with_annotations(
                    ToolAnnotations::with_title(def.title)
                        .read_only(def.read_only)
                        .destructive(def.destructive)
                        .idempotent(def.idempotent)
                        .open_world(false),
                )
        })
        .collect()
}

impl<B: Backend> ServerHandler for KnodeServer<B> {
    fn get_info(&self) -> ServerConfig {
        let mut server = Implementation::new(SERVER_NAME, version());
        server.title = Some("Knode".into());
        server.website_url = Some("https://knode.md".into());
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(server)
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(mcp_tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let agent = context
            .client_info()
            .map(|info| info.name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| UNKNOWN_AGENT.to_string());
        let arguments = request.arguments.unwrap_or_default();
        let Some(output) = tools::call(self.backend.as_ref(), &agent, &request.name, arguments)
        else {
            return Err(ErrorData::invalid_params(
                format!(
                    "unknown tool {:?}; the tools are {}",
                    request.name,
                    tools::TOOL_NAMES.join(", ")
                ),
                None,
            ));
        };
        let content = vec![ContentBlock::text(output.text)];
        let mut result = if output.is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        };
        result.structured_content = output.structured;
        Ok(result.into())
    }
}

/// Serves `backend` over standard input and output until the client closes the connection.
/// Blocks; starts its own single-threaded runtime.
pub fn run_stdio<B: Backend>(backend: B) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let service = KnodeServer::new(backend)
            .serve(rmcp::transport::stdio())
            .await
            .map_err(io::Error::other)?;
        service.waiting().await.map_err(io::Error::other)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_a_version() {
        assert!(!version().is_empty());
    }

    #[test]
    fn lists_every_tool_with_schemas() {
        let tools = mcp_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(names, tools::TOOL_NAMES);
        for tool in &tools {
            assert!(tool.output_schema.is_some());
            assert!(tool.description.is_some());
        }
        let search = &tools[0];
        assert_eq!(
            search.annotations.as_ref().unwrap().read_only_hint,
            Some(true)
        );
    }
}
