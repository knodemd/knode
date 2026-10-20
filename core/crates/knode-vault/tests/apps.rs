//! Importing notes from other apps, from synthetic exports and databases written here.

mod common;

use std::io::Write;
use std::path::Path;

use common::put;
use knode_format::{Node, NodeId, Provenance, Space, Timestamp, link_kinds};
use knode_vault::apps::{ImportError, NoteApp, NotesReport, detect, import_notes, keys};

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn space(dir: &Path) -> Space {
    Space::create(dir.join("Notes.knode"), "Notes").unwrap()
}

fn import(space: &mut Space, app: NoteApp, path: &Path) -> NotesReport {
    import_notes(space, app, path, &me()).unwrap()
}

/// The node imported for a note, by its id in the app.
fn node(space: &Space, source_id: &str) -> Node {
    let found = space.find_by_property(keys::ID, source_id).unwrap();
    assert_eq!(found.len(), 1, "one node for {source_id}");
    space.get_node(&found[0].id).unwrap().unwrap()
}

fn prop(space: &Space, id: &NodeId, key: &str) -> Option<String> {
    space.property(id, key).unwrap()
}

fn parents(space: &Space, id: &NodeId) -> Vec<String> {
    space
        .links_from(id)
        .unwrap()
        .into_iter()
        .filter(|l| l.kind == link_kinds::PART_OF)
        .map(|l| l.to_title)
        .collect()
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n synthetic image";

fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut out);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }
    out.into_inner()
}

// Apple Notes ------------------------------------------------------------------------------

/// A tiny protobuf writer.
#[derive(Default)]
struct Proto(Vec<u8>);

impl Proto {
    fn varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    fn int(mut self, field: u32, v: u64) -> Self {
        Self::varint(u64::from(field) << 3, &mut self.0);
        Self::varint(v, &mut self.0);
        self
    }
    fn bytes(mut self, field: u32, b: &[u8]) -> Self {
        Self::varint((u64::from(field) << 3) | 2, &mut self.0);
        Self::varint(b.len() as u64, &mut self.0);
        self.0.extend_from_slice(b);
        self
    }
    fn msg(self, field: u32, m: Proto) -> Self {
        self.bytes(field, &m.0)
    }
}

/// A run of a note: its text, and how it looks.
struct R<'a> {
    text: &'a str,
    style: Option<u64>,
    weight: u64,
    checked: Option<bool>,
    attachment: Option<(&'a str, &'a str)>,
    link: Option<&'a str>,
}

fn r(text: &str) -> R<'_> {
    R {
        text,
        style: None,
        weight: 0,
        checked: None,
        attachment: None,
        link: None,
    }
}

/// A note body as Notes stores it: gzipped `NoteStoreProto`.
fn note_data(runs: &[R<'_>]) -> Vec<u8> {
    let text: String = runs.iter().map(|r| r.text).collect();
    let mut note = Proto::default().bytes(2, text.as_bytes());
    for run in runs {
        let mut m = Proto::default().int(1, run.text.encode_utf16().count() as u64);
        if run.style.is_some() || run.checked.is_some() {
            let mut p = Proto::default();
            if let Some(style) = run.style {
                p = p.int(1, style);
            }
            if let Some(done) = run.checked {
                p = p.msg(
                    5,
                    Proto::default().bytes(1, b"uuid").int(2, u64::from(done)),
                );
            }
            m = m.msg(2, p);
        }
        if run.weight > 0 {
            m = m.int(5, run.weight);
        }
        if let Some(link) = run.link {
            m = m.bytes(9, link.as_bytes());
        }
        if let Some((id, uti)) = run.attachment {
            m = m.msg(
                12,
                Proto::default()
                    .bytes(1, id.as_bytes())
                    .bytes(2, uti.as_bytes()),
            );
        }
        note = note.msg(5, m);
    }
    let store = Proto::default().msg(2, Proto::default().int(2, 1).msg(3, note));
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&store.0).unwrap();
    gz.finish().unwrap()
}

/// Seconds since 2001-01-01 of 2026-10-20T09:50:00Z.
const CORE_DATA_TIME: f64 = 1_792_489_800.0 - 978_307_200.0;

fn apple_notes_container(dir: &Path) -> std::path::PathBuf {
    let container = dir.join("group.com.apple.notes");
    std::fs::create_dir_all(&container).unwrap();
    let conn = rusqlite::Connection::open(container.join("NoteStore.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE ZICCLOUDSYNCINGOBJECT (Z_PK INTEGER PRIMARY KEY, ZIDENTIFIER TEXT,
            ZTITLE1 TEXT, ZCREATIONDATE1 REAL, ZCREATIONDATE3 REAL, ZMODIFICATIONDATE1 REAL,
            ZFOLDER INTEGER, ZMARKEDFORDELETION INTEGER, ZISPASSWORDPROTECTED INTEGER,
            ZTITLE2 TEXT, ZPARENT INTEGER, ZFOLDERTYPE INTEGER, ZTYPEUTI TEXT, ZALTTEXT TEXT,
            ZTOKENCONTENTIDENTIFIER TEXT, ZURLSTRING TEXT, ZTITLE TEXT, ZMEDIA INTEGER,
            ZFILENAME TEXT, ZNOTE INTEGER, ZNOTEDATA INTEGER);
         CREATE TABLE ZICNOTEDATA (Z_PK INTEGER PRIMARY KEY, ZNOTE INTEGER, ZDATA BLOB);
         INSERT INTO ZICCLOUDSYNCINGOBJECT (Z_PK, ZIDENTIFIER, ZTITLE2, ZPARENT, ZFOLDERTYPE) VALUES
            (1, 'F-WORK', 'Work', NULL, 0), (2, 'F-PROJ', 'Projects', 1, 0),
            (3, 'F-TRASH', 'Recently Deleted', NULL, 1);
         INSERT INTO ZICCLOUDSYNCINGOBJECT (Z_PK, ZIDENTIFIER, ZTYPEUTI, ZALTTEXT,
            ZTOKENCONTENTIDENTIFIER, ZURLSTRING, ZTITLE, ZMEDIA, ZNOTE) VALUES
            (20, 'A-TAG', 'com.apple.notes.inlinetextattachment.hashtag', '#Garden', NULL, NULL, NULL, NULL, 10),
            (21, 'A-LINK', 'com.apple.notes.inlinetextattachment.link', 'Ideas',
                'applenotes:note/n-ideas?ownerIdentifier=x', NULL, NULL, NULL, 10),
            (22, 'A-IMG', 'public.png', NULL, NULL, NULL, NULL, 30, 10),
            (23, 'A-TABLE', 'com.apple.notes.table', NULL, NULL, NULL, NULL, NULL, 10),
            (24, 'A-URL', 'public.url', NULL, NULL, 'https://example.com/seeds', 'Seeds', NULL, 10);
         INSERT INTO ZICCLOUDSYNCINGOBJECT (Z_PK, ZIDENTIFIER, ZFILENAME) VALUES
            (30, 'MEDIA-1', 'beds.png');",
    )
    .unwrap();
    let add = |pk: i64, id: &str, title: &str, folder: i64, flags: (i64, i64), data: Vec<u8>| {
        conn.execute(
            "INSERT INTO ZICCLOUDSYNCINGOBJECT (Z_PK, ZIDENTIFIER, ZTITLE1, ZCREATIONDATE3,
                ZMODIFICATIONDATE1, ZFOLDER, ZMARKEDFORDELETION, ZISPASSWORDPROTECTED, ZNOTEDATA)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?1)",
            rusqlite::params![
                pk,
                id,
                title,
                CORE_DATA_TIME,
                CORE_DATA_TIME + 60.0,
                folder,
                flags.0,
                flags.1
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ZICNOTEDATA (Z_PK, ZNOTE, ZDATA) VALUES (?1, ?1, ?2)",
            rusqlite::params![pk, data],
        )
        .unwrap();
    };
    let obj = "\u{fffc}";
    add(
        10,
        "N-PLAN",
        "Garden plan",
        2,
        (0, 0),
        note_data(&[
            R {
                style: Some(0),
                ..r("Garden plan\n")
            },
            R {
                style: Some(1),
                ..r("Beds\n")
            },
            r("Water the "),
            R {
                weight: 1,
                ..r("tomatoes")
            },
            r(" daily, 3 * 2 times. "),
            R {
                attachment: Some(("A-TAG", "com.apple.notes.inlinetextattachment.hashtag")),
                ..r(obj)
            },
            r("\n"),
            R {
                style: Some(103),
                checked: Some(true),
                ..r("Buy seeds\n")
            },
            R {
                style: Some(103),
                checked: Some(false),
                ..r("Plant 🌱\n")
            },
            R {
                style: Some(100),
                ..r("Compost\n")
            },
            r("See "),
            R {
                attachment: Some(("A-LINK", "com.apple.notes.inlinetextattachment.link")),
                ..r(obj)
            },
            r(" and "),
            R {
                link: Some("https://example.com/soil"),
                ..r("soil")
            },
            r("\n"),
            R {
                attachment: Some(("A-IMG", "public.png")),
                ..r(obj)
            },
            r("\n"),
            R {
                attachment: Some(("A-TABLE", "com.apple.notes.table")),
                ..r(obj)
            },
            r("\n"),
            R {
                attachment: Some(("A-URL", "public.url")),
                ..r(obj)
            },
            r("\n"),
            R {
                style: Some(4),
                ..r("let x = 1;\n")
            },
        ]),
    );
    add(
        11,
        "N-IDEAS",
        "Ideas",
        1,
        (0, 0),
        note_data(&[r("Ideas\nA greenhouse.")]),
    );
    add(12, "N-LOCKED", "Secret", 1, (0, 1), b"encrypted".to_vec());
    add(
        13,
        "N-TRASHED",
        "Old",
        3,
        (0, 0),
        note_data(&[r("Old\nGone.")]),
    );
    add(
        14,
        "N-DELETED",
        "Deleted",
        1,
        (1, 0),
        note_data(&[r("Deleted")]),
    );
    put(
        &container,
        "Accounts/ACCOUNT-1/Media/MEDIA-1/1_ABC/beds.png",
        PNG,
    );
    container
}

#[test]
fn apple_notes_database() {
    let tmp = tempfile::tempdir().unwrap();
    let container = apple_notes_container(tmp.path());
    assert_eq!(detect(&container), Some(NoteApp::AppleNotes));
    let mut space = space(tmp.path());
    let report = import(&mut space, NoteApp::AppleNotes, &container);
    assert_eq!(report.created.len(), 2, "{report:?}");
    assert_eq!(report.skipped.len(), 1, "the locked note: {report:?}");
    assert!(report.skipped[0].reason.contains("locked"));
    assert_eq!(report.attached, 1);
    assert!(report.limits.iter().any(|l| l.contains("tables")));

    let plan = node(&space, "N-PLAN");
    assert_eq!(plan.title, "Garden plan");
    assert_eq!(plan.tags, ["garden"]);
    assert_eq!(plan.created_at, Timestamp(1_792_489_800_000));
    assert_eq!(plan.updated_at, Timestamp(1_792_489_860_000));
    assert_eq!(
        plan.id.created_at(),
        plan.created_at,
        "the id is of its time"
    );
    assert_eq!(
        prop(&space, &plan.id, keys::FOLDER).as_deref(),
        Some("Work/Projects")
    );
    assert_eq!(
        prop(&space, &plan.id, keys::APP).as_deref(),
        Some("apple-notes")
    );
    let expected = "## Beds\n\n\
        Water the **tomatoes** daily, 3 \\* 2 times. #Garden\n\n\
        - [x] Buy seeds\n\
        - [ ] Plant 🌱\n\
        - Compost\n\n\
        See [[Ideas]] and [soil](<https://example.com/soil>)\n\n\
        ![[beds.png]]\n\n\
        *(A table from Apple Notes: tables are not imported yet.)*\n\n\
        [Seeds](<https://example.com/seeds>)\n\n\
        ```\nlet x = 1;\n```\n";
    assert_eq!(plan.body, expected);
    let image = space.attachment(&plan.id, "beds.png").unwrap().unwrap();
    assert_eq!(space.read_blob(&image.hash).unwrap().unwrap(), PNG);

    let ideas = node(&space, "N-IDEAS");
    assert_eq!(ideas.body, "A greenhouse.\n");
    assert_eq!(
        prop(&space, &ideas.id, keys::FOLDER).as_deref(),
        Some("Work")
    );
    assert!(
        space
            .find_by_property(keys::ID, "N-TRASHED")
            .unwrap()
            .is_empty()
    );
    assert!(
        space
            .find_by_property(keys::ID, "N-DELETED")
            .unwrap()
            .is_empty()
    );

    // Again: nothing changes.
    let again = import(&mut space, NoteApp::AppleNotes, &container);
    assert_eq!((again.created.len(), again.updated.len()), (0, 0));
    assert_eq!(again.unchanged, 2);

    // A note changed in Notes is updated.
    let conn = rusqlite::Connection::open(container.join("NoteStore.sqlite")).unwrap();
    conn.execute(
        "UPDATE ZICNOTEDATA SET ZDATA = ?1 WHERE ZNOTE = 11",
        [note_data(&[r("Ideas\nA greenhouse and a pond.")])],
    )
    .unwrap();
    let changed = import(&mut space, NoteApp::AppleNotes, &container);
    assert_eq!(changed.updated.len(), 1);
    assert_eq!(changed.unchanged, 1);
    assert_eq!(node(&space, "N-IDEAS").body, "A greenhouse and a pond.\n");
    assert_eq!(node(&space, "N-IDEAS").id, ideas.id);
}

#[cfg(unix)]
#[test]
fn apple_notes_without_access_says_how_to_give_it() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let container = apple_notes_container(tmp.path());
    std::fs::set_permissions(&container, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut space = space(tmp.path());
    let result = import_notes(&mut space, NoteApp::AppleNotes, &container, &me());
    std::fs::set_permissions(&container, std::fs::Permissions::from_mode(0o755)).unwrap();
    let Err(error) = result else {
        // Running as root: permissions do not apply.
        return;
    };
    assert!(matches!(error, ImportError::NoAccess { .. }), "{error:?}");
    assert!(error.to_string().contains("Full Disk Access"));
}

#[test]
fn apple_notes_exported_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let export = tmp.path().join("Export");
    put(
        &export,
        "Work/Plan.md",
        "# Plan\n\nSee [ideas](Ideas.md) and ![](Attachments/beds.png).\n",
    );
    put(&export, "Work/Ideas.md", "A greenhouse.\n");
    put(&export, "Work/Attachments/beds.png", PNG);
    let mut space = space(tmp.path());
    let report = import(&mut space, NoteApp::AppleNotes, &export);
    assert_eq!(report.created.len(), 2, "{report:?}");
    let plan = node(&space, "Work/Plan.md");
    assert_eq!(plan.title, "Plan");
    assert_eq!(plan.body, "See [[Ideas|ideas]] and ![[beds.png]].\n");
    assert_eq!(
        prop(&space, &plan.id, keys::FOLDER).as_deref(),
        Some("Work")
    );
    assert!(space.attachment(&plan.id, "beds.png").unwrap().is_some());
}

// Bear --------------------------------------------------------------------------------------

#[test]
fn bear_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let info = |id: &str, trashed: u8| {
        format!(
            r#"{{"version": 2, "type": "net.daringfireball.markdown",
                "net.shinyfrog.bear": {{"uniqueIdentifier": "{id}", "trashed": {trashed},
                "creationDate": "2026-10-20T09:50:00Z", "modificationDate": "2026-10-21T09:50:00Z"}}}}"#
        )
    };
    let backup = tmp.path().join("Notes.bear2bk");
    let plan_info = info("B-PLAN", 0);
    let ideas_info = info("B-IDEAS", 0);
    let old_info = info("B-OLD", 1);
    std::fs::write(
        &backup,
        zip(&[
            (
                "Bear Notes/Plan.textbundle/text.md",
                "# Plan\nWater the beds. #garden #to do list# #home/yard\n\nSee [[Ideas]].\n\n![](assets/beds%20one.png)\n".as_bytes(),
            ),
            ("Bear Notes/Plan.textbundle/info.json", plan_info.as_bytes()),
            ("Bear Notes/Plan.textbundle/assets/beds one.png", PNG),
            ("Bear Notes/Ideas.textbundle/text.md", b"# Ideas\nA greenhouse.\n"),
            ("Bear Notes/Ideas.textbundle/info.json", ideas_info.as_bytes()),
            ("Bear Notes/Old.textbundle/text.md", b"# Old\nGone.\n"),
            ("Bear Notes/Old.textbundle/info.json", old_info.as_bytes()),
        ]),
    )
    .unwrap();
    assert_eq!(detect(&backup), Some(NoteApp::Bear));
    let mut space = space(tmp.path());
    let report = import(&mut space, NoteApp::Bear, &backup);
    assert_eq!(report.created.len(), 2, "{report:?}");
    let plan = node(&space, "B-PLAN");
    assert_eq!(plan.title, "Plan");
    assert_eq!(
        plan.body,
        "Water the beds. #garden #to do list# #home/yard\n\nSee [[Ideas]].\n\n![[beds one.png]]\n"
    );
    assert_eq!(plan.tags, ["garden", "home/yard", "to-do-list"]);
    assert_eq!(plan.created_at, Timestamp(1_792_489_800_000));
    assert_eq!(plan.updated_at, Timestamp(1_792_576_200_000));
    assert!(
        space
            .attachment(&plan.id, "beds one.png")
            .unwrap()
            .is_some()
    );
    let ideas = node(&space, "B-IDEAS");
    assert_eq!(
        space.backlinks(&ideas.id).unwrap().len(),
        1,
        "[[Ideas]] links"
    );

    let again = import(&mut space, NoteApp::Bear, &backup);
    assert_eq!(again.unchanged, 2);
    assert!(again.created.is_empty() && again.updated.is_empty());
}

#[test]
fn bear_database() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("Application Data");
    std::fs::create_dir_all(&data).unwrap();
    let conn = rusqlite::Connection::open(data.join("database.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE ZSFNOTE (Z_PK INTEGER PRIMARY KEY, ZUNIQUEIDENTIFIER TEXT, ZTITLE TEXT,
            ZTEXT TEXT, ZCREATIONDATE REAL, ZMODIFICATIONDATE REAL, ZTRASHED INTEGER,
            ZARCHIVED INTEGER, ZENCRYPTED INTEGER, ZPERMANENTLYDELETED INTEGER);
         CREATE TABLE ZSFNOTEFILE (Z_PK INTEGER PRIMARY KEY, ZNOTE INTEGER,
            ZUNIQUEIDENTIFIER TEXT, ZFILENAME TEXT);",
    )
    .unwrap();
    let add = |pk: i64, id: &str, title: &str, text: &str, flags: [i64; 4]| {
        conn.execute(
            "INSERT INTO ZSFNOTE VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                pk,
                id,
                title,
                text,
                CORE_DATA_TIME,
                flags[0],
                flags[1],
                flags[2],
                flags[3]
            ],
        )
        .unwrap();
    };
    add(
        1,
        "D-PLAN",
        "Plan",
        "# Plan\nBeds: ![](beds.png) and [image:F-2/old.png]\n#garden",
        [0, 1, 0, 0],
    );
    add(2, "D-SECRET", "Secret", "# Secret\n...", [0, 0, 1, 0]);
    add(3, "D-TRASH", "Trash", "# Trash\n", [1, 0, 0, 0]);
    conn.execute_batch(
        "INSERT INTO ZSFNOTEFILE VALUES (1, 1, 'F-1', 'beds.png'), (2, 1, 'F-2', 'old.png');",
    )
    .unwrap();
    put(&data, "Local Files/Note Images/F-1/beds.png", PNG);
    put(&data, "Local Files/Note Images/F-2/old.png", b"GIF89a old");
    assert_eq!(detect(&data), Some(NoteApp::Bear));

    let mut space = space(tmp.path());
    let report = import(&mut space, NoteApp::Bear, &data);
    assert_eq!(report.created.len(), 1, "{report:?}");
    assert_eq!(report.skipped.len(), 1, "the encrypted note");
    assert_eq!(report.attached, 2);
    let plan = node(&space, "D-PLAN");
    assert_eq!(plan.body, "Beds: ![[beds.png]] and ![[old.png]]\n#garden\n");
    assert_eq!(plan.tags, ["garden"]);
    assert_eq!(
        prop(&space, &plan.id, keys::ARCHIVED).as_deref(),
        Some("true")
    );
}

// Notion ------------------------------------------------------------------------------------

const ROOT: &str = "0123456789abcdef0123456789abcdef";
const CHILD: &str = "11111111111111111111111111111111";
const TASKS: &str = "22222222222222222222222222222222";
const ROW: &str = "33333333333333333333333333333333";

#[test]
fn notion_export() {
    let tmp = tempfile::tempdir().unwrap();
    let root_md = format!(
        "# Garden\n\nSee [Soil notes](Garden%20{ROOT}/Soil%20{CHILD}.md), the \
         [tasks](Garden%20{ROOT}/Tasks%20{TASKS}.csv) and [web](https://www.notion.so/Soil-{CHILD}).\n\n\
         ![beds.png](Garden%20{ROOT}/beds.png)\n"
    );
    let child_md = format!("# Soil\n\nBack to [Garden](../Garden%20{ROOT}.md).\n");
    let tasks_csv = "Name,Status,Tags\nBuy seeds,Done,\"garden, spring\"\nDig,\"To do\",\n";
    let row_md = "# Buy seeds\n\nStatus: Done\nTags: garden, spring\nCreated: October 20, 2026 9:50 AM\n\nGet them early.\n";
    let inner = zip(&[
        (&format!("Garden {ROOT}.md"), root_md.as_bytes()),
        (
            &format!("Garden {ROOT}/Soil {CHILD}.md"),
            child_md.as_bytes(),
        ),
        (&format!("Garden {ROOT}/beds.png"), PNG),
        (
            &format!("Garden {ROOT}/Tasks {TASKS}.csv"),
            b"Name\nBuy seeds\n",
        ),
        (
            &format!("Garden {ROOT}/Tasks {TASKS}_all.csv"),
            tasks_csv.as_bytes(),
        ),
        (
            &format!("Garden {ROOT}/Tasks {TASKS}/Buy seeds {ROW}.md"),
            row_md.as_bytes(),
        ),
    ]);
    // Notion splits large exports into parts: a zip of zips.
    let export = tmp.path().join("Export.zip");
    std::fs::write(&export, zip(&[("Export-Part-1.zip", &inner)])).unwrap();
    assert_eq!(detect(&export), Some(NoteApp::Notion));

    let mut space = space(tmp.path());
    let report = import(&mut space, NoteApp::Notion, &export);
    assert_eq!(report.created.len(), 4, "{report:?}");
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(
        report.links, 3,
        "Soil, Tasks and the row are part of their parents"
    );

    let garden = node(&space, ROOT);
    assert_eq!(
        garden.body,
        "See [[Soil|Soil notes]], the [[Tasks|tasks]] and [[Soil|web]].\n\n![[beds.png]]\n"
    );
    assert!(space.attachment(&garden.id, "beds.png").unwrap().is_some());
    let soil = node(&space, CHILD);
    assert_eq!(soil.body, "Back to [[Garden]].\n");
    assert_eq!(
        prop(&space, &soil.id, keys::FOLDER).as_deref(),
        Some("Garden")
    );
    assert_eq!(parents(&space, &soil.id), ["Garden"]);

    let tasks = node(&space, TASKS);
    assert_eq!(
        tasks.body,
        "| Name | Status | Tags |\n| --- | --- | --- |\n| [[Buy seeds]] | Done | garden, spring |\n| Dig | To do |  |\n"
    );
    let row = node(&space, ROW);
    assert_eq!(row.body, "Get them early.\n");
    assert_eq!(row.tags, ["garden", "spring"]);
    assert_eq!(row.created_at, Timestamp(1_792_489_800_000));
    assert_eq!(
        prop(&space, &row.id, "notion.status").as_deref(),
        Some("Done")
    );
    assert_eq!(
        prop(&space, &row.id, keys::FOLDER).as_deref(),
        Some("Garden/Tasks")
    );
    assert_eq!(parents(&space, &row.id), ["Tasks"]);

    let again = import(&mut space, NoteApp::Notion, &export);
    assert_eq!(again.unchanged, 4, "{again:?}");
}

// Evernote ----------------------------------------------------------------------------------

fn enex(notes: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE en-export SYSTEM \"http://xml.evernote.com/pub/evernote-export4.dtd\">\n\
         <en-export export-date=\"20261020T000000Z\" application=\"Evernote\" version=\"10.0\">\n{notes}</en-export>\n"
    )
}

#[test]
fn evernote_enex() {
    use base64::Engine;
    let tmp = tempfile::tempdir().unwrap();
    let hash = format!("{:x}", md5::compute(PNG));
    let data = base64::engine::general_purpose::STANDARD.encode(PNG);
    let work = enex(&format!(
        "<note><title>Garden plan</title><content><![CDATA[<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <!DOCTYPE en-note SYSTEM \"http://xml.evernote.com/pub/enml2.dtd\">\
         <en-note><div>Water the <b>tomatoes</b>.</div><div><en-media hash=\"{hash}\" type=\"image/png\"/></div>\
         <div>See <a href=\"evernote:///view/1/s1/abc/abc/\">Ideas</a>.</div></en-note>]]></content>\
         <created>20261020T095000Z</created><updated>20261021T095000Z</updated>\
         <tag>Garden</tag><tag>Spring plans</tag>\
         <note-attributes><source-url>https://example.com/plan</source-url></note-attributes>\
         <resource><data encoding=\"base64\">{data}</data><mime>image/png</mime>\
         <resource-attributes><file-name>beds.png</file-name></resource-attributes></resource></note>\n\
         <note><title>Ideas</title><content><![CDATA[<en-note><div>A greenhouse.</div></en-note>]]></content>\
         <created>20261020T100000Z</created></note>\n"
    ));
    let home = enex(
        "<note><title>Chores</title><content><![CDATA[<en-note><div><en-todo checked=\"true\"/>Dishes</div></en-note>]]></content>\
         <created>20261020T110000Z</created></note>\n",
    );
    let dir = tmp.path().join("Evernote");
    put(&dir, "Work.enex", work);
    put(&dir, "Home.enex", home);
    assert_eq!(detect(&dir), Some(NoteApp::Evernote));
    assert_eq!(detect(dir.join("Work.enex")), Some(NoteApp::Evernote));

    let mut space = space(tmp.path());
    let report = import(&mut space, NoteApp::Evernote, &dir);
    assert_eq!(report.created.len(), 3, "{report:?}");
    assert_eq!(report.attached, 1);
    let plan = node(&space, "20261020T095000Z:Garden plan");
    assert_eq!(
        plan.body,
        "Water the **tomatoes**.\n\n![[beds.png]]\n\nSee [[Ideas]].\n"
    );
    assert_eq!(plan.tags, ["garden", "spring-plans"]);
    assert_eq!(plan.created_at, Timestamp(1_792_489_800_000));
    assert_eq!(plan.updated_at, Timestamp(1_792_576_200_000));
    assert_eq!(
        prop(&space, &plan.id, keys::FOLDER).as_deref(),
        Some("Work")
    );
    assert_eq!(
        prop(&space, &plan.id, keys::URL).as_deref(),
        Some("https://example.com/plan")
    );
    let image = space.attachment(&plan.id, "beds.png").unwrap().unwrap();
    assert_eq!(space.read_blob(&image.hash).unwrap().unwrap(), PNG);
    let chores = node(&space, "20261020T110000Z:Chores");
    assert_eq!(chores.body, "- [x] Dishes\n");
    assert_eq!(
        prop(&space, &chores.id, keys::FOLDER).as_deref(),
        Some("Home")
    );

    let again = import(&mut space, NoteApp::Evernote, &dir);
    assert_eq!(again.unchanged, 3);
    assert!(again.created.is_empty());

    // A note deleted in Knode comes back with its id on the next import.
    space.delete_node(&chores.id, &me()).unwrap();
    let back = import(&mut space, NoteApp::Evernote, &dir);
    assert_eq!(back.created.len(), 1, "{back:?}");
    assert_eq!(node(&space, "20261020T110000Z:Chores").id, chores.id);
}

#[test]
fn unknown_paths() {
    let tmp = tempfile::tempdir().unwrap();
    put(tmp.path(), "notes/Plan.md", "# Plan\n");
    assert_eq!(detect(tmp.path().join("notes")), None);
    let mut space = space(tmp.path());
    let missing = import_notes(
        &mut space,
        NoteApp::Evernote,
        tmp.path().join("nope.enex"),
        &me(),
    );
    assert!(matches!(missing, Err(ImportError::NotFound(_))));
    let wrong = import_notes(&mut space, NoteApp::Notion, tmp.path().join("notes"), &me());
    assert!(matches!(wrong, Err(ImportError::NotRecognized { .. })));
}
