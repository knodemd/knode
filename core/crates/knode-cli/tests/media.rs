//! `knode import` of photos, audio and video, and `knode spaces photo-location`.

mod common;

use std::fs;

use common::Sandbox;

#[test]
fn photos_recordings_and_videos_are_imported_once() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let camera = sb.path("Camera");
    fs::create_dir_all(&camera).unwrap();
    fs::write(camera.join("board.jpg"), b"jpeg").unwrap();
    fs::write(camera.join("talk.mov"), b"movie").unwrap();

    let photo = camera.join("board.jpg");
    let out = sb.ok(&["import", photo.to_str().unwrap()]);
    assert!(out.contains("(photo): 1 new"), "{out}");
    assert!(out.contains("reads the text in photos"), "{out}");

    let out = sb.ok(&["import", "--from", "media", camera.to_str().unwrap()]);
    assert!(
        out.contains("(folder of media): 1 new, 0 updated, 1 unchanged"),
        "{out}"
    );
    assert!(out.contains("transcribes audio and video"), "{out}");

    let text = sb.path("notes.txt");
    fs::write(&text, b"x").unwrap();
    let err = sb.fails(&["import", "--from", "media", text.to_str().unwrap()]);
    assert!(err.contains("is not a photo"), "{err}");
}

#[test]
fn photo_location_is_turned_off_per_space() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let out = sb.ok(&["spaces", "photo-location", "work", "off"]);
    assert!(
        out.contains("no longer keep where they were taken"),
        "{out}"
    );
    let out = sb.ok(&["spaces", "photo-location", "work", "on"]);
    assert!(
        out.contains("keep where they were taken from now on"),
        "{out}"
    );
}
