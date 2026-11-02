//! Generate small, synthetic interoperability examples in a new directory.
//! `cargo run -p knode-sync --example format_examples -- /tmp/knode-examples`
use std::path::Path;

use knode_format::{
    ChunkVector, EmbeddingModelSpec, FolderSpec, GrantLevel, LinkKindSpec, NewNode, NodeUpdate,
    PositionSpec, Provenance, Space, folder_kinds,
};
use knode_sync::{BodyEdit, Docs};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn permissions(dir: &Path) -> Result<()> {
    let ada = Provenance::person("Ada");
    let mut space = Space::create(dir.join("Permissions.knode"), "Permissions example")?;
    space.set_folder(
        &FolderSpec {
            id: None,
            kind: folder_kinds::PROJECT.into(),
            path: "~/Projects/example".into(),
            capture: false,
            rules: serde_json::json!({ "exclude": ["node_modules/**"] }),
        },
        &ada,
    )?;
    let reader = space.add_agent(None, "example-reader", "codex", &ada)?;
    // Only a dummy fingerprint: these examples contain no usable credential.
    space.add_agent_key(&reader.id, &format!("sha256:{}", "0".repeat(64)), &ada)?;
    space.set_grant(&reader.id, GrantLevel::Read, &ada)?;
    let writer = space.add_agent(None, "example-writer", "claude-code", &ada)?;
    space.set_grant(&writer.id, GrantLevel::Write, &ada)?;
    let note = space.create_node(NewNode::note("Release plan", "Draft the release."), &ada)?;
    space.update_node(
        &note.id,
        NodeUpdate {
            body: Some("Draft the release and verify the installer.".into()),
            ..Default::default()
        },
        &Provenance::agent("example-writer"),
    )?;
    assert_eq!(space.history(&note.id)?.len(), 2);
    assert_eq!(space.grant(&reader.id)?, GrantLevel::Read);
    assert_eq!(space.folders()?.len(), 1);
    Ok(())
}

fn map(dir: &Path) -> Result<()> {
    let ada = Provenance::person("Ada");
    let mut space = Space::create(dir.join("Map.knode"), "Map example")?;
    let project = space.create_node(
        NewNode::note("Project", "See [[Decision]]. ![[plan.svg]]"),
        &ada,
    )?;
    let decision = space.create_node(NewNode::note("Decision", "Keep the data local."), &ada)?;
    space.set_link_kind(
        &LinkKindSpec::directed("depends-on", "depends on", "needed by"),
        &ada,
    )?;
    space.link(&project.id, &decision.id, "depends-on", &ada)?;
    let canvas = space.create_canvas(NewNode::note("Board", "Project and its decision."), &ada)?;
    space.set_positions(
        &canvas.id,
        &[
            PositionSpec::at(project.id, 0.0, 0.0),
            PositionSpec::at(decision.id, 320.0, 0.0),
        ],
        &ada,
    )?;
    space.attach(&project.id, "plan.svg", &b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 20 20\"><rect width=\"20\" height=\"20\"/></svg>"[..], &ada)?;
    // Deliberately synthetic unit vectors: demonstrate storage, not a language model.
    let model =
        space.add_embedding_model(&EmbeddingModelSpec::new("example/basis-vectors", 4), &ada)?;
    for (i, node) in space.stale_nodes(model.id, 10)?.into_iter().enumerate() {
        let chunks = node
            .chunks()
            .into_iter()
            .map(|chunk| {
                let mut vector = vec![0.0; 4];
                vector[i % 4] = 1.0;
                ChunkVector { chunk, vector }
            })
            .collect::<Vec<_>>();
        assert!(space.store_chunks(model.id, &node, &chunks)?);
    }
    assert_eq!(space.attachments(&project.id)?.len(), 1);
    assert_eq!(space.positions(&canvas.id)?.len(), 2);
    assert_eq!(
        space
            .vector_search(model.id, &[1.0, 0.0, 0.0, 0.0], 10)?
            .len(),
        3
    );
    Ok(())
}

fn editing(dir: &Path) -> Result<()> {
    let ada = Provenance::person("Ada");
    let mut space = Space::create(dir.join("Editing.knode"), "Editing example")?;
    let note = space.create_node(NewNode::note("Shared plan", "Ship v0.3."), &ada)?;
    let mut docs = Docs::new();
    let seen = docs.open_doc(&mut space, &note.id)?;
    docs.apply_local_edit(&mut space, &note.id, BodyEdit::insert(9, " together"), &ada)?;
    docs.apply_local_edit(
        &mut space,
        &note.id,
        BodyEdit::text("Release v0.3.").based_on(seen.version),
        &Provenance::agent("codex"),
    )?;
    assert_eq!(
        space.get_node(&note.id)?.unwrap().body,
        "Release v0.3 together."
    );
    assert_eq!(space.history(&note.id)?.len(), 3);
    assert!(Docs::has_doc(&space, &note.id));
    Ok(())
}

fn generate(dir: &Path) -> Result<()> {
    // Refuse existing output, including empty folders: never overwrite someone's space.
    std::fs::create_dir(dir)?;
    permissions(dir)?;
    map(dir)?;
    editing(dir)?;
    for name in ["Permissions", "Map", "Editing"] {
        let space = Space::open(dir.join(format!("{name}.knode")))?;
        assert!(space.node_count()? > 0);
        assert!(!space.search("release", 10)?.is_empty() || name == "Map");
    }
    Ok(())
}

fn main() -> Result<()> {
    let dir = std::env::args_os()
        .nth(1)
        .ok_or("usage: format_examples <new directory>")?;
    generate(Path::new(&dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_reopen_and_existing_output_is_never_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("examples");
        generate(&dir).unwrap();
        let editing = Space::open(dir.join("Editing.knode")).unwrap();
        let note = editing.find_by_title("Shared plan").unwrap().remove(0);
        assert_eq!(
            editing.get_node(&note.id).unwrap().unwrap().body,
            "Release v0.3 together."
        );
        assert!(generate(&dir).is_err());
        assert_eq!(
            Space::open(dir.join("Map.knode"))
                .unwrap()
                .node_count()
                .unwrap(),
            3
        );
    }
}
