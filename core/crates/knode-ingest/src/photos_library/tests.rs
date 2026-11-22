use std::fs;
use std::path::Path;

use knode_format::{MAX_ATTACHMENT_SIZE, Space};

use super::*;

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
        original_path: None,
    }
}

#[test]
fn adds_each_asset_once_by_identifier() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let a = photo(dir.path(), "AAA/L0/001", "IMG_1.HEIC", b"heic one");
    let first = add(&mut space, &a, &by()).unwrap();
    assert!(first.created);
    let Added::Photo(p) = &first.node else {
        panic!("a photo")
    };
    assert_eq!(p.title, "IMG_1");
    assert_eq!(p.metadata.width, Some(4));
    let props = space.properties(&p.id).unwrap();
    assert_eq!(props.get(keys::ID).map(String::as_str), Some("AAA/L0/001"));
    assert_eq!(props.get(keys::STATE).map(String::as_str), Some("present"));

    // The same asset again, even exported to another file, is the same node.
    let mut again = photo(dir.path(), "AAA/L0/001", "IMG_1 copy.HEIC", b"other bytes");
    again.title = "Renamed".into();
    let second = add(&mut space, &again, &by()).unwrap();
    assert!(!second.created);
    assert_eq!(second.node.id(), p.id);
    assert_eq!(
        known(&space, &["AAA/L0/001".into(), "BBB/L0/001".into()]).unwrap(),
        vec!["AAA/L0/001".to_string()]
    );

    // Another asset with the same content is the same photo, found by both identifiers.
    let dup = photo(dir.path(), "BBB/L0/001", "IMG_2.HEIC", b"heic one");
    let third = add(&mut space, &dup, &by()).unwrap();
    assert!(!third.created);
    assert_eq!(third.node.id(), p.id);
    assert_eq!(find(&space, "BBB/L0/001").unwrap(), Some(p.id));
    assert_eq!(find(&space, "AAA/L0/001").unwrap(), Some(p.id));
    assert_eq!(photos::pending(&space).unwrap().len(), 1);
    assert!(add(&mut space, &photo(dir.path(), " ", "x.jpg", b"x"), &by()).is_err());
}

#[test]
fn videos_are_recordings_with_their_large_original_only_when_kept() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let audio = dir.path().join("Trip.m4a");
    fs::write(&audio, b"aac").unwrap();
    let original = dir.path().join("Trip.mov");
    fs::File::create(&original)
        .unwrap()
        .set_len(MAX_ATTACHMENT_SIZE + 1)
        .unwrap();
    let asset = LibraryAsset {
        id: "VID/L0/001".into(),
        kind: AssetKind::Video,
        path: audio.clone(),
        title: "Trip".into(),
        created_at: Some(1_795_343_400_000),
        duration_ms: Some(60_000),
        metadata: PhotoMetadata::default(),
        original: Some("Trip.mov".into()),
        original_path: Some(original.clone()),
    };
    let added = add(&mut space, &asset, &by()).unwrap();
    let Added::Recording(r) = &added.node else {
        panic!("a recording")
    };
    assert_eq!(r.duration_ms, 60_000);
    assert_eq!(r.key.as_deref(), Some("photos-library:VID/L0/001"));
    assert_eq!(r.original.as_deref(), Some("Trip.mov"));
    assert_eq!(space.attachments(&r.id).unwrap().len(), 1);
    let body = space.get_node(&r.id).unwrap().unwrap().body;
    assert!(body.contains("kept only where it was"), "{body}");
    assert!(!add(&mut space, &asset, &by()).unwrap().created);

    // A space that keeps large originals attaches the video too.
    crate::media::set_keep_large_originals(&mut space, true, &Provenance::person("Ada")).unwrap();
    let kept = LibraryAsset {
        id: "VID/L0/002".into(),
        ..asset
    };
    let added = add(&mut space, &kept, &by()).unwrap();
    let names: Vec<String> = space
        .attachments(&added.node.id())
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, vec!["Trip.m4a".to_string(), "Trip.mov".to_string()]);
    let body = space.get_node(&added.node.id()).unwrap().unwrap().body;
    assert!(
        body.contains("keeps the original too: [[Trip.mov]]"),
        "{body}"
    );
}

#[test]
fn marks_assets_that_left_the_albums_and_restores_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let a = add(&mut space, &photo(dir.path(), "A", "a.jpg", b"a"), &by()).unwrap();
    let b = add(&mut space, &photo(dir.path(), "B", "b.jpg", b"b"), &by()).unwrap();
    let marked = mark(&mut space, &["A".into()], &by()).unwrap();
    assert_eq!(
        marked,
        Marked {
            removed: 1,
            restored: 0
        }
    );
    let body = space.get_node(&b.node.id()).unwrap().unwrap().body;
    assert!(body.starts_with(REMOVED_LINE), "{body}");
    assert_eq!(
        space
            .property(&b.node.id(), keys::STATE)
            .unwrap()
            .as_deref(),
        Some("removed")
    );
    // Marking again changes nothing; the asset is still known, so it is never added again.
    assert_eq!(
        mark(&mut space, &["A".into()], &by()).unwrap(),
        Marked::default()
    );
    assert_eq!(known(&space, &["B".into()]).unwrap(), vec!["B".to_string()]);

    let marked = mark(&mut space, &["A".into(), "B".into()], &by()).unwrap();
    assert_eq!(
        marked,
        Marked {
            removed: 0,
            restored: 1
        }
    );
    let body = space.get_node(&b.node.id()).unwrap().unwrap().body;
    assert!(!body.contains(REMOVED_LINE));
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
    let key = alias_key("9F983DBA-EC35-42B8-8773-B597CF782EDD/L0/001");
    assert!(key.starts_with(keys::ALIAS_PREFIX));
    assert!(key.len() <= 64);
    assert!(
        key.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
    );
}
