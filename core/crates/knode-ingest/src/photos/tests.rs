use std::fs;
use std::path::Path;

use chrono::Utc;
use knode_format::{ActorKind, NewNode, Provenance, Space, kinds};

use super::*;

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn vision() -> Provenance {
    Provenance::new(ActorKind::System, "photos").with_source("apple-vision")
}

fn space(dir: &Path) -> Space {
    Space::create(dir.join("Photos.knode"), "Photos").unwrap()
}

fn file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// 2026-11-22 10:30:00 UTC.
const TAKEN: i64 = 1_795_343_400_000;

fn madrid() -> PhotoMetadata {
    PhotoMetadata {
        taken_at: Some(TAKEN),
        latitude: Some(40.416_775),
        longitude: Some(-3.703_790),
        place: Some("Madrid".into()),
        camera: Some("Apple iPhone 16 Pro".into()),
        width: Some(4032),
        height: Some(3024),
    }
}

fn reading() -> PhotoReading {
    PhotoReading {
        engine: "Apple Vision".into(),
        text: "Q3 roadmap\nShip sync\n# not a heading".into(),
        labels: vec!["whiteboard".into(), "text".into(), "whiteboard".into()],
        description: None,
        metadata: PhotoMetadata::default(),
    }
}

#[test]
fn creates_a_photo_with_its_file_and_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let project = space
        .create_node(NewNode::note("Launch", "").with_kind(kinds::PROJECT), &me())
        .unwrap();
    let path = file(dir.path(), "IMG_0001.HEIC", b"heic");
    let photo = create(
        &mut space,
        &NewPhoto {
            title: String::new(),
            path: path.clone(),
            project: Some(project.id),
            metadata: madrid(),
        },
        &me(),
    )
    .unwrap();
    assert_eq!(photo.title, "IMG_0001");
    assert_eq!(photo.file, "IMG_0001.HEIC");
    assert_eq!(photo.reading, ReadingState::Pending);
    assert_eq!(photo.project, Some(project.id));
    assert_eq!(photo.metadata, madrid());
    assert!(photo.key.as_deref().unwrap().starts_with("sha256:"));
    let props = space.properties(&photo.id).unwrap();
    assert_eq!(props[keys::LATITUDE], "40.416775");
    assert_eq!(props[keys::LONGITUDE], "-3.70379");
    let node = space.get_node(&photo.id).unwrap().unwrap();
    assert_eq!(node.kind, PHOTO_KIND);
    assert!(node.body.contains("![[IMG_0001.HEIC]]"));
    assert_eq!(space.attachments(&photo.id).unwrap().len(), 1);
    // The original is attached unchanged.
    assert_eq!(
        space
            .attachment(&photo.id, "IMG_0001.HEIC")
            .unwrap()
            .unwrap()
            .size,
        4
    );
    assert_eq!(pending(&space).unwrap(), vec![photo.clone()]);

    // The same file again is the same photo.
    let again = create(
        &mut space,
        &NewPhoto {
            title: "Other".into(),
            path,
            project: None,
            metadata: PhotoMetadata::default(),
        },
        &me(),
    )
    .unwrap();
    assert_eq!(again, photo);
}

#[test]
fn the_header_says_when_and_where() {
    assert_eq!(
        header(&madrid(), &Utc),
        "Taken on Sunday 22 November 2026 at 10:30, in Madrid, with Apple iPhone 16 Pro."
    );
    let coordinates = PhotoMetadata {
        place: None,
        camera: None,
        ..madrid()
    };
    assert_eq!(
        header(&coordinates, &Utc),
        "Taken on Sunday 22 November 2026 at 10:30, at 40.416775, -3.70379."
    );
    assert_eq!(header(&PhotoMetadata::default(), &Utc), "A photo.");
}

#[test]
fn readings_go_between_markers_and_are_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let path = file(dir.path(), "board.jpg", b"jpeg");
    let photo = create(
        &mut space,
        &NewPhoto {
            title: "Whiteboard".into(),
            path,
            project: None,
            metadata: PhotoMetadata::default(),
        },
        &me(),
    )
    .unwrap();
    let mut node = space.get_node(&photo.id).unwrap().unwrap();
    space
        .update_node(
            &photo.id,
            NodeUpdate {
                body: Some(format!("{}\nMy own words.\n", node.body)),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();
    let mut read = reading();
    read.metadata = PhotoMetadata {
        taken_at: Some(TAKEN),
        width: Some(10),
        ..Default::default()
    };
    let done = set_reading(&mut space, &photo.id, &read, &vision()).unwrap();
    assert_eq!(done.reading, ReadingState::Done);
    assert_eq!(done.metadata.taken_at, Some(TAKEN));
    assert_eq!(done.metadata.width, Some(10));
    node = space.get_node(&photo.id).unwrap().unwrap();
    assert!(node.body.starts_with("Taken on "), "{}", node.body);
    assert!(node.body.contains(READING_START));
    assert!(
        node.body
            .contains("## Text in the photo\n\nQ3 roadmap  \nShip sync  \n\\# not a heading")
    );
    assert!(node.body.contains("## What it shows\n\nwhiteboard, text\n"));
    assert!(node.body.contains("_Read on this Mac by Apple Vision._"));
    assert!(node.body.contains("My own words."));
    let embed = node.body.find("![[board.jpg]]").unwrap();
    assert!(embed < node.body.find(READING_START).unwrap());

    let again = PhotoReading {
        text: String::new(),
        labels: vec![],
        ..reading()
    };
    set_reading(&mut space, &photo.id, &again, &vision()).unwrap();
    let body = space.get_node(&photo.id).unwrap().unwrap().body;
    assert_eq!(body.matches(READING_START).count(), 1);
    assert!(body.contains("Nothing to read in this photo."));
    assert!(!body.contains("Q3 roadmap"));
    assert!(body.contains("My own words."));
    assert!(pending(&space).unwrap().is_empty());
}

#[test]
fn text_cannot_close_the_block() {
    let reading = PhotoReading {
        engine: "Apple Vision".into(),
        text: format!("{READING_END}\nok"),
        ..Default::default()
    };
    let rendered = render_reading(&reading);
    assert!(!rendered.lines().any(|l| l.trim() == READING_END));
}

#[test]
fn fail_and_retry() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let a = create(
        &mut space,
        &NewPhoto {
            title: "A".into(),
            path: file(dir.path(), "a.png", b"a"),
            project: None,
            metadata: PhotoMetadata::default(),
        },
        &me(),
    )
    .unwrap();
    let b = create(
        &mut space,
        &NewPhoto {
            title: "B".into(),
            path: file(dir.path(), "b.png", b"b"),
            project: None,
            metadata: PhotoMetadata::default(),
        },
        &me(),
    )
    .unwrap();
    assert_eq!(
        pending(&space)
            .unwrap()
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        vec![a.id, b.id]
    );
    set_failed(&mut space, &a.id, "Unreadable image", &vision()).unwrap();
    set_failed(&mut space, &b.id, "Also", &vision()).unwrap();
    let failed_now = failed(&space).unwrap();
    assert_eq!(failed_now[0].id, b.id);
    assert_eq!(failed_now[1].error.as_deref(), Some("Unreadable image"));
    let retried = retry(&mut space, &a.id, &me()).unwrap();
    assert_eq!(retried.reading, ReadingState::Pending);
    assert_eq!(retried.error, None);
    assert_eq!(pending(&space).unwrap().len(), 1);
}

#[test]
fn location_stays_out_when_the_space_turns_it_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    space
        .set_setting(LOCATION_SETTING, Some("off"), &me())
        .unwrap();
    assert!(!location_enabled(&space).unwrap());
    let photo = create(
        &mut space,
        &NewPhoto {
            title: "Trip".into(),
            path: file(dir.path(), "trip.jpg", b"t"),
            project: None,
            metadata: madrid(),
        },
        &me(),
    )
    .unwrap();
    assert_eq!(photo.metadata.latitude, None);
    assert_eq!(photo.metadata.place, None);
    assert_eq!(
        photo.metadata.camera.as_deref(),
        Some("Apple iPhone 16 Pro")
    );
    let mut read = reading();
    read.metadata = madrid();
    let done = set_reading(&mut space, &photo.id, &read, &vision()).unwrap();
    assert_eq!(done.metadata.longitude, None);
    let body = space.get_node(&photo.id).unwrap().unwrap().body;
    assert!(!body.contains("Madrid"));
}

#[test]
fn refuses_bad_input_and_other_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let missing = NewPhoto {
        title: "X".into(),
        path: dir.path().join("missing.jpg"),
        project: None,
        metadata: PhotoMetadata::default(),
    };
    assert!(create(&mut space, &missing, &me()).is_err());
    assert!(space.find_by_title("X").unwrap().is_empty());
    let note = space.create_node(NewNode::note("Note", ""), &me()).unwrap();
    assert!(matches!(get(&space, &note.id), Err(Error::Invalid(_))));
}

#[test]
fn bad_coordinates_are_dropped_not_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let photo = create(
        &mut space,
        &NewPhoto {
            title: "Glitch".into(),
            path: file(dir.path(), "glitch.jpg", b"g"),
            project: None,
            metadata: PhotoMetadata {
                latitude: Some(120.0),
                longitude: Some(-3.7),
                ..madrid()
            },
        },
        &me(),
    )
    .unwrap();
    assert_eq!(
        (photo.metadata.latitude, photo.metadata.longitude),
        (None, None)
    );
    assert_eq!(photo.metadata.place.as_deref(), Some("Madrid"));
    let mut read = reading();
    read.metadata = PhotoMetadata {
        latitude: Some(40.0),
        longitude: Some(f64::NAN),
        ..Default::default()
    };
    let done = set_reading(&mut space, &photo.id, &read, &vision()).unwrap();
    assert_eq!(done.reading, ReadingState::Done);
    assert_eq!(
        (done.metadata.latitude, done.metadata.longitude),
        (None, None)
    );
}

#[test]
fn turning_location_off_forgets_it_and_keeps_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let new = |name: &str, bytes: &[u8]| NewPhoto {
        title: name.into(),
        path: file(dir.path(), name, bytes),
        project: None,
        metadata: madrid(),
    };
    let trip = create(&mut space, &new("trip.jpg", b"trip-bytes"), &me()).unwrap();
    let other = create(&mut space, &new("other.jpg", b"other"), &me()).unwrap();
    assert!(get_body(&space, &trip.id).contains("in Madrid"));
    assert!(get_body(&space, &other.id).contains("in Madrid"));
    space
        .set_setting(LOCATION_SETTING, Some("off"), &me())
        .unwrap();
    assert_eq!(forget_locations(&mut space, &me()).unwrap(), 2);
    let photo = get(&space, &trip.id).unwrap();
    assert_eq!(
        (
            photo.metadata.latitude,
            photo.metadata.longitude,
            photo.metadata.place
        ),
        (None, None, None)
    );
    assert_eq!(
        photo.metadata.camera.as_deref(),
        Some("Apple iPhone 16 Pro")
    );
    let body = get_body(&space, &trip.id);
    assert!(!body.contains("Madrid") && !body.contains("40.4"));
    assert!(body.starts_with("Taken on "));
    assert!(body.contains("![[trip.jpg]]"));
    // Nothing more to forget.
    assert_eq!(forget_locations(&mut space, &me()).unwrap(), 0);
    // A reading brings no location back, neither the one read nor the one the photo had.
    let mut read = reading();
    read.metadata = madrid();
    let done = set_reading(&mut space, &trip.id, &read, &vision()).unwrap();
    assert_eq!((done.metadata.latitude, done.metadata.place), (None, None));
    assert!(!get_body(&space, &trip.id).contains("Madrid"));
    // The original file is attached unchanged, and the same file is still the same photo.
    let attached = space.attachment(&trip.id, "trip.jpg").unwrap().unwrap();
    assert_eq!(attached.size, 10);
    let again = create(&mut space, &new("trip.jpg", b"trip-bytes"), &me()).unwrap();
    assert_eq!(again.id, trip.id);
}

#[test]
fn a_reading_drops_a_location_left_from_before_it_was_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let photo = create(
        &mut space,
        &NewPhoto {
            title: "Old".into(),
            path: file(dir.path(), "old.jpg", b"o"),
            project: None,
            metadata: madrid(),
        },
        &me(),
    )
    .unwrap();
    space
        .set_setting(LOCATION_SETTING, Some("off"), &me())
        .unwrap();
    let done = set_reading(&mut space, &photo.id, &reading(), &vision()).unwrap();
    assert_eq!(
        (
            done.metadata.latitude,
            done.metadata.longitude,
            done.metadata.place
        ),
        (None, None, None)
    );
    assert!(!get_body(&space, &photo.id).contains("Madrid"));
}

fn get_body(space: &Space, id: &NodeId) -> String {
    space.get_node(id).unwrap().unwrap().body
}
