//! Projects: nodes of kind [`project`](crate::kinds::PROJECT) that other nodes belong to through
//! `part-of` links. A project inside another is `part-of` it too.

use serde::{Deserialize, Serialize};

use unicode_normalization::UnicodeNormalization;

use rusqlite::{Transaction, TransactionBehavior};

use crate::access::{FolderWrite, write_folder};
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

/// What [`Space::add_folder`] does with the project of a project folder.
enum Choice {
    /// The folder holds the files of this project, which the space has.
    Found(NodeSummary),
    /// A new project, named after the folder and, when `placed`, where it is.
    Create { placed: bool },
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

/// same name as the folder, or named after where it is too (see [`Space::add_folder`]).
/// same name as the folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderProject {
    /// The project node.
    pub node: NodeSummary,
    /// Whether adding the folder created it. `false` when the space already had a project of that
    /// name, which the folder now holds the files of.
    #[serde(default)]
    pub created: bool,
    /// Whether the space already had the folder, and the folder already held the files of this
    /// project: adding it again changed nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub again: bool,
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
    title_text(last)
}

/// The title of the project of a project folder whose [name](folder_project_name) another
/// project folder of the space already uses: the name followed by the folder it is in, as
/// recorded, such as `api (~/work/b)` for `~/work/b/api`. Paths are unique, so no two folders
/// share it. `None` when the folder has no name of its own or is not in a folder.
fn folder_project_full_name(path: &str) -> Option<String> {
    let name = folder_project_name(path)?;
    let (parent, _) = path.trim().trim_end_matches('/').rsplit_once('/')?;
    let parent = if parent.is_empty() { "/" } else { parent };
    title_text(&format!("{name} ({parent})"))
}

/// Text as a title can hold it: in Unicode NFC, with `[` and `]` written `(` and `)`, `|`
/// written `-` and runs of spaces as one. `None` when nothing is left.
fn title_text(text: &str) -> Option<String> {
    let title: String = text
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

/// How project names are compared: as [`title_key`](knode_md::title_key) does, after Unicode NFC,
/// so `Café` typed and `Café` from a file system that decomposes it are the same project.
fn project_key(name: &str) -> String {
    knode_md::title_key(&name.nfc().collect::<String>())
}

impl Space {
    /// The `project` node with this name, compared without regard to case, extra spaces or
    /// Unicode normalization, or `None`. With several (such as one made on each of two devices
    /// before they synced), always the oldest: the first created, then the smallest id, so every
    /// device finds the same one.
    pub fn project_named(&self, name: &str) -> Result<Option<NodeSummary>> {
        let key = project_key(name);
        Ok(self
            .query_summaries(
                "WHERE n.kind = ?1 ORDER BY n.created_at, n.id",
                rusqlite::params![kinds::PROJECT],
            )?
            .into_iter()
            .find(|node| project_key(&node.title) == key))
    }

    /// The `project` node with this name, created when the space has none (see
    /// [`project_named`](Space::project_named)). The name is used as it is, in NFC; a name a
    /// title cannot hold is refused. A note or other node with the same title is not a project
    /// and is left as it is.
    pub fn find_or_create_project(&mut self, name: &str, by: &Provenance) -> Result<FolderProject> {
        let name: String = name.trim().nfc().collect();
        if let Some(node) = self.project_named(&name)? {
            return Ok(FolderProject {
                node,
                created: false,
                again: false,
            });
        }
        let node = self.create_node(project_node(&name), by)?;
        self.created_project(&node.id)
    }

    /// The `project` node a folder holds the files of, if the space has it: for a project folder,
    /// the project named after the folder and where it is (such as `api (~/work/b)`, made when
    /// another folder already used the name `api`), else the one named after the folder alone.
    /// `None` for other kinds of folder. Reads only; see [`add_folder`](Space::add_folder).
    pub fn folder_project(&self, folder: &Folder) -> Result<Option<NodeSummary>> {
        if folder.kind != folder_kinds::PROJECT {
            return Ok(None);
        }
        if let Some(full) = folder_project_full_name(&folder.path)
            && let Some(node) = self.project_named(&full)?
        {
            return Ok(Some(node));
        }
        match folder_project_name(&folder.path) {
            Some(name) => self.project_named(&name),
            None => Ok(None),
        }
    }

    /// Adds a folder as [`set_folder`](Space::set_folder) does and, for a project folder, creates
    /// the `project` node named after it ([`folder_project_name`]) or finds the one the space
    /// already has (format section 15.2), in one transaction: either both are written or
    /// neither. Adding the same folder again changes nothing and finds the same node. Every
    /// client adds folders through this, so a folder and its project match. Removing the folder
    /// later keeps the node and everything part of it.
    ///
    /// When an older project folder in another place already holds the files of the project of
    /// that name (two repositories called `api`), the new folder gets its own project, named
    /// after the folder and where it is, such as `api (~/work/b)`, instead of mixing the notes
    /// of both. [`folder_project`](Space::folder_project) finds it again from the path alone.
    pub fn add_folder(&mut self, spec: &FolderSpec, by: &Provenance) -> Result<AddedFolder> {
        let write = self.prepare_folder(spec, by)?;
        let name = folder_project_name(&write.path).filter(|_| write.kind == folder_kinds::PROJECT);
        let full = name
            .as_ref()
            .and_then(|_| folder_project_full_name(&write.path));
        // Made ready before the transaction, which needs the space only to read; at most one is
        // written.
        let mut prepare = |title: &Option<String>| match title {
            Some(title) => self
                .prepare_node(project_node(title), &Properties::new())
                .map(Some),
            None => Ok(None),
        };
        let (named, placed) = (prepare(&name)?, prepare(&full)?);
        let now = Timestamp::now();
        // Immediate, with the lookup inside: two writers on one package cannot both find no
        // project and both create one.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let choice = match &name {
            Some(name) => Some(self.choose_folder_project(&write, name, full.as_deref())?),
            None => None,
        };
        write_folder(&tx, &write, by, now)?;
        let mut created = None;
        if let Some(Choice::Create { placed: in_place }) = &choice {
            let new = if *in_place { placed } else { named };
            let (id, content) =
                new.ok_or_else(|| Error::Invalid("the project has no name".into()))?;
            write_new_node(&tx, &id, content, &Properties::new(), by, now)?;
            created = Some(id);
        }
        tx.commit()?;
        let folder = self
            .folder(&write.id)?
            .ok_or_else(|| Error::Invalid("the folder could not be read back".into()))?;
        let project = match (created, choice) {
            (Some(id), _) => Some(self.created_project(&id)?),
            (None, Some(Choice::Found(node))) => Some(FolderProject {
                node,
                created: false,
                again: write.exists,
            }),
            (None, _) => None,
        };
        Ok(AddedFolder { folder, project })
    }

    /// Which project a project folder named `name` holds the files of, as it is being added (see
    /// [`add_folder`](Space::add_folder)): its own, named `full`, if the space has it; else the
    /// one named `name`, unless an older project folder in another place holds its files; else a
    /// new one, named `name` when the space has no project of that name and `full` when it does.
    fn choose_folder_project(
        &self,
        write: &FolderWrite,
        name: &str,
        full: Option<&str>,
    ) -> Result<Choice> {
        if let Some(full) = full
            && let Some(node) = self.project_named(full)?
        {
            return Ok(Choice::Found(node));
        }
        let Some(node) = self.project_named(name)? else {
            return Ok(Choice::Create { placed: false });
        };
        if full.is_none() {
            return Ok(Choice::Found(node));
        }
        let key = project_key(name);
        // Oldest first: only folders added before this one count, so adding an older folder
        // again keeps the project it always had.
        for other in self.folders()? {
            if other.id == write.id {
                break;
            }
            let same_name = other.kind == folder_kinds::PROJECT
                && folder_project_name(&other.path).is_some_and(|n| project_key(&n) == key);
            if same_name
                && self
                    .folder_project(&other)?
                    .is_some_and(|held| held.id == node.id)
            {
                return Ok(Choice::Create { placed: true });
            }
        }
        Ok(Choice::Found(node))
    }

    fn created_project(&self, id: &NodeId) -> Result<FolderProject> {
        let node = self
            .query_summaries("WHERE n.id = ?1", [id.to_string()])?
            .pop()
            .ok_or_else(|| Error::Invalid("the project could not be read back".into()))?;
        Ok(FolderProject {
            node,
            created: true,
            again: false,
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
    use super::folder_project_name;
    use crate::{
        Folder, FolderSpec, ImportedNode, NewNode, NodeId, Properties, Provenance, Space,
        Timestamp, folder_kinds, kinds, link_kinds,
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
        let other = space
            .add_folder(
                &folder(folder_kinds::PROJECT, "/Volumes/X/CAFE\u{301}"),
                &by,
            )
            .unwrap();
        // Another folder of that name, elsewhere, gets its own project, named in NFC too.
        let other = other.project.unwrap();
        assert_ne!(other.node.id, nfd);
        assert_eq!(other.node.title, "CAF\u{c9} (/Volumes/X)");
        assert_eq!(space.projects().unwrap().len(), 2);
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

        // The same folder again finds it.
        let again = space
            .add_folder(&folder(folder_kinds::PROJECT, "~/Developer/knode"), &by)
            .unwrap();
        assert_eq!(again.folder.id, added.folder.id);
        let again = again.project.unwrap();
        assert!(!again.created && again.again);
        assert_eq!(again.node.id, project.node.id);
        assert_eq!(space.projects().unwrap().len(), 1);
        assert_eq!(
            space.folder_project(&added.folder).unwrap().map(|n| n.id),
            Some(project.node.id)
        );
        // Another folder of the same name in another case and place gets its own.
        let other = space
            .add_folder(&folder(folder_kinds::PROJECT, "/Volumes/Backup/KNODE"), &by)
            .unwrap();
        assert_ne!(other.folder.id, added.folder.id);
        let own = other.project.unwrap();
        assert!(own.created);
        assert_eq!(own.node.title, "KNODE (/Volumes/Backup)");
        assert_eq!(space.projects().unwrap().len(), 2);
        assert_eq!(
            space.folder_project(&other.folder).unwrap().map(|n| n.id),
            Some(own.node.id)
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
        assert_eq!(space.projects().unwrap().len(), 2);

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
        assert!(!found.created);
        assert_eq!(found.node.id, older);

        // Removing the folder keeps the project and what is part of it.
        assert!(space.remove_folder(&added.folder.id, &by).unwrap());
        assert!(space.get_node(&older).unwrap().is_some());
        assert_eq!(space.project_notes(&older, 10).unwrap().len(), 1);
        assert!(space.find_or_create_project("  ", &by).is_err());
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

    #[test]
    fn project_named_returns_the_oldest_by_creation() {
        // A project made on another device before this one's, imported by sync after it: the
        // oldest by creation wins on every device, not the first row written here.
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("T.knode"), "T").unwrap();
        let by = Provenance::person("Ada");
        let local = space.find_or_create_project("api", &by).unwrap().node.id;
        let remote = NodeId::parse("01JB7Q9W8K3M2N4P5R6S7T8V9X").unwrap();
        let old = Timestamp(1_600_000_000_000);
        let imported = |id: NodeId| ImportedNode {
            id,
            node: NewNode {
                kind: kinds::PROJECT.into(),
                title: "API".into(),
                ..Default::default()
            },
            created_at: old,
            updated_at: old,
            created_by: Provenance::person("Grace"),
            updated_by: Provenance::person("Grace"),
        };
        space
            .import_node(imported(remote), &Properties::new(), &by)
            .unwrap();
        assert_ne!(local, remote);
        assert_eq!(space.project_named("api").unwrap().unwrap().id, remote);
        // Made at the same time: the smaller id wins, wherever it was written first.
        let twin = NodeId::parse("01JB7Q9W8K3M2N4P5R6S7T8V9W").unwrap();
        space
            .import_node(imported(twin), &Properties::new(), &by)
            .unwrap();
        assert_eq!(space.project_named("api").unwrap().unwrap().id, twin);
    }

    #[test]
    fn folders_with_the_same_name_in_different_places_get_their_own_projects() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let by = Provenance::person("Tester");
        let add = |space: &mut Space, path: &str| {
            let added = space
                .add_folder(&folder(folder_kinds::PROJECT, path), &by)
                .unwrap();
            (added.folder, added.project.unwrap())
        };

        let (a, api) = add(&mut space, "~/w/a/api");
        assert!(api.created && !api.again);
        assert_eq!(api.node.title, "api");
        // Another repository with the same name is another project, named after where it is.
        let (b, other) = add(&mut space, "~/w/b/api");
        assert!(other.created && !other.again);
        assert_ne!(other.node.id, api.node.id);
        assert_eq!(other.node.title, "api (~/w/b)");
        let (c, third) = add(&mut space, "/Volumes/Backup [old]/API");
        assert!(third.created);
        assert_eq!(third.node.title, "API (/Volumes/Backup (old))");
        assert_eq!(space.projects().unwrap().len(), 3);

        // Each folder keeps its own project, read back or added again.
        for (folder, project) in [(&a, &api), (&b, &other), (&c, &third)] {
            let read = space.folder_project(folder).unwrap().unwrap();
            assert_eq!(read.id, project.node.id, "{}", folder.path);
            let (_, again) = add(&mut space, &folder.path);
            assert!(!again.created && again.again, "{}", folder.path);
            assert_eq!(again.node.id, project.node.id, "{}", folder.path);
        }
        assert_eq!(space.projects().unwrap().len(), 3);

        // Removing the first folder and adding it again finds its project, not another.
        assert!(space.remove_folder(&a.id, &by).unwrap());
        let (_, back) = add(&mut space, "~/w/a/api");
        assert!(!back.created && !back.again);
        assert_eq!(back.node.id, api.node.id);
        assert_eq!(space.projects().unwrap().len(), 3);
    }

    #[test]
    fn folders_that_shared_a_project_keep_it_for_the_oldest() {
        // Two folders of one name that shared a project before each got its own: adding the
        // older again keeps the project; adding the newer again gives it its own.
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let by = Provenance::person("Tester");
        let older = space
            .set_folder(&folder(folder_kinds::PROJECT, "~/w/a/api"), &by)
            .unwrap();
        let newer = space
            .set_folder(&folder(folder_kinds::PROJECT, "~/w/b/api"), &by)
            .unwrap();
        let shared = space.find_or_create_project("api", &by).unwrap().node.id;
        for folder in [&older, &newer] {
            assert_eq!(space.folder_project(folder).unwrap().unwrap().id, shared);
        }
        let again = |space: &mut Space, folder: &Folder| {
            let spec = FolderSpec {
                id: None,
                kind: folder.kind.clone(),
                path: folder.path.clone(),
                capture: true,
                rules: serde_json::json!({}),
            };
            space.add_folder(&spec, &by).unwrap().project.unwrap()
        };
        let kept = again(&mut space, &older);
        assert!(kept.again && !kept.created);
        assert_eq!(kept.node.id, shared);
        let own = again(&mut space, &newer);
        assert!(own.created);
        assert_eq!(own.node.title, "api (~/w/b)");
        assert_eq!(
            space.folder_project(&newer).unwrap().unwrap().id,
            own.node.id
        );
        assert_eq!(space.folder_project(&older).unwrap().unwrap().id, shared);
    }

    #[test]
    fn concurrent_writers_create_one_project_of_a_name() {
        // Writers on one package (the CLI without the service while the service runs) add
        // folders of the same new name at once: one project of that name, not several.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("S.knode");
        drop(Space::create(&path, "S").unwrap());
        const WRITERS: usize = 4;
        for round in 0..20 {
            let name = format!("app{round}");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));
            let handles: Vec<_> = (0..WRITERS)
                .map(|writer| {
                    let (path, name, barrier) = (path.clone(), name.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        let mut space = Space::open(&path).unwrap();
                        let spec = folder(folder_kinds::PROJECT, &format!("~/w{writer}/{name}"));
                        barrier.wait();
                        space
                            .add_folder(&spec, &Provenance::person("Tester"))
                            .unwrap();
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
            let space = Space::open(&path).unwrap();
            let named = space
                .projects()
                .unwrap()
                .into_iter()
                .filter(|p| p.node.title == name)
                .count();
            assert_eq!(named, 1, "{name}");
        }
    }
}
