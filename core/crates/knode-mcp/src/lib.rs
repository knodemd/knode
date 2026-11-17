//! The MCP server agents use to search, read and write a Knode space.
//!
//! [`KnodeServer`] answers the [Model Context Protocol](https://modelcontextprotocol.io) with
//! eleven tools, `search`, `read_node`, `write_node`, `link_nodes`, `recent`, `history`,
//! `undo`, `link_kinds`, `read_canvas`, `attach_file` and `read_attachment` (see [`tools`]),
//! on top of a [`Backend`]: [`ServiceBackend`] through Knode Service, or [`DirectBackend`],
//! which opens the space itself when the service cannot run. Through the service, the agent
//! connects with its own key: Knode Service checks its grants on every call and records its
//! writes under the agent's name, such as `claude-code`.
//!
//! When the space is not set up yet (see [`knode_vault::onboarding::is_set_up`]), the
//! instructions the server sends to an agent that runs commands on this machine (Claude Code,
//! Codex) ask it to offer to set Knode up for the person with the `setup_guide` tool, so
//! connecting an agent is enough to get started. Other agents are asked to send the person to
//! Get started in the Knode app.
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
use std::sync::{Arc, OnceLock};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeRequestParams, InitializeResult, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;

pub use backend::{
    Backend, BackendError, BackendResult, BodyDoc, DirectBackend, LocalFolder, SearchResults,
    Written,
};
#[cfg(unix)]
pub use service::ServiceBackend;

/// The name the server gives itself to clients.
pub const SERVER_NAME: &str = "knode";

/// The agent name used when a client does not say who it is.
pub const UNKNOWN_AGENT: &str = "mcp-client";

/// What the server tells agents when they connect.
pub const INSTRUCTIONS: &str = "Knode is the user's long-term memory, shared by all their AI \
agents and their Knode app. Every node is Markdown with a title. Search it before answering \
questions about earlier work, decisions, preferences or project context; when the work is about \
a project, projects lists its notes directly. Save what is worth \
remembering beyond this conversation (decisions and their reasons, findings, plans, summaries) \
with write_node, one topic per node, and link related nodes with [[Title]] in the text or with \
link_nodes, with a kind from link_kinds that says how they relate. Search first and update an \
existing node instead of creating a duplicate. history shows who changed what; undo takes back a \
change of your own. Use read_excerpt for bounded body ranges of long notes; its snapshot is for continuation, not a full-note rewrite. Read surrounding ranges when qualifications may be missing and read_node before rewriting a note.";

/// What the server adds to [`INSTRUCTIONS`] when the space it serves is not set up yet and the
/// agent runs commands on this machine (see [`runs_commands`]), which the guide needs.
pub const SETUP_INSTRUCTIONS: &str = "This person has just connected Knode and their memory is \
empty: Knode is not set up yet. Offer once, in a sentence, to set it up for them now (bring \
their notes, projects and their agents' memory into Knode); start only if they agree. Then call \
the setup_guide tool and follow it step by step for the space you are connected to, asking only \
what it says to ask, in the person's language.";

/// What the server adds to [`INSTRUCTIONS`] when the space is not set up yet and the agent may
/// not run commands on this machine, such as Claude Desktop: the guide needs the `knode`
/// command, so the agent sends the person to the app instead.
pub const SETUP_IN_APP_INSTRUCTIONS: &str = "Knode is not set up yet: this person's memory is \
empty. Setting it up runs commands on their computer. If they ask about Knode or their memory, \
tell them to open the Knode app and choose Get started.";

/// The MCP clients that run commands on this machine, by the name they give in `initialize`:
/// Claude Code (`claude-code`) and Codex (`codex-mcp-client`, `codex`). Any other client,
/// including one that gives no name, counts as one that cannot, so it is never asked to follow
/// a guide it cannot run.
pub fn runs_commands(client: &str) -> bool {
    let client = client.trim().to_ascii_lowercase();
    client == "claude-code" || client == "codex" || client.starts_with("codex-")
}

/// The version of this crate.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// An MCP server over a [`Backend`].
#[derive(Debug)]
pub struct KnodeServer<B> {
    backend: Arc<B>,
    /// Whether the space is set up, asked once, the first time the instructions are needed.
    set_up: Arc<OnceLock<bool>>,
}

impl<B> Clone for KnodeServer<B> {
    fn clone(&self) -> Self {
        KnodeServer {
            backend: Arc::clone(&self.backend),
            set_up: Arc::clone(&self.set_up),
        }
    }
}

impl<B: Backend> KnodeServer<B> {
    /// A server over `backend`.
    pub fn new(backend: B) -> Self {
        KnodeServer {
            backend: Arc::new(backend),
            set_up: Arc::default(),
        }
    }

    /// The backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// What the server tells the agent `client` (the name it gave in `initialize`) when it
    /// connects: [`INSTRUCTIONS`], and after them, when the space is not set up yet,
    /// [`SETUP_INSTRUCTIONS`] for an agent that [`runs_commands`] or
    /// [`SETUP_IN_APP_INSTRUCTIONS`] for any other. Whether it is set up is asked of the backend
    /// once and kept; a space that cannot be asked counts as set up.
    pub fn instructions(&self, client: Option<&str>) -> String {
        let set_up = *self
            .set_up
            .get_or_init(|| self.backend.is_set_up().unwrap_or(true));
        if set_up {
            INSTRUCTIONS.to_string()
        } else if client.is_some_and(runs_commands) {
            format!("{INSTRUCTIONS}\n\n{SETUP_INSTRUCTIONS}")
        } else {
            format!("{INSTRUCTIONS}\n\n{SETUP_IN_APP_INSTRUCTIONS}")
        }
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
            .with_instructions(self.instructions(None))
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        context.peer.set_peer_info(request.clone());
        let mut result = self.negotiate_initialize(&request)?;
        result.instructions = Some(self.instructions(Some(&request.client_info.name)));
        Ok(result)
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
        let setup = tools.iter().find(|t| t.name == "setup_guide").unwrap();
        let hints = setup.annotations.as_ref().unwrap();
        assert_eq!(setup.title.as_deref(), Some("Set up Knode"));
        assert_eq!(hints.read_only_hint, Some(true));
        assert_eq!(hints.idempotent_hint, Some(true));
        // Only the instructions for a space that is not set up nudge agents to it, and only
        // agents that can run it.
        assert!(!INSTRUCTIONS.contains("setup_guide"));
        assert!(SETUP_INSTRUCTIONS.contains("setup_guide"));
        assert!(!SETUP_IN_APP_INSTRUCTIONS.contains("setup_guide"));
    }

    fn instructions_of(space: knode_format::Space) -> String {
        KnodeServer::new(DirectBackend::from_space(space)).instructions(Some("claude-code"))
    }

    #[test]
    fn only_agents_that_run_commands_are_asked_to_set_up_and_only_as_an_offer() {
        assert!(runs_commands("claude-code"));
        assert!(runs_commands("codex-mcp-client"));
        assert!(runs_commands("Codex"));
        for other in [
            "claude-ai",
            "Claude Desktop",
            "cursor-vscode",
            "",
            UNKNOWN_AGENT,
        ] {
            assert!(!runs_commands(other), "{other}");
        }
        // An offer: the agent asks first.
        assert!(!SETUP_INSTRUCTIONS.contains("without asking"));
        assert!(SETUP_INSTRUCTIONS.contains("only if they agree"));
        assert!(SETUP_IN_APP_INSTRUCTIONS.contains("Get started"));

        let dir = tempfile::tempdir().unwrap();
        let fresh = |name: &str| {
            let path = dir.path().join(format!("{name}.knode"));
            KnodeServer::new(DirectBackend::from_space(
                knode_format::Space::create(path, name).unwrap(),
            ))
        };
        let shell = fresh("Shell").instructions(Some("claude-code"));
        assert!(shell.ends_with(SETUP_INSTRUCTIONS), "{shell}");
        for (name, client) in [("Desktop", Some("claude-ai")), ("Nameless", None)] {
            let text = fresh(name).instructions(client);
            assert!(text.starts_with(INSTRUCTIONS));
            assert!(text.ends_with(SETUP_IN_APP_INSTRUCTIONS), "{text}");
            assert!(!text.contains("setup_guide"), "{text}");
        }
        // What the server says before it knows the client is for an agent without a shell.
        let info = fresh("Info").get_info().instructions.unwrap_or_default();
        assert!(info.ends_with(SETUP_IN_APP_INSTRUCTIONS), "{info}");
    }

    #[test]
    fn asks_the_agent_to_set_up_a_space_that_is_not_set_up() {
        use knode_format::{NewNode, Provenance, Space, kinds};
        let dir = tempfile::tempdir().unwrap();

        let fresh = Space::create(dir.path().join("Fresh.knode"), "Fresh").unwrap();
        let text = instructions_of(fresh);
        assert!(text.starts_with(INSTRUCTIONS));
        assert!(text.ends_with(SETUP_INSTRUCTIONS), "{text}");

        let mut with_project = Space::create(dir.path().join("Work.knode"), "Work").unwrap();
        let mut project = NewNode::note("Web", "");
        project.kind = kinds::PROJECT.into();
        with_project
            .create_node(project, &Provenance::person("Ada"))
            .unwrap();
        assert_eq!(instructions_of(with_project), INSTRUCTIONS);

        let onboarded = Space::create(dir.path().join("Notes.knode"), "Notes").unwrap();
        std::fs::write(onboarded.path().join("onboarding.json"), "{}").unwrap();
        assert_eq!(instructions_of(onboarded), INSTRUCTIONS);

        // A space that cannot be opened is not pushed to be set up.
        let missing = KnodeServer::new(DirectBackend::unavailable("no space"));
        assert_eq!(missing.instructions(Some("claude-code")), INSTRUCTIONS);
        assert_eq!(
            missing.get_info().instructions.as_deref(),
            Some(INSTRUCTIONS)
        );
    }
}
