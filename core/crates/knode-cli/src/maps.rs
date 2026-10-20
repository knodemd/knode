//! `knode links`, `knode link`, `knode kinds`, `knode attach` and `knode attachments`: typed
//! links and attachments, through Knode Service or the space opened directly.

use std::path::Path;

use anyhow::{Result, bail};
use clap::Subcommand;
use knode_format::{LinkDirection, LinkKindSpec, NodeId, Provenance, link_kinds};
use knode_mcp::Backend;

#[derive(Debug, Subcommand)]
pub enum KindsAction {
    /// List the kinds of link: the built-in kinds, then those the space defines.
    List {
        /// Print the kinds as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Define a kind of link, or change its labels and description.
    Add {
        /// The name: lower case letters, digits and -, such as depends-on.
        kind: String,
        /// How the link reads from its source, such as "depends on".
        #[arg(long)]
        label: String,
        /// How it reads from its target, such as "needed by".
        #[arg(long, value_name = "LABEL", conflicts_with = "symmetric")]
        inverse: Option<String>,
        /// The link reads the same from both nodes, such as "sibling of".
        #[arg(long)]
        symmetric: bool,
        /// What the kind means, for people and agents choosing one.
        #[arg(long)]
        description: Option<String>,
    },
    /// Remove a kind the space defined. Links of that kind must be removed first.
    Remove {
        /// The name of the kind.
        kind: String,
    },
}

/// A node by id or by its exact title.
fn resolve(backend: &dyn Backend, node: &str) -> Result<NodeId> {
    let node = node.trim();
    if let Ok(id) = NodeId::parse(node)
        && backend.get_node(&id)?.is_some()
    {
        return Ok(id);
    }
    match backend.find_by_title(node)?.first() {
        Some(found) => Ok(found.id),
        None => bail!("no node has the id or title \"{node}\"; `knode search` finds it"),
    }
}

fn title(backend: &dyn Backend, id: &NodeId) -> Result<String> {
    Ok(backend
        .get_node(id)?
        .map(|n| n.title)
        .unwrap_or_else(|| id.to_string()))
}

/// Prints the links of a node, going out then coming in, each as it reads from the node.
pub fn links(backend: &dyn Backend, node: &str, json: bool) -> Result<()> {
    let id = resolve(backend, node)?;
    let links = backend.node_links(&id)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&links)?);
        return Ok(());
    }
    let name = title(backend, &id)?;
    if links.is_empty() {
        println!("\"{name}\" has no links.");
        return Ok(());
    }
    println!("\"{name}\":");
    for link in &links {
        let arrow = if link.outgoing { "->" } else { "<-" };
        println!(
            "  {arrow} {} \"{}\"  ({}, {})",
            link.label, link.title, link.link.kind, link.node
        );
    }
    Ok(())
}

/// Links two nodes with a kind, `related` by default.
pub fn link(
    backend: &dyn Backend,
    from: &str,
    to: &str,
    kind: Option<&str>,
    by: &Provenance,
) -> Result<()> {
    let from = resolve(backend, from)?;
    let to = resolve(backend, to)?;
    let kind = kind
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .unwrap_or(link_kinds::RELATED);
    if kind == link_kinds::MENTIONS {
        bail!("mentions links come only from [[links]] in a note's text; use another kind");
    }
    let kinds = backend.link_kinds()?;
    let Some(known) = kinds.iter().find(|k| k.kind == kind) else {
        bail!(
            "this space has no link kind \"{kind}\"; `knode kinds list` shows them and `knode \
             kinds add {kind} --label ...` defines it"
        );
    };
    let link = backend.link(&from, &to, kind, by)?;
    println!(
        "Linked \"{}\" {} \"{}\" ({}).",
        link.from_title, known.label, link.to_title, link.kind
    );
    Ok(())
}

pub fn kinds(backend: &dyn Backend, action: Option<KindsAction>, by: &Provenance) -> Result<()> {
    match action.unwrap_or(KindsAction::List { json: false }) {
        KindsAction::List { json } => {
            let kinds = backend.link_kinds()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&kinds)?);
                return Ok(());
            }
            for kind in &kinds {
                let reads = match kind.direction {
                    LinkDirection::Symmetric => format!("{} (both ways)", kind.label),
                    LinkDirection::Directed => {
                        format!("{} / {}", kind.label, kind.label_from(false))
                    }
                };
                let origin = if kind.built_in { "built in" } else { "defined" };
                println!("{:<14} {reads}  [{origin}]", kind.kind);
                if !kind.description.is_empty() {
                    println!("{:<14} {}", "", kind.description);
                }
            }
            Ok(())
        }
        KindsAction::Add {
            kind,
            label,
            inverse,
            symmetric,
            description,
        } => {
            let spec = LinkKindSpec {
                kind,
                label,
                inverse_label: inverse.unwrap_or_default(),
                direction: if symmetric {
                    LinkDirection::Symmetric
                } else {
                    LinkDirection::Directed
                },
                description: description.unwrap_or_default(),
            };
            let kind = backend.set_link_kind(&spec, by)?;
            match kind.direction {
                LinkDirection::Symmetric => {
                    println!("The kind {} reads \"{}\" both ways.", kind.kind, kind.label)
                }
                LinkDirection::Directed => println!(
                    "The kind {} reads \"{}\" from its source and \"{}\" from its target.",
                    kind.kind,
                    kind.label,
                    kind.label_from(false)
                ),
            }
            Ok(())
        }
        KindsAction::Remove { kind } => {
            if backend.remove_link_kind(&kind, by)? {
                println!("Removed the kind {kind}.");
                Ok(())
            } else {
                bail!("the space defines no kind named \"{kind}\"")
            }
        }
    }
}

/// Attaches a file to a node under its file name.
pub fn attach(backend: &dyn Backend, node: &str, file: &Path, by: &Provenance) -> Result<()> {
    let id = resolve(backend, node)?;
    let file = crate::absolute(file)?;
    if !file.is_file() {
        bail!("{} is not a file", file.display());
    }
    let attachment = backend.attach_file(&id, &file, by)?;
    println!(
        "Attached {} ({}, {}) to \"{}\". Write ![[{}]] in its text to show it.",
        attachment.name,
        attachment.media_type,
        size(attachment.size),
        title(backend, &id)?,
        attachment.name
    );
    Ok(())
}

/// Prints the files attached to a node.
pub fn attachments(backend: &dyn Backend, node: &str, json: bool) -> Result<()> {
    let id = resolve(backend, node)?;
    let list = backend.attachments(&id)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    let name = title(backend, &id)?;
    if list.is_empty() {
        println!("\"{name}\" has no attachments.");
        return Ok(());
    }
    println!("\"{name}\":");
    for a in &list {
        let date = a.created_at.to_string();
        println!(
            "  {}  ({}, {}, {} by {})",
            a.name,
            a.media_type,
            size(a.size),
            &date[..10],
            a.created_by.actor
        );
    }
    Ok(())
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{} KB", b.div_ceil(1024)),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}
