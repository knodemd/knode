//! Projects: nodes of kind [`project`](crate::kinds::PROJECT) that other nodes belong to through
//! `part-of` links. A project inside another is `part-of` it too.

use std::collections::HashMap;

use rusqlite::{Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use unicode_normalization::UnicodeNormalization;

use crate::access::write_folder;
use crate::space::{SUMMARY_COLUMNS, SUMMARY_PATH, summary, write_new_node};
use crate::{
    Error, Folder, FolderSpec, NewNode, NodeId, NodeSummary, Properties, Provenance, Result, Space,
    Timestamp, folder_kinds, kinds, link_kinds,
};

/// A new, empty `project` node with this title.
fn project_node(title: &str) -> NewNode {
    NewNode {
        kind: kinds::PROJECT.into(),
        title: title.into(),
        ..Default::default()
    }
}

/// A project of the space, with where it sits and how much is in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// The project node.
    pub node: NodeSummary,
    /// The project it is part of, if any: the oldest such link when there are several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<NodeId>,
    /// How many nodes that are not projects are part of it.
    pub notes: u64,
    /// How many projects are part of it.
    pub projects: u64,
}

/// The `project` node a project folder holds the files of (format section 15.2): the one named
/// after the folder (see [`Space::add_folder`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderProject {
    /// The project node.
    pub node: NodeSummary,
    /// Whether adding the folder created it. `false` when the space already had it.
    #[serde(default)]
    pub created: bool,
    /// Whether the folder had been added before and already held the files of this project.
    #[serde(default, skip_serializing_if = "is_false")]
    pub already_held: bool,
    /// The project of the folder's own name that the folder did not join because it already
    /// holds the files of other folders, when adding it created a project named after its parent
    /// folders instead, such as `api (b)` next to `api`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed_over: Option<PassedOverProject>,
    /// The other folders, as recorded, that hold the files of this project too. Empty unless
    /// the space had them share it before Knode kept folders of the same name apart, or every
    /// name Knode could give the project was already taken.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_with: Vec<String>,
}

impl FolderProject {
    /// A project the space already had, with nothing more to say about it.
    pub fn found(node: NodeSummary) -> Self {
        FolderProject {
            node,
            created: false,
            already_held: false,
            passed_over: None,
            shared_with: Vec::new(),
        }
    }
}

/// A project a folder was not added to, and the folders that hold its files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassedOverProject {
    /// Its title.
    pub title: String,
    /// The folders, as recorded, that hold its files.
    pub folders: Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A folder added with [`Space::add_folder`], and the project node it holds the files of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedFolder {
    /// The folder as the space records it.
    pub folder: Folder,
    /// The project of a project folder, or `None` for other kinds and for a folder with no name
    /// of its own, such as the home folder or the root of a disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<FolderProject>,
}

/// The title of the project a project folder holds, from its recorded path (format section
/// 15.3): its last part, such as `knode` for `~/Developer/knode`, in Unicode NFC, with `[` and `]`
/// written `(` and `)` and `|` written `-`, since titles cannot hold them (`[[links]]` use them).
/// `None` for the home folder (`~`), the root of a disk and other paths with no name of their own.
pub fn folder_project_name(path: &str) -> Option<String> {
    let last = path.trim().trim_end_matches('/').rsplit('/').next()?.trim();
    let drive = last.len() == 2 && last.ends_with(':');
    if last.is_empty() || last == "~" || last == "." || last == ".." || drive {
        return None;
    }
    let title: String = last
        .nfc()
        .map(|c| match c {
            '[' => '(',
            ']' => ')',
            '|' => '-',
            c => c,
        })
        .collect();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty()).then_some(title)
}

/// The names a project folder's project can have, from the plainest: the folder's own name, then
/// that name followed by the names of its parent folders, nearest last, such as `api`, `api (b)`
/// and `api (w - b)` for `~/w/b/api`. Parent folders with no name of their own (the home folder,
/// the root of a disk) end the list. Empty for a folder with no name.
fn folder_project_names(path: &str) -> Vec<String> {
    let Some(base) = folder_project_name(path) else {
        return Vec::new();
    };
    let mut names = vec![base.clone()];
    let mut parents: Vec<String> = Vec::new();
    let mut rest = path.trim().trim_end_matches('/');
    while let Some((parent, _)) = rest.rsplit_once('/') {
        let Some(name) = folder_project_name(parent) else {
            break;
        };
        parents.insert(0, name);
        names.push(format!("{base} ({})", parents.join(" - ")));
        rest = parent.trim_end_matches('/');
    }
    names
}

/// How project names are compared: as [`title_key`](knode_md::title_key) does, after Unicode NFC,
/// so `Café` typed and `Café` from a file system that decomposes it are the same project.
fn project_key(name: &str) -> String {
    knode_md::title_key(&name.nfc().collect::<String>())
}

/// What [`Space::add_folder`] does for a project folder.
enum FolderPlan {
    /// Creates the project of the folder's name at `index` (see [`folder_project_names`]).
    Create {
        index: usize,
        passed_over: Option<PassedOverProject>,
    },
    /// Uses this project the space has.
    Join(Box<FolderProject>),
}

/// The project a project folder at `path` holds the files of, with the index of its name among
/// [`folder_project_names`]: the one of its most specific name the space has.
fn held_project<'a>(
    path: &str,
    projects: &'a HashMap<String, NodeSummary>,
) -> Option<(usize, &'a NodeSummary)> {
    folder_project_names(path)
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, name)| projects.get(&project_key(name)).map(|node| (index, node)))
}

impl Space {
    /// The `project` node with this name, compared without regard to case, extra spaces or
    /// Unicode normalization, or `None`. With several (such as one made on each of two devices
    /// before they synced), always the oldest: the earliest `created_at`, then the smallest id,
    /// so every device finds the same one.
    pub fn project_named(&self, name: &str) -> Result<Option<NodeSummary>> {
        Ok(self.projects_by_key()?.remove(&project_key(name)))
    }

    /// Every `project` node by [`project_key`], the oldest of each name.
    fn projects_by_key(&self) -> Result<HashMap<String, NodeSummary>> {
        let mut by_key = HashMap::new();
        for node in self.query_summaries(
            "WHERE n.kind = ?1 ORDER BY n.created_at, n.id",
            rusqlite::params![kinds::PROJECT],
        )? {
            by_key.entry(project_key(&node.title)).or_insert(node);
        }
        Ok(by_key)
    }

    /// The `project` node with this name, created when the space has none (see
    /// [`project_named`](Space::project_named)). The name is used as it is, in NFC; a name a
    /// title cannot hold is refused. A note or other node with the same title is not a project
    /// and is left as it is.
    pub fn find_or_create_project(&mut self, name: &str, by: &Provenance) -> Result<FolderProject> {
        let name: String = name.trim().nfc().collect();
        if let Some(node) = self.project_named(&name)? {
            return Ok(FolderProject::found(node));
        }
        let node = self.create_node(project_node(&name), by)?;
        self.created_project(&node.id)
    }

    /// The `project` node a folder holds the files of: for a project folder, the project of the
    /// most specific of its names the space has (see [`add_folder`](Space::add_folder)), such as
    /// `api (b)` before `api`. `None` for other kinds of folder. Reads only.
    pub fn folder_project(&self, folder: &Folder) -> Result<Option<NodeSummary>> {
        if folder.kind != folder_kinds::PROJECT {
            return Ok(None);
        }
        let projects = self.projects_by_key()?;
        Ok(held_project(&folder.path, &projects).map(|(_, node)| node.clone()))
    }

    /// Adds a folder as [`set_folder`](Space::set_folder) does and, for a project folder, finds
    /// or creates the `project` node it holds the files of, in one transaction: either both are
    /// written or neither. Every client adds folders through this, so a folder and its project
    /// match. Removing the folder later keeps the node and everything part of it.
    ///
    /// The project is named after the folder ([`folder_project_name`]), and two different
    /// folders of the same name are kept apart:
    ///
    /// - a folder added again keeps the project it holds (`already_held`);
    /// - a new folder joins the space's project of its most specific name (see
    ///   [`folder_project`](Space::folder_project)) when no other folder holds that project yet,
    ///   such as one an import made;
    /// - when another folder holds it, the folder gets a new project named after its parent
    ///   folders too, such as `api (b)` for `~/w/b/api` when `~/w/a/api` holds `api`, and
    ///   `passed_over` names the project it did not join;
    /// - only when every such name is taken does it join the most specific one, and
    ///   `shared_with` names the folders it shares it with.
    ///
    /// The lookup and the writes are made in one immediate transaction, so two writers adding
    /// folders of the same name at once create one project, not two.
    pub fn add_folder(&mut self, spec: &FolderSpec, by: &Provenance) -> Result<AddedFolder> {
        let write = self.prepare_folder(spec, by)?;
        let names = if write.kind == folder_kinds::PROJECT {
            folder_project_names(&write.path)
        } else {
            Vec::new()
        };
        let mut prepared = Vec::with_capacity(names.len());
        for name in &names {
            prepared.push(self.prepare_node(project_node(name), &Properties::new())?);
        }
        let now = Timestamp::now();
        // Immediate: no other writer can add a project between the lookup and the write.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let plan = if names.is_empty() {
            None
        } else {
            Some(self.plan_folder_project(&write.id, write.exists, &write.path)?)
        };
        write_folder(&tx, &write, by, now)?;
        let mut created = None;
        if let Some(FolderPlan::Create { index, .. }) = &plan {
            let (id, content) = prepared.swap_remove(*index);
            write_new_node(&tx, &id, content, &Properties::new(), by, now)?;
            created = Some(id);
        }
        tx.commit()?;
        let folder = self
            .folder(&write.id)?
            .ok_or_else(|| Error::Invalid("the folder could not be read back".into()))?;
        let project = match (plan, created) {
            (Some(FolderPlan::Create { passed_over, .. }), Some(id)) => {
                let mut project = self.created_project(&id)?;
                project.passed_over = passed_over;
                Some(project)
            }
            (Some(FolderPlan::Join(project)), _) => Some(*project),
            _ => None,
        };
        Ok(AddedFolder { folder, project })
    }

    /// What adding the project folder `id` at `path` does to the projects of the space (see
    /// [`add_folder`](Space::add_folder)). Reads only; called inside its transaction.
    fn plan_folder_project(&self, id: &str, exists: bool, path: &str) -> Result<FolderPlan> {
        let projects = self.projects_by_key()?;
        let names = folder_project_names(path);
        // The other project folders, by the project each holds.
        let mut holders: HashMap<NodeId, Vec<String>> = HashMap::new();
        for other in self.folders()? {
            if other.id == id || other.kind != folder_kinds::PROJECT {
                continue;
            }
            if let Some((_, node)) = held_project(&other.path, &projects) {
                holders.entry(node.id).or_default().push(other.path);
            }
        }
        let others = |node: &NodeSummary| holders.get(&node.id).cloned().unwrap_or_default();
        Ok(match held_project(path, &projects) {
            None => FolderPlan::Create {
                index: 0,
                passed_over: None,
            },
            Some((_, node)) if exists => FolderPlan::Join(Box::new(FolderProject {
                already_held: true,
                shared_with: others(node),
                ..FolderProject::found(node.clone())
            })),
            Some((index, node)) => {
                let folders = others(node);
                if folders.is_empty() {
                    FolderPlan::Join(Box::new(FolderProject::found(node.clone())))
                } else if index + 1 < names.len() {
                    FolderPlan::Create {
                        index: index + 1,
                        passed_over: Some(PassedOverProject {
                            title: node.title.clone(),
                            folders,
                        }),
                    }
                } else {
                    FolderPlan::Join(Box::new(FolderProject {
                        shared_with: folders,
                        ..FolderProject::found(node.clone())
                    }))
                }
            }
        })
    }

    fn created_project(&self, id: &NodeId) -> Result<FolderProject> {
        let node = self
            .query_summaries("WHERE n.id = ?1", [id.to_string()])?
            .pop()
            .ok_or_else(|| Error::Invalid("the project could not be read back".into()))?;
        Ok(FolderProject {
            created: true,
            ..FolderProject::found(node)
        })
    }

    /// Every project of the space, by title.
    pub fn projects(&self) -> Result<Vec<Project>> {
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS},
                    (SELECT l.target_id FROM links l JOIN nodes p ON p.id = l.target_id
                     WHERE l.source_id = n.id AND l.kind = ?2 AND p.kind = ?1
                     ORDER BY l.created_at, l.target_id LIMIT 1),
                    (SELECT count(*) FROM links l JOIN nodes c ON c.id = l.source_id
                     WHERE l.target_id = n.id AND l.kind = ?2 AND c.kind != ?1),
                    (SELECT count(*) FROM links l JOIN nodes c ON c.id = l.source_id
                     WHERE l.target_id = n.id AND l.kind = ?2 AND c.kind = ?1),
                    {SUMMARY_PATH}
             FROM nodes n JOIN provenance u ON u.id = n.updated_by
             WHERE n.kind = ?1
             ORDER BY n.title_key, n.seq"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params![kinds::PROJECT, link_kinds::PART_OF],
            |row| {
                let parent: Option<String> = row.get(9)?;
                Ok(Project {
                    node: summary(row)?,
                    parent: parent.and_then(|p| NodeId::parse(&p).ok()),
                    notes: row.get::<_, i64>(10)?.try_into().unwrap_or(0),
                    projects: row.get::<_, i64>(11)?.try_into().unwrap_or(0),
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The nodes that are part of a project and are not projects themselves, most recently
    /// changed first, at most `limit`.
    pub fn project_notes(&self, project: &NodeId, limit: usize) -> Result<Vec<NodeSummary>> {
        self.query_summaries(
            "JOIN links l ON l.source_id = n.id AND l.kind = ?2 AND l.target_id = ?1
             WHERE n.kind != ?3
             ORDER BY n.updated_at DESC, n.seq DESC LIMIT ?4",
            rusqlite::params![
                project.to_string(),
                link_kinds::PART_OF,
                kinds::PROJECT,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{folder_project_name, folder_project_names};
    use crate::{
        FolderSpec, ImportedNode, NewNode, NodeId, Properties, Provenance, Space, Timestamp,
        folder_kinds, kinds, link_kinds,
    };

    fn folder(kind: &str, path: &str) -> FolderSpec {
        FolderSpec {
            id: None,
            kind: kind.into(),
            path: path.into(),
            capture: true,
            rules: serde_json::json!({}),
        }
    }

    #[test]
    fn a_project_folder_is_named_after_its_last_part() {
        let name = |path: &str| folder_project_name(path);
        assert_eq!(name("~/Developer/knode").as_deref(), Some("knode"));
        assert_eq!(name("/Volumes/Work/site/").as_deref(), Some("site"));
        assert_eq!(name("C:/Users/ada/My App").as_deref(), Some("My App"));
        assert_eq!(name("knode").as_deref(), Some("knode"));
        // Characters a title cannot hold are replaced, and the name is in NFC.
        assert_eq!(
            name("~/Developer/site [old]").as_deref(),
            Some("site (old)")
        );
        assert_eq!(name("~/a|b").as_deref(), Some("a-b"));
        assert_eq!(name("~/Cafe\u{301}").as_deref(), Some("Caf\u{e9}"));
        for nameless in ["~", "/", "", "C:", "C:/", "  "] {
            assert_eq!(name(nameless), None, "{nameless:?}");
        }
    }

    #[test]
    fn a_folder_with_brackets_still_gets_its_project() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let by = Provenance::person("Tester");
        let spec = folder(folder_kinds::PROJECT, "~/Developer/site [old]");
        let added = space.add_folder(&spec, &by).unwrap();
        let project = added.project.unwrap();
        assert!(project.created);
        assert_eq!(project.node.title, "site (old)");
        // Adding it again finds the same project.
        let again = space.add_folder(&spec, &by).unwrap();
        assert_eq!(again.project.unwrap().node.id, project.node.id);
        assert_eq!(space.folders().unwrap().len(), 1);
        assert_eq!(space.projects().unwrap().len(), 1);
    }

    #[test]
    fn decomposed_and_composed_names_are_one_project() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let by = Provenance::person("Tester");
        // A project written by another app, decomposed (NFD).
        let nfd = space
            .create_node(
                NewNode {
                    kind: kinds::PROJECT.into(),
                    title: "Cafe\u{301}".into(),
                    ..Default::default()
                },
                &by,
            )
            .unwrap()
            .id;
        let added = space
            .add_folder(&folder(folder_kinds::PROJECT, "~/Caf\u{e9}"), &by)
            .unwrap();
        assert_eq!(added.project.unwrap().node.id, nfd);
        assert_eq!(space.projects().unwrap().len(), 1);
        assert_eq!(
            space
                .find_or_create_project("caf\u{e9}", &by)
                .unwrap()
                .node
                .id,
            nfd
        );
    }

    #[test]
    fn adding_a_project_folder_creates_its_project_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let by = Provenance::person("Tester");

        let added = space
            .add_folder(&folder(folder_kinds::PROJECT, "~/Developer/knode"), &by)
            .unwrap();
        let project = added.project.expect("a project folder has a project");
        assert!(project.created);
        assert_eq!(
            (project.node.kind.as_str(), project.node.title.as_str()),
            (kinds::PROJECT, "knode")
        );

        // The same folder again finds it, and says the folder already held it.
        let again = space
            .add_folder(&folder(folder_kinds::PROJECT, "~/Developer/knode"), &by)
            .unwrap();
        assert_eq!(again.folder.id, added.folder.id);
        let again = again.project.unwrap();
        assert!(!again.created && again.already_held);
        assert!(again.shared_with.is_empty() && again.passed_over.is_none());
        assert_eq!(again.node.id, project.node.id);
        assert_eq!(space.projects().unwrap().len(), 1);
        assert_eq!(
            space.folder_project(&added.folder).unwrap().map(|n| n.id),
            Some(project.node.id)
        );

        // Notes and agent folders, and folders with no name, have no project.
        let notes = space
            .add_folder(&folder(folder_kinds::NOTES, "~/Notes"), &by)
            .unwrap();
        assert_eq!(notes.project, None);
        assert_eq!(space.folder_project(&notes.folder).unwrap(), None);
        let home = space
            .add_folder(&folder(folder_kinds::PROJECT, "~"), &by)
            .unwrap();
        assert_eq!(home.project, None);
        assert_eq!(space.projects().unwrap().len(), 1);

        // Only a person adds folders, so nothing is written for anyone else: no folder, no node.
        let agent = Provenance::agent("claude-code");
        let folders = space.folders().unwrap().len();
        assert!(
            space
                .add_folder(&folder(folder_kinds::PROJECT, "~/Developer/other"), &agent)
                .is_err()
        );
        assert!(space.project_named("other").unwrap().is_none());
        assert_eq!(space.folders().unwrap().len(), folders);
    }

    #[test]
    fn a_project_folder_finds_the_project_the_space_has() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let by = Provenance::person("Tester");
        // A note of the same name is not a project; the oldest project of the name wins.
        space
            .create_node(NewNode::note("Site", "Notes about the site"), &by)
            .unwrap();
        let project = |space: &mut Space, title: &str| {
            space
                .create_node(
                    NewNode {
                        kind: kinds::PROJECT.into(),
                        title: title.into(),
                        ..Default::default()
                    },
                    &by,
                )
                .unwrap()
                .id
        };
        let older = project(&mut space, "Site");
        project(&mut space, "site");
        let plan = space
            .create_node(NewNode::note("Plan", ""), &by)
            .unwrap()
            .id;
        space.link(&plan, &older, link_kinds::PART_OF, &by).unwrap();

        let added = space
            .add_folder(&folder(folder_kinds::PROJECT, "/Volumes/Work/site"), &by)
            .unwrap();
        let found = added.project.unwrap();
        assert!(!found.created && !found.already_held);
        assert_eq!(found.node.id, older);

        // Removing the folder keeps the project and what is part of it.
        assert!(space.remove_folder(&added.folder.id, &by).unwrap());
        assert!(space.get_node(&older).unwrap().is_some());
        assert_eq!(space.project_notes(&older, 10).unwrap().len(), 1);
        assert!(space.find_or_create_project("  ", &by).is_err());
    }

    #[test]
    fn a_folder_has_its_name_then_its_parents() {
        assert_eq!(
            folder_project_names("~/w/b/api"),
            ["api", "api (b)", "api (w - b)"]
        );
        assert_eq!(
            folder_project_names("/Volumes/Work/site [old]/"),
            [
                "site (old)",
                "site (old) (Work)",
                "site (old) (Volumes - Work)"
            ]
        );
        assert_eq!(folder_project_names("~/api"), ["api"]);
        assert_eq!(folder_project_names("C:/api"), ["api"]);
        assert!(folder_project_names("~").is_empty());
    }

    #[test]
    fn project_named_returns_the_oldest_by_creation() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("T.knode"), "T").unwrap();
        let by = Provenance::person("Ada");
        let local = space.find_or_create_project("api", &by).unwrap().node;
        // A project made on another device, earlier, arrives later: it has a later local row.
        let remote = NodeId::parse("01JB7Q9W8K3M2N4P5R6S7T8V9X").unwrap();
        let old = Timestamp(1_600_000_000_000);
        space
            .import_node(
                ImportedNode {
                    id: remote,
                    node: NewNode {
                        kind: kinds::PROJECT.into(),
                        title: "API".into(),
                        ..Default::default()
                    },
                    created_at: old,
                    updated_at: old,
                    created_by: Provenance::person("Grace"),
                    updated_by: Provenance::person("Grace"),
                },
                &Properties::new(),
                &by,
            )
            .unwrap();
        assert_ne!(local.id, remote);
        assert_eq!(space.project_named("api").unwrap().unwrap().id, remote);
        // A project folder of that name finds the same one.
        let added = space
            .add_folder(&folder(folder_kinds::PROJECT, "~/code/api"), &by)
            .unwrap();
        assert_eq!(added.project.unwrap().node.id, remote);
    }

    #[test]
    fn two_folders_with_the_same_name_get_their_own_projects() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let by = Provenance::person("Tester");
        let a = folder(folder_kinds::PROJECT, "~/w/a/api");
        let b = folder(folder_kinds::PROJECT, "~/w/b/api");

        let first = space.add_folder(&a, &by).unwrap().project.unwrap();
        assert!(first.created && first.passed_over.is_none());
        assert_eq!(first.node.title, "api");

        // Another folder named api does not join the project of ~/w/a/api.
        let added_b = space.add_folder(&b, &by).unwrap();
        let second = added_b.project.unwrap();
        assert!(second.created);
        assert_eq!(second.node.title, "api (b)");
        assert_ne!(second.node.id, first.node.id);
        let passed = second.passed_over.unwrap();
        assert_eq!(
            (passed.title.as_str(), passed.folders.as_slice()),
            ("api", ["~/w/a/api".to_string()].as_slice())
        );

        // Adding either again keeps its own project and says so.
        let again = space.add_folder(&a, &by).unwrap().project.unwrap();
        assert_eq!(again.node.id, first.node.id);
        assert!(!again.created && again.already_held && again.passed_over.is_none());
        let again = space.add_folder(&b, &by).unwrap().project.unwrap();
        assert_eq!(again.node.id, second.node.id);
        assert!(again.already_held);
        assert_eq!(
            space.folder_project(&added_b.folder).unwrap().map(|n| n.id),
            Some(second.node.id)
        );

        // A third folder named api under another b goes one parent further up.
        let third = space
            .add_folder(&folder(folder_kinds::PROJECT, "~/v/b/api"), &by)
            .unwrap()
            .project
            .unwrap();
        assert_eq!(third.node.title, "api (v - b)");
        assert_eq!(third.passed_over.unwrap().title, "api (b)");

        // Removing a folder does not move another folder to its project.
        let a_id = space
            .folders()
            .unwrap()
            .into_iter()
            .find(|f| f.path == "~/w/a/api")
            .unwrap()
            .id;
        assert!(space.remove_folder(&a_id, &by).unwrap());
        assert_eq!(
            space.folder_project(&added_b.folder).unwrap().map(|n| n.id),
            Some(second.node.id)
        );
        // The project api now holds no folder, so the next folder named api joins it.
        let joined = space
            .add_folder(&folder(folder_kinds::PROJECT, "~/x/api"), &by)
            .unwrap()
            .project
            .unwrap();
        assert_eq!(joined.node.id, first.node.id);
        assert!(!joined.created && joined.passed_over.is_none());

        // When every name is taken, the folder shares the most specific one and says with whom.
        let shared = space
            .add_folder(&folder(folder_kinds::PROJECT, "~/api"), &by)
            .unwrap()
            .project
            .unwrap();
        assert_eq!(shared.node.id, first.node.id);
        assert_eq!(shared.shared_with, ["~/x/api"]);
        assert_eq!(space.projects().unwrap().len(), 3);
    }

    #[test]
    fn concurrent_writers_create_one_project_of_a_name() {
        // Two processes add folders named api to one space at the same time: the lookup and the
        // write happen in one immediate transaction, so one creates api and the other, seeing
        // it, creates api (b). Without it both could create api.
        for round in 0..8 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("S.knode");
            drop(Space::create(&path, "S").unwrap());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let writers: Vec<_> = ["~/w/a/api", "~/w/b/api"]
                .into_iter()
                .map(|folder_path| {
                    let path = path.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        let mut space = Space::open(&path).unwrap();
                        barrier.wait();
                        space
                            .add_folder(
                                &folder(folder_kinds::PROJECT, folder_path),
                                &Provenance::person("Tester"),
                            )
                            .unwrap()
                    })
                })
                .collect();
            for writer in writers {
                writer.join().unwrap();
            }
            let space = Space::open(&path).unwrap();
            let mut titles: Vec<_> = space
                .projects()
                .unwrap()
                .into_iter()
                .map(|p| p.node.title)
                .collect();
            titles.sort();
            assert_eq!(titles.len(), 2, "round {round}: {titles:?}");
            assert!(
                titles.contains(&"api".to_string()),
                "round {round}: {titles:?}"
            );
        }
    }

    #[test]
    fn projects_with_their_parent_notes_and_projects() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let by = Provenance::person("Tester");
        let project = |space: &mut Space, title: &str| {
            space
                .create_node(
                    NewNode {
                        kind: kinds::PROJECT.into(),
                        title: title.into(),
                        ..Default::default()
                    },
                    &by,
                )
                .unwrap()
                .id
        };
        let org = project(&mut space, "Org");
        let app = project(&mut space, "app");
        space.link(&app, &org, link_kinds::PART_OF, &by).unwrap();
        let older = space
            .create_node(NewNode::note("Plan", ""), &by)
            .unwrap()
            .id;
        let newer = space
            .create_node(NewNode::note("Status", ""), &by)
            .unwrap()
            .id;
        let loose = space
            .create_node(NewNode::note("Loose", ""), &by)
            .unwrap()
            .id;
        for note in [older, newer] {
            space.link(&note, &app, link_kinds::PART_OF, &by).unwrap();
        }
        space.link(&loose, &app, link_kinds::RELATED, &by).unwrap();

        let projects = space.projects().unwrap();
        let titles: Vec<_> = projects.iter().map(|p| p.node.title.as_str()).collect();
        assert_eq!(titles, ["app", "Org"]);
        let app_row = &projects[0];
        assert_eq!(
            (app_row.parent, app_row.notes, app_row.projects),
            (Some(org), 2, 0)
        );
        let org_row = &projects[1];
        assert_eq!(
            (org_row.parent, org_row.notes, org_row.projects),
            (None, 0, 1)
        );

        let notes: Vec<_> = space
            .project_notes(&app, 10)
            .unwrap()
            .into_iter()
            .map(|n| n.title)
            .collect();
        assert_eq!(notes, ["Status", "Plan"]);
        assert_eq!(space.project_notes(&app, 1).unwrap().len(), 1);
        assert!(space.project_notes(&org, 10).unwrap().is_empty());
    }
}
