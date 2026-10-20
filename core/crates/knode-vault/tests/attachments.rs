//! Attachments and kinds of link the space defines, through an export and an import.

mod common;

use std::fs;

use common::{assert_same, put, tree};
use knode_format::{LinkKindSpec, NewNode, NodeId, Provenance, Space};
use knode_vault::{export, import};

fn me() -> Provenance {
    Provenance::person("Ada")
}

const BEDS: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
const OTHER_BEDS: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>";
const PHOTO: &[u8] = b"\x89PNG\r\n\x1a\n not really a photo";

/// A space with attachments (one used through `knode:blob/<hash>`, one with a space in its name,
/// one with the name of another node's attachment, one that cannot be a file) and a link of a
/// kind the space defines.
fn sample(dir: &std::path::Path) -> (Space, NodeId, NodeId) {
    let mut space = Space::create(dir.join("Garden.knode"), "Garden").unwrap();
    let me = me();
    let plan = space
        .create_node(NewNode::note("Plan", "Draft.\n"), &me)
        .unwrap();
    let photo = space.attach(&plan.id, "photo 1.png", PHOTO, &me).unwrap();
    space.attach(&plan.id, "beds.svg", BEDS, &me).unwrap();
    space.attach(&plan.id, ".hidden", &b"x"[..], &me).unwrap();
    space
        .update_node(
            &plan.id,
            knode_format::NodeUpdate {
                body: Some(format!(
                    "The beds: ![[beds.svg]]\n\n![A photo](knode:blob/{}) and `knode:blob/{}`.\n",
                    photo.hash, photo.hash
                )),
                ..Default::default()
            },
            &me,
        )
        .unwrap();
    let shed = space
        .create_node(NewNode::note("Shed", "Its beds: ![[beds.svg]]\n"), &me)
        .unwrap();
    space.attach(&shed.id, "beds.svg", OTHER_BEDS, &me).unwrap();
    space
        .set_link_kind(
            &LinkKindSpec::directed("depends-on", "depends on", "needed by")
                .with_description("The source needs the target first."),
            &me,
        )
        .unwrap();
    space.link(&plan.id, &shed.id, "depends-on", &me).unwrap();
    (space, plan.id, shed.id)
}

#[test]
fn attachments_and_link_kinds_survive_export_and_import() {
    let tmp = tempfile::tempdir().unwrap();
    let (original, plan, shed) = sample(tmp.path());
    let first = tmp.path().join("first");
    let report = export(&original, &first).unwrap();
    assert_eq!(report.files, 2);
    assert_eq!(report.attachments, 3);
    assert_eq!(report.left_out.len(), 1, "{report:?}");
    assert!(report.left_out[0].contains(".hidden"));

    let files = tree(&first);
    assert_eq!(files["beds.svg"], BEDS);
    assert_eq!(files["beds 2.svg"], OTHER_BEDS, "the other node's file");
    assert_eq!(files["photo 1.png"], PHOTO);
    let plan_md = String::from_utf8(files["Plan.md"].clone()).unwrap();
    assert!(
        plan_md.contains("![A photo](<photo 1.png>)"),
        "blob links become file names: {plan_md}"
    );
    assert!(plan_md.contains("`knode:blob/"), "code is left as written");
    assert!(plan_md.contains("depends-on: ["));
    let kinds = String::from_utf8(files[".knode/link-kinds.json"].clone()).unwrap();
    assert!(kinds.contains("\"needed by\""), "{kinds}");

    let mut copy = Space::create(tmp.path().join("Copy.knode"), "Copy").unwrap();
    let imported = import(&mut copy, &first, &me()).unwrap();
    assert!(imported.skipped.is_empty(), "{imported:?}");
    assert!(imported.unlinked.is_empty(), "{imported:?}");
    assert_eq!(imported.link_kinds, ["depends-on"]);
    assert_eq!(imported.links, 1);
    assert_eq!(imported.attached, ["beds 2.svg", "beds.svg", "photo 1.png"]);
    assert!(imported.attachments.is_empty(), "{imported:?}");

    // The same attachments under the same names, the same body, the same kind of link.
    for id in [plan, shed] {
        let names = |space: &Space| -> Vec<(String, String)> {
            space
                .attachments(&id)
                .unwrap()
                .into_iter()
                .filter(|a| a.name != ".hidden")
                .map(|a| (a.name, a.hash))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        assert_eq!(names(&copy), names(&original));
        assert_eq!(
            copy.get_node(&id).unwrap().unwrap().body,
            original.get_node(&id).unwrap().unwrap().body
        );
    }
    let kind = copy.link_kind("depends-on").unwrap().unwrap();
    assert_eq!(
        (kind.label.as_str(), kind.inverse_label.as_str()),
        ("depends on", "needed by")
    );
    assert_eq!(kind.description, "The source needs the target first.");
    let links = copy.links_from(&plan).unwrap();
    assert!(links.iter().any(|l| l.kind == "depends-on" && l.to == shed));

    let second = tmp.path().join("second");
    export(&copy, &second).unwrap();
    assert_same(&tree(&first), &tree(&second));

    // Importing again changes nothing.
    let again = import(&mut copy, &first, &me()).unwrap();
    assert_eq!(again.unchanged, 2, "{again:?}");
    assert!(again.link_kinds.is_empty());
    assert_eq!(copy.attachments(&plan).unwrap().len(), 2);
}

#[test]
fn export_keeps_attachment_files_in_step() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut space, plan, _) = sample(tmp.path());
    let dir = tmp.path().join("out");
    export(&space, &dir).unwrap();

    // A file Knode did not write is never overwritten.
    put(&dir, "notes.svg", "mine");
    space.attach(&plan, "notes.svg", BEDS, &me()).unwrap();
    let report = export(&space, &dir).unwrap();
    assert_eq!(report.written, ["notes 2.svg"]);
    assert_eq!(fs::read(dir.join("notes.svg")).unwrap(), b"mine");

    // Unchanged files are not written again; files of removed attachments go.
    space.detach(&plan, "notes.svg", &me()).unwrap();
    let report = export(&space, &dir).unwrap();
    assert!(report.written.is_empty(), "{report:?}");
    assert_eq!(report.removed, ["notes 2.svg"]);
    assert!(dir.join("notes.svg").exists());
}

#[test]
fn a_changed_file_replaces_its_attachment() {
    let tmp = tempfile::tempdir().unwrap();
    let (original, plan, _) = sample(tmp.path());
    let dir = tmp.path().join("out");
    export(&original, &dir).unwrap();
    let mut copy = Space::create(tmp.path().join("Copy.knode"), "Copy").unwrap();
    import(&mut copy, &dir, &me()).unwrap();

    let edited: &[u8] = b"\x89PNG\r\n\x1a\n a better photo";
    fs::write(dir.join("photo 1.png"), edited).unwrap();
    let report = import(&mut copy, &dir, &me()).unwrap();
    assert_eq!(report.updated, ["Plan.md"], "{report:?}");
    let photo = copy.attachment(&plan, "photo 1.png").unwrap().unwrap();
    assert_eq!(copy.read_blob(&photo.hash).unwrap().unwrap(), edited);
    let body = copy.get_node(&plan).unwrap().unwrap().body;
    assert!(
        body.contains(&format!("![A photo](knode:blob/{})", photo.hash)),
        "{body}"
    );
    assert_eq!(copy.attachments(&plan).unwrap().len(), 2);
}

#[test]
fn notes_from_other_apps_attach_the_files_they_embed() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    put(
        &vault,
        "Garden/Beds.md",
        "# Beds\n\n![[layout.png|300]] and [[Plan]]\n",
    );
    put(&vault, "Garden/layout.png", PHOTO);
    put(&vault, "Garden/unused.pdf", "%PDF");
    put(&vault, "elsewhere.png", BEDS);
    put(&vault, "Plan.md", "See ![[elsewhere.png]] from the root.\n");

    let mut space = Space::create(tmp.path().join("Vault.knode"), "Vault").unwrap();
    let report = import(&mut space, &vault, &me()).unwrap();
    assert_eq!(report.attached, ["Garden/layout.png", "elsewhere.png"]);
    assert_eq!(report.attachments, ["Garden/unused.pdf"]);

    let beds = &space.find_by_title("Beds").unwrap()[0];
    let attachments = space.attachments(&beds.id).unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].name, "layout.png");
    // The body is kept as written.
    assert_eq!(
        space.get_node(&beds.id).unwrap().unwrap().body,
        "# Beds\n\n![[layout.png|300]] and [[Plan]]\n"
    );
}

#[test]
fn links_of_unknown_kinds_are_kept_or_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("in");
    let id = |text: &str| NodeId::parse(text).unwrap();
    let a = id("01JB7Q9W8K3M2N4P5R6S7T8V9A");
    let b = id("01JB7Q9W8K3M2N4P5R6S7T8V9B");
    let missing = id("01JB7Q9W8K3M2N4P5R6S7T8V9C");
    let file = |id: &NodeId, links: &str| {
        format!(
            "---\nid: {id}\nkind: note\ncreated: 2026-10-20T09:30:00.000Z\nupdated: 2026-10-20T09:30:00.000Z\n{links}---\nText.\n"
        )
    };
    put(
        &dir,
        "A.md",
        file(
            &a,
            &format!("links:\n  blocks: [{b}, {missing}]\n  Not A Kind: [{b}]\n"),
        ),
    );
    put(&dir, "B.md", file(&b, ""));

    let mut space = Space::create(tmp.path().join("S.knode"), "S").unwrap();
    let report = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(report.link_kinds, ["blocks"]);
    assert_eq!(report.links, 1);
    let kind = space.link_kind("blocks").unwrap().unwrap();
    assert_eq!(kind.label, "blocks");
    assert_eq!(report.unlinked.len(), 2, "{report:?}");
    assert!(report.unlinked.iter().all(|s| s.path == "A.md"));
    assert!(
        report
            .unlinked
            .iter()
            .any(|s| s.reason.contains(&missing.to_string()))
    );
    assert!(
        report
            .unlinked
            .iter()
            .any(|s| s.reason.contains("Not A Kind"))
    );
}
