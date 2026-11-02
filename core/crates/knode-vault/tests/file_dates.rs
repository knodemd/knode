//! Imported notes keep the dates of their files.

mod common;

use common::put;
use knode_format::{Provenance, Space};
use knode_vault::import;

fn me() -> Provenance {
    Provenance::person("Ada")
}

/// A note from another app keeps the dates of its file: the most recently changed notes are the
/// ones last changed there, not every note at the time of the import.
#[test]
fn notes_keep_the_dates_of_their_files() {
    use std::time::{Duration, UNIX_EPOCH};
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    put(&vault, "Old.md", "Written long ago.\n");
    put(&vault, "Newer.md", "Written later.\n");
    let at = |path: &str, millis: u64| {
        std::fs::File::options()
            .write(true)
            .open(vault.join(path))
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_millis(millis))
            .unwrap();
    };
    at("Old.md", 1_704_067_200_000); // 2024-01-01
    at("Newer.md", 1_735_689_600_000); // 2025-01-01
    let mut space = Space::create(dir.path().join("Space.knode"), "Space").unwrap();
    import(&mut space, &vault, &me()).unwrap();
    let old = space.find_by_title("Old").unwrap()[0].clone();
    assert_eq!(old.updated_at.0, 1_704_067_200_000);
    assert!(old.created_at <= old.updated_at);
    let order: Vec<_> = space
        .recent(10)
        .unwrap()
        .into_iter()
        .map(|n| n.title)
        .collect();
    assert_eq!(order, vec!["Newer", "Old"]);
    // Importing again an unchanged folder changes nothing.
    let again = import(&mut space, &vault, &me()).unwrap();
    assert!(
        again.created.is_empty() && again.updated.is_empty(),
        "{again:?}"
    );
}
