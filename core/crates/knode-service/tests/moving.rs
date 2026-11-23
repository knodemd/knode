//! Moving a project between spaces through a real service: the person moves and undoes, an
//! agent cannot, and the folders the project moved with route to its new space.

use std::path::{Path, PathBuf};

use knode_format::{GrantLevel, NewNode, NodeId, Provenance, actions, kinds, link_kinds};
use knode_service::keys::{fingerprint, generate_key};
use knode_service::protocol::codes;
use knode_service::spaces::{SpaceFolders, working_space};
use knode_service::{Client, Config, Error, FolderInput, ServiceHandle, start};
use serde_json::json;
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    _service: ServiceHandle,
    person: Client,
    personal: PathBuf,
    team: PathBuf,
}

fn me() -> Provenance {
    Provenance::person("Ada")
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        let person = Client::connect(service.socket_path()).unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let personal = root.join("Personal.knode");
        let team = root.join("Team.knode");
        person.create_space(&personal, "Personal").unwrap();
        person.create_space(&team, "Team").unwrap();
        Fixture {
            dir,
            _service: service,
            person,
            personal,
            team,
        }
    }

    fn agent(&self, name: &str) -> Client {
        let key = generate_key().unwrap();
        self.person
            .add_agent(name, "claude-code", &fingerprint(&key), &me())
            .unwrap();
        for space in [&self.personal, &self.team] {
            self.person
                .set_grant(name, space, GrantLevel::Write, &me())
                .unwrap();
        }
        Client::connect_as_agent(self.dir.path().join("home/service.sock"), &key).unwrap()
    }

    fn spaces(&self) -> Vec<SpaceFolders> {
        [(&self.personal, "Personal"), (&self.team, "Team")]
            .into_iter()
            .map(|(space, name)| SpaceFolders {
                space: space.clone(),
                name: name.into(),
                folders: self
                    .person
                    .folders(space)
                    .unwrap()
                    .into_iter()
                    .map(|f| f.folder)
                    .collect(),
                capture: true,
            })
            .collect()
    }
}

fn rpc(result: Result<impl std::fmt::Debug, Error>) -> knode_service::RpcError {
    match result {
        Err(Error::Rpc(e)) => e,
        other => panic!("expected an error from the service, got {other:?}"),
    }
}

/// Acme, a project with a note, and the folder that holds Acme, in the personal space.
fn acme(f: &Fixture) -> (NodeId, NodeId) {
    let added = f
        .person
        .set_folder(
            &f.personal,
            &FolderInput {
                id: None,
                kind: "project".into(),
                path: "/Users/example/acme".into(),
                capture: true,
                rules: json!({}),
            },
            &me(),
        )
        .unwrap();
    let project = added.project.unwrap().node.id;
    let note = f
        .person
        .create_node(&f.personal, NewNode::note("Plan", "Ship it."), &me())
        .unwrap()
        .id;
    f.person
        .link(&f.personal, &note, &project, link_kinds::PART_OF, &me())
        .unwrap();
    (project, note)
}

fn space_of<'a>(spaces: &'a [SpaceFolders], folder: &str) -> Option<&'a Path> {
    working_space(spaces, Path::new(folder)).map(|s| s.space.as_path())
}

#[test]
fn a_person_moves_a_project_and_its_folder_routes_to_the_new_space() {
    let f = Fixture::new();
    let (project, note) = acme(&f);
    assert_eq!(
        space_of(&f.spaces(), "/Users/example/acme/app"),
        Some(f.personal.as_path())
    );

    let preview = f
        .person
        .preview_move(&f.personal, &project, &f.team, &me())
        .unwrap();
    assert_eq!(preview.notes, 1);
    assert_eq!(preview.folders.len(), 1);
    assert!(f.person.get_node(&f.team, &note).unwrap().is_none());

    let moved = f
        .person
        .move_project(&f.personal, &project, &f.team, &me())
        .unwrap();
    assert!(f.person.get_node(&f.team, &note).unwrap().is_some());
    assert!(f.person.get_node(&f.personal, &note).unwrap().is_none());
    let projects = f.person.projects(&f.team).unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].node.kind, kinds::PROJECT);
    // The next conversation in the folder goes to the team space.
    assert_eq!(
        space_of(&f.spaces(), "/Users/example/acme/app"),
        Some(f.team.as_path())
    );
    let change = f.person.change(&f.team, moved.to_change).unwrap().unwrap();
    assert_eq!(change.change.action, actions::PROJECT_MOVE);

    // Undone from the space it left: everything is back, folder included.
    let undone = f
        .person
        .undo_move(&f.personal, moved.from_change, &f.team, &me())
        .unwrap();
    assert_eq!(undone.nodes.len(), 2);
    assert!(f.person.get_node(&f.personal, &note).unwrap().is_some());
    assert!(f.person.get_node(&f.team, &note).unwrap().is_none());
    assert_eq!(
        space_of(&f.spaces(), "/Users/example/acme/app"),
        Some(f.personal.as_path())
    );
    // A move is not undone one space at a time.
    let error = rpc(f.person.undo(&f.team, moved.to_change, true, &me()));
    assert_eq!(error.code, codes::CANNOT_UNDO);
}

#[test]
fn an_agent_cannot_move_a_project_even_with_write_in_both_spaces() {
    let f = Fixture::new();
    let (project, _) = acme(&f);
    let agent = f.agent("helper");
    let by = Provenance::agent("helper");
    for result in [
        agent
            .preview_move(&f.personal, &project, &f.team, &by)
            .map(|_| ()),
        agent
            .move_project(&f.personal, &project, &f.team, &by)
            .map(|_| ()),
    ] {
        assert_eq!(rpc(result).code, codes::FORBIDDEN);
    }
    let moved = f
        .person
        .move_project(&f.personal, &project, &f.team, &me())
        .unwrap();
    let error = rpc(agent.undo_move(&f.team, moved.to_change, &f.personal, &by));
    assert_eq!(error.code, codes::FORBIDDEN);
    assert!(f.person.get_node(&f.team, &project).unwrap().is_some());
}

#[test]
fn a_project_does_not_move_to_its_own_space() {
    let f = Fixture::new();
    let (project, _) = acme(&f);
    let error = rpc(f
        .person
        .move_project(&f.personal, &project, &f.personal, &me()));
    assert_eq!(error.code, codes::INVALID_PARAMS);
}
