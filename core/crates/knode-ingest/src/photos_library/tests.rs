use std::fs;
use std::path::Path;

use knode_format::{NewNode, Space};

use super::*;

const LIB: &str = "01JLIBRARYAAAAAAAAAAAAAAAA";
const OTHER: &str = "01JLIBRARYBBBBBBBBBBBBBBBB";

fn space(dir: &Path) -> Space {
    Space::create(dir.join("Library.knode"), "Library").unwrap()
}

fn by() -> Provenance {
    crate::sources::provenance(SOURCE_ID)
}

fn photo(dir: &Path, id: &str, name: &str, bytes: &[u8]) -> LibraryAsset {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    LibraryAsset {
        id: id.into(),
        kind: AssetKind::Photo,
        path,
        title: String::new(),
        created_at: Some(1_795_343_400_000),
        duration_ms: None,
        metadata: PhotoMetadata {
            width: Some(4),
            height: Some(3),
            ..Default::default()
        },
        original: None,
        content: None,
    }
}

fn video(dir: &Path, id: &str, name: &str, content: Option<&str>) -> LibraryAsset {
    let path = dir.join(name);
    fs::write(&path, b"aac").unwrap();
    LibraryAsset {
        id: id.into(),
        kind: AssetKind::Video,
        path,
        title: "Trip".into(),
        created_at: Some(1_795_343_400_000),
        duration_ms: Some(60_000),
        metadata: PhotoMetadata::default(),
        original: Some("Trip.mov".into()),
        content: content.map(str::to_string),
    }
}

fn state(space: &Space, node: &NodeId) -> Option<String> {
    space.property(node, keys::STATE).unwrap()
}

#[test]
fn adds_each_asset_once_by_identifier() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut ledger = Ledger::new(LIB);
    let a = photo(dir.path(), "AAA/L0/001", "IMG_1.HEIC", b"heic one");
    let first = add(&mut space, LIB, &a, &mut ledger, &by()).unwrap();
    assert!(first.created);
    let Added::Photo(p) = &first.node else {
        panic!("a photo")
    };
    assert_eq!(p.title, "IMG_1");
    assert_eq!(p.metadata.width, Some(4));
    let props = space.properties(&p.id).unwrap();
    assert_eq!(props.get(keys::ID).map(String::as_str), Some("AAA/L0/001"));
    assert_eq!(props.get(keys::LIBRARY).map(String::as_str), Some(LIB));
    assert_eq!(props.get(keys::STATE).map(String::as_str), Some("present"));
    assert_eq!(ledger.held.get("AAA/L0/001"), Some(&p.id));

    // The same asset again, even exported to another file, is the same node.
    let mut again = photo(dir.path(), "AAA/L0/001", "IMG_1 copy.HEIC", b"other bytes");
    again.title = "Renamed".into();
    let second = add(&mut space, LIB, &again, &mut ledger, &by()).unwrap();
    assert!(!second.created);
    assert_eq!(second.node.id(), p.id);
    assert_eq!(
        known(
            &space,
            LIB,
            &["AAA/L0/001".into(), "BBB/L0/001".into()],
            &ledger
        )
        .unwrap(),
        vec!["AAA/L0/001".to_string()]
    );
    // Identifiers mean something only in their library.
    assert!(
        known(&space, OTHER, &["AAA/L0/001".into()], &Ledger::new(OTHER))
            .unwrap()
            .is_empty()
    );

    // Another asset with the same content is the same photo, found by both identifiers.
    let dup = photo(dir.path(), "BBB/L0/001", "IMG_2.HEIC", b"heic one");
    let third = add(&mut space, LIB, &dup, &mut ledger, &by()).unwrap();
    assert!(!third.created);
    assert_eq!(third.node.id(), p.id);
    assert_eq!(find(&space, LIB, "BBB/L0/001").unwrap(), Some(p.id));
    assert_eq!(find(&space, LIB, "AAA/L0/001").unwrap(), Some(p.id));
    assert_eq!(photos::pending(&space).unwrap().len(), 1);
    let blank = photo(dir.path(), " ", "x.jpg", b"x");
    assert!(add(&mut space, LIB, &blank, &mut ledger, &by()).is_err());
}

#[test]
fn a_photo_someone_else_added_is_never_taken_over() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let ada = Provenance::person("Ada");
    let path = dir.path().join("mine.jpg");
    fs::write(&path, b"same bytes").unwrap();
    let mine = photos::create(
        &mut space,
        &NewPhoto {
            title: "Mine".into(),
            path,
            project: None,
            metadata: PhotoMetadata::default(),
        },
        &ada,
    )
    .unwrap();
    let mut ledger = Ledger::new(LIB);
    let asset = photo(dir.path(), "A", "IMG_9.HEIC", b"same bytes");
    let added = add(&mut space, LIB, &asset, &mut ledger, &by()).unwrap();
    assert!(added.created);
    assert_ne!(added.node.id(), mine.id);
    let props = space.properties(&mine.id).unwrap();
    assert!(
        props.keys().all(|k| !k.starts_with("photos_library.")),
        "{props:?}"
    );
    // Leaving the albums marks only the library's node.
    mark(&mut space, LIB, &[], &by()).unwrap();
    assert_eq!(state(&space, &mine.id), None);
    assert_eq!(state(&space, &added.node.id()).as_deref(), Some(REMOVED));
    assert_eq!(space.get_node(&mine.id).unwrap().unwrap().title, "Mine");
    // The file is stored once.
    assert_eq!(space.stored_blobs().unwrap().len(), 1);
}

#[test]
fn a_photo_whose_node_was_deleted_never_comes_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut ledger = Ledger::new(LIB);
    let asset = photo(dir.path(), "A", "a.jpg", b"a");
    let added = add(&mut space, LIB, &asset, &mut ledger, &by()).unwrap();
    space
        .delete_node(&added.node.id(), &Provenance::person("Ada"))
        .unwrap();
    // Added again before the ledger looked: still refused.
    let mut unseen = ledger.clone();
    assert!(add(&mut space, LIB, &asset, &mut unseen, &by()).is_err());
    assert!(unseen.deleted.contains("A"));
    assert_eq!(ledger.reconcile(&space).unwrap(), 1);
    assert!(ledger.deleted.contains("A"));
    assert_eq!(
        known(&space, LIB, &["A".into()], &ledger).unwrap(),
        vec!["A".to_string()]
    );
    assert!(add(&mut space, LIB, &asset, &mut ledger, &by()).is_err());
    assert!(space.find_by_property(keys::ID, "A").unwrap().is_empty());

    // A lost ledger learns the nodes of the library again.
    let other = add(
        &mut space,
        LIB,
        &photo(dir.path(), "B", "b.jpg", b"b"),
        &mut ledger,
        &by(),
    )
    .unwrap();
    let mut fresh = Ledger::new(LIB);
    fresh.reconcile(&space).unwrap();
    assert_eq!(fresh.held.get("B"), Some(&other.node.id()));
}

#[test]
fn failures_for_good_are_skipped() {
    let mut ledger = Ledger::new(LIB);
    ledger.fail("V", "the audio is larger than 100 MB");
    assert!(ledger.skips("V"));
    let dir = tempfile::tempdir().unwrap();
    let space = space(dir.path());
    assert_eq!(
        known(&space, LIB, &["V".into(), "W".into()], &ledger).unwrap(),
        vec!["V".to_string()]
    );
}

#[test]
fn videos_are_recordings_found_again_by_their_content() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut ledger = Ledger::new(LIB);
    let content = "sha256:00ff";
    let asset = video(dir.path(), "VID/L0/001", "Trip.m4a", Some(content));
    let added = add(&mut space, LIB, &asset, &mut ledger, &by()).unwrap();
    let Added::Recording(r) = &added.node else {
        panic!("a recording")
    };
    assert_eq!(r.duration_ms, 60_000);
    assert_eq!(
        r.key.as_deref(),
        Some(format!("photos-library:{LIB}/VID/L0/001").as_str())
    );
    assert_eq!(r.original.as_deref(), Some("Trip.mov"));
    assert_eq!(space.attachments(&r.id).unwrap().len(), 1);
    let body = space.get_node(&r.id).unwrap().unwrap().body;
    assert!(body.contains("kept only where it was"), "{body}");
    assert!(
        !add(&mut space, LIB, &asset, &mut ledger, &by())
            .unwrap()
            .created
    );

    // After a library rebuild the same video has a new identifier: it is the same recording.
    let rebuilt = video(dir.path(), "NEW/L0/001", "Trip 2.m4a", Some(content));
    let again = add(&mut space, LIB, &rebuilt, &mut ledger, &by()).unwrap();
    assert!(!again.created);
    assert_eq!(again.node.id(), r.id);
    assert_eq!(
        space
            .find_by_property(keys::CONTENT, content)
            .unwrap()
            .len(),
        1
    );
    let mark_new = mark(&mut space, LIB, &["NEW/L0/001".into()], &by()).unwrap();
    assert_eq!(mark_new, Marked::default());
}

#[test]
fn marks_only_the_nodes_of_the_reporting_library() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut ledger = Ledger::new(LIB);
    let mut other = Ledger::new(OTHER);
    let a = add(
        &mut space,
        LIB,
        &photo(dir.path(), "A", "a.jpg", b"a"),
        &mut ledger,
        &by(),
    )
    .unwrap();
    let b = add(
        &mut space,
        LIB,
        &photo(dir.path(), "B", "b.jpg", b"b"),
        &mut ledger,
        &by(),
    )
    .unwrap();
    let c = add(
        &mut space,
        OTHER,
        &photo(dir.path(), "C", "c.jpg", b"c"),
        &mut other,
        &by(),
    )
    .unwrap();
    let note = space
        .create_node(NewNode::note("Note", "text"), &Provenance::person("Ada"))
        .unwrap();
    let marked = mark(&mut space, LIB, &["A".into()], &by()).unwrap();
    assert_eq!(
        marked,
        Marked {
            removed: 1,
            restored: 0
        }
    );
    assert_eq!(state(&space, &b.node.id()).as_deref(), Some("removed"));
    // The body is never changed: the state is in the property.
    let body = space.get_node(&b.node.id()).unwrap().unwrap().body;
    assert!(!body.contains("No longer"), "{body}");
    assert_eq!(state(&space, &c.node.id()).as_deref(), Some("present"));
    assert_eq!(state(&space, &note.id), None);
    // Marking again changes nothing; the asset is still known, so it is never added again.
    assert_eq!(
        mark(&mut space, LIB, &["A".into()], &by()).unwrap(),
        Marked::default()
    );
    assert_eq!(
        known(&space, LIB, &["B".into()], &ledger).unwrap(),
        vec!["B".to_string()]
    );

    let marked = mark(&mut space, LIB, &["A".into(), "B".into()], &by()).unwrap();
    assert_eq!(
        marked,
        Marked {
            removed: 0,
            restored: 1
        }
    );
    assert_eq!(state(&space, &b.node.id()).as_deref(), Some("present"));
    assert!(space.get_node(&a.node.id()).unwrap().is_some());
}

#[test]
fn icloud_downloads_are_off_unless_chosen() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    assert!(!icloud_enabled(&space).unwrap());
    let ada = Provenance::person("Ada");
    assert!(set_icloud(&mut space, true, &ada).unwrap());
    assert!(icloud_enabled(&space).unwrap());
    set_icloud(&mut space, false, &ada).unwrap();
    assert_eq!(space.setting(ICLOUD_SETTING).unwrap(), None);
}

#[test]
fn alias_keys_are_property_keys() {
    let key = alias_key(&qualified(
        LIB,
        "9F983DBA-EC35-42B8-8773-B597CF782EDD/L0/001",
    ));
    assert!(key.starts_with(keys::ALIAS_PREFIX));
    assert!(key.len() <= 64);
    assert!(
        key.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
    );
}
