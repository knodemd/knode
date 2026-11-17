//! Claude Code's memory read into a space: a fake `.claude/projects` tree in a temporary folder,
//! matched to the projects of a temporary space.

use std::fs;
use std::path::Path;

use knode_format::{
    FolderSpec, NewNode, NodeId, Provenance, Space, folder_kinds, kinds, link_kinds,
};
use knode_vault::claude_memory::{
    DESCRIPTION_PROPERTY, MemoryOutcome, TYPE_PROPERTY, import_claude_code_memory,
};
use knode_vault::property_keys;

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn memory(projects: &Path, folder: &str, file: &str, text: &str) {
    let dir = projects.join(folder).join("memory");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(file), text).unwrap();
}

fn memory_file(name: &str, kind: &str, body: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: About {name}\nmetadata:\n  type: {kind}\n---\n\n{body}\n"
    )
}

fn project(space: &mut Space, title: &str) -> NodeId {
    space
        .create_node(
            NewNode {
                kind: kinds::PROJECT.into(),
                title: title.into(),
                ..Default::default()
            },
            &me(),
        )
        .unwrap()
        .id
}

fn project_folder(space: &mut Space, path: &str) {
    space
        .set_folder(
            &FolderSpec {
                id: None,
                kind: folder_kinds::PROJECT.into(),
                path: path.into(),
                capture: false,
                rules: serde_json::json!({}),
            },
            &me(),
        )
        .unwrap();
}

/// The projects a node is `part-of`.
fn parts(space: &Space, id: &NodeId) -> Vec<NodeId> {
    space
        .links_from(id)
        .unwrap()
        .into_iter()
        .filter(|l| l.kind == link_kinds::PART_OF)
        .map(|l| l.to)
        .collect()
}

fn node_at(space: &Space, path: &str) -> NodeId {
    let found = space.find_by_property(property_keys::PATH, path).unwrap();
    assert_eq!(found.len(), 1, "{path}");
    found[0].id
}

#[test]
fn memory_is_linked_to_the_project_folders_of_the_space() {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join(".claude");
    let projects = claude.join("projects");
    memory(
        &projects,
        "-Users-ada-Work-web-app",
        "build.md",
        &memory_file("Build steps", "project", "Run `make` first."),
    );
    memory(
        &projects,
        "-Users-ada-Work-web-app",
        "MEMORY.md",
        "- [Build steps](build.md)\n",
    );
    // A folder inside the project.
    memory(
        &projects,
        "-Users-ada-Work-web-app-api",
        "api.md",
        &memory_file("API keys live in the vault", "reference", "See the vault."),
    );
    memory(
        &projects,
        "-Users-ada-Projects-DAV-",
        "mix.md",
        "---\nname: Mixing notes\nmetadata: {type: feedback}\n---\nKeep it quiet.\n",
    );
    memory(
        &projects,
        "-Users-ada-Elsewhere-misc",
        "loose.md",
        "No frontmatter here.\n",
    );
    // Another folder working on web.app, as a link to its folder: read once.
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        projects.join("-Users-ada-Work-web-app"),
        projects.join("-private-Users-ada-Work-web-app"),
    )
    .unwrap();
    // A folder with sessions and no memory.
    fs::create_dir_all(projects.join("-Users-ada-Work-nomem")).unwrap();
    fs::write(projects.join("-Users-ada-Work-nomem/s.jsonl"), "{}\n").unwrap();

    let mut space = Space::create(dir.path().join("Work.knode"), "Work").unwrap();
    let work = project(&mut space, "Work");
    let web = project(&mut space, "web.app");
    let dav = project(&mut space, "DAV+");
    project_folder(&mut space, "/Users/ada/Work");
    project_folder(&mut space, "/Users/ada/Work/web.app");
    project_folder(&mut space, "/Users/ada/Projects/DAV+");
    let before = space.node_count().unwrap();

    // The .claude folder is taken for its projects folder.
    let report = import_claude_code_memory(&mut space, &claude, &me()).unwrap();
    assert_eq!(report.count(MemoryOutcome::Created), 4, "{report:#?}");
    assert_eq!(space.node_count().unwrap(), before + 4);
    assert_eq!(report.linked.get("web.app"), Some(&2));
    assert_eq!(report.linked.get("DAV+"), Some(&1));
    assert_eq!(report.linked.get("Work"), None, "the longest folder wins");
    assert_eq!(report.unmatched, vec!["-Users-ada-Elsewhere-misc"]);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(
        report
            .left_out
            .contains(&"-Users-ada-Work-web-app/memory/MEMORY.md".to_string())
    );
    #[cfg(unix)]
    assert!(
        report
            .left_out
            .contains(&"-private-Users-ada-Work-web-app".to_string())
    );

    let build = node_at(
        &space,
        "claude-code-memory/-Users-ada-Work-web-app/build.md",
    );
    let node = space.get_node(&build).unwrap().unwrap();
    assert_eq!(node.title, "Build steps");
    assert!(node.body.contains("Run `make` first."));
    assert!(
        node.created_by
            .source
            .as_deref()
            .unwrap()
            .ends_with("projects/-Users-ada-Work-web-app/memory/build.md")
    );
    assert_eq!(
        space.property(&build, TYPE_PROPERTY).unwrap().as_deref(),
        Some("project")
    );
    assert_eq!(
        space
            .property(&build, DESCRIPTION_PROPERTY)
            .unwrap()
            .as_deref(),
        Some("About Build steps")
    );
    assert_eq!(parts(&space, &build), vec![web]);
    let api = node_at(
        &space,
        "claude-code-memory/-Users-ada-Work-web-app-api/api.md",
    );
    assert_eq!(parts(&space, &api), vec![web]);
    let mix = node_at(&space, "claude-code-memory/-Users-ada-Projects-DAV-/mix.md");
    assert_eq!(parts(&space, &mix), vec![dav]);
    assert_eq!(
        space.property(&mix, TYPE_PROPERTY).unwrap().as_deref(),
        Some("feedback")
    );
    let loose = node_at(
        &space,
        "claude-code-memory/-Users-ada-Elsewhere-misc/loose.md",
    );
    assert!(parts(&space, &loose).is_empty());
    assert_eq!(space.get_node(&loose).unwrap().unwrap().title, "loose");
    assert!(space.project_notes(&work, 10).unwrap().is_empty());
    assert!(
        space
            .find_by_property(
                property_keys::PATH,
                "claude-code-memory/-Users-ada-Work-web-app/MEMORY.md"
            )
            .unwrap()
            .is_empty()
    );

    // Again: nothing new, nothing linked twice.
    let links = space.links_from(&build).unwrap().len();
    let again = import_claude_code_memory(&mut space, &projects, &me()).unwrap();
    assert_eq!(again.count(MemoryOutcome::Unchanged), 4, "{again:#?}");
    assert_eq!(again.count(MemoryOutcome::Created), 0);
    assert_eq!(space.node_count().unwrap(), before + 4);
    assert_eq!(space.links_from(&build).unwrap().len(), links);

    // A memory that changed updates its note.
    memory(
        &projects,
        "-Users-ada-Work-web-app",
        "build.md",
        &memory_file("Build steps", "project", "Run `make all` first."),
    );
    let third = import_claude_code_memory(&mut space, &projects, &me()).unwrap();
    assert_eq!(third.count(MemoryOutcome::Updated), 1, "{third:#?}");
    assert_eq!(third.count(MemoryOutcome::Unchanged), 3);
    assert!(
        space
            .get_node(&build)
            .unwrap()
            .unwrap()
            .body
            .contains("make all")
    );
    assert_eq!(space.node_count().unwrap(), before + 4);
}

#[test]
fn a_space_made_by_onboarding_matches_by_its_source_and_project_names() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    memory(
        &projects,
        "-Users-ada-Proyectos-web",
        "a.md",
        &memory_file("Web deploys on Fridays", "project", "Never on Mondays."),
    );
    memory(
        &projects,
        "-Users-ada-Proyectos-Clients-api-src",
        "b.md",
        &memory_file("API uses tokens", "project", "Bearer tokens."),
    );
    // No folder in the receipt: a project titled as the folder.
    memory(
        &projects,
        "-Users-ada-Code-Notes-App",
        "c.md",
        &memory_file("Notes app uses SwiftUI", "project", "SwiftUI."),
    );
    memory(
        &projects,
        "-Users-ada-Code-Other",
        "d.md",
        &memory_file("Nothing to see", "user", "Here."),
    );

    let package = dir.path().join("Imported.knode");
    let mut space = Space::create(&package, "Imported").unwrap();
    let web = project(&mut space, "web");
    let api = project(&mut space, "api");
    let notes = project(&mut space, "Notes App");
    let receipt = serde_json::json!({
        "space_id": "x",
        "destination": package,
        "plan": {
            "version": 1,
            "mode": "vault",
            "source": "/Users/ada/Proyectos",
            "groups": [
                { "id": "web", "name": "web" },
                { "id": "Clients/api", "name": "api" },
                { "id": "shared", "name": "shared" }
            ],
            "documents": [],
            "files": [],
            "warnings": []
        },
        "import": {},
        "contexts": { "web": web.to_string() },
        "unresolved": {}
    });
    fs::write(
        package.join("onboarding.json"),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();

    let report = import_claude_code_memory(&mut space, &projects, &me()).unwrap();
    assert_eq!(report.count(MemoryOutcome::Created), 4, "{report:#?}");
    let a = node_at(&space, "claude-code-memory/-Users-ada-Proyectos-web/a.md");
    assert_eq!(parts(&space, &a), vec![web]);
    let b = node_at(
        &space,
        "claude-code-memory/-Users-ada-Proyectos-Clients-api-src/b.md",
    );
    assert_eq!(parts(&space, &b), vec![api]);
    let c = node_at(&space, "claude-code-memory/-Users-ada-Code-Notes-App/c.md");
    assert_eq!(parts(&space, &c), vec![notes]);
    assert_eq!(report.unmatched, vec!["-Users-ada-Code-Other"]);
    assert_eq!(space.projects().unwrap().len(), 3, "no project is made up");
}
