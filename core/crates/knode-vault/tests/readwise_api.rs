//! Readwise through its API, against a local server that answers as the documented
//! `/api/v2/export/` does: pages with `nextPageCursor`, `updatedAfter`, and `429` with
//! `Retry-After`.
#![cfg(feature = "fetch")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use knode_format::{Provenance, Space, Timestamp};
use knode_vault::reading::{self, Outcome, keys, settings};

const TOKEN: &str = "test-token-1234";

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn space(dir: &Path) -> Space {
    Space::create(dir.join("Reading.knode"), "Reading").unwrap()
}

/// One request the server saw: the path with its query, and the `Authorization` header.
#[derive(Debug, Clone)]
struct Seen {
    target: String,
    auth: String,
}

/// A server that answers each request with the next of `answers` (status, extra headers, body)
/// and records what it was asked.
fn server(answers: Vec<(u16, &'static str, String)>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for (status, headers, body) in answers {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let target = first.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut auth = String::new();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                let lower = line.to_lowercase();
                if lower.starts_with("authorization:") {
                    auth = line["authorization:".len()..].trim().to_string();
                }
                if let Some(v) = lower.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut rest = vec![0; length];
            reader.read_exact(&mut rest).unwrap();
            log.lock().unwrap().push(Seen { target, auth });
            let reason = if status == 200 { "OK" } else { "Error" };
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    (format!("http://127.0.0.1:{port}/api/v2/export/"), seen)
}

fn page(results: &str, next: Option<&str>) -> String {
    let next = next.map_or("null".to_string(), |n| format!("\"{n}\""));
    format!("{{\"count\": 2, \"nextPageCursor\": {next}, \"results\": [{results}]}}")
}

const WALDEN: &str = r#"{
  "user_book_id": 1, "title": "Walden; or, Life in the Woods", "readable_title": "Walden",
  "author": "Henry David Thoreau", "source": "kindle", "category": "books",
  "source_url": null, "unique_url": null, "book_tags": [{"id": 1, "name": "classics"}],
  "highlights": [
    {"id": 10, "text": "I went to the woods because I wished to live deliberately.",
     "location": 40, "location_type": "location", "note": "", "highlighted_at": "2026-01-05T21:15:02.000Z",
     "tags": [], "is_discard": false},
    {"id": 11, "text": "A discarded passage.", "location": 41, "location_type": "location",
     "note": "", "highlighted_at": null, "tags": [], "is_discard": true}
  ]
}"#;

const RIVERS: &str = r#"{
  "user_book_id": 2, "title": "On Rivers", "readable_title": "On Rivers",
  "author": "Ada Lovelace", "category": "articles",
  "source_url": "https://example.com/on-rivers", "book_tags": [],
  "highlights": [
    {"id": 20, "text": "Rivers carry water, and \"stories\".", "location": 3,
     "location_type": "order", "note": "A good line", "highlighted_at": "2026-02-01T10:00:00Z",
     "tags": [{"id": 5, "name": "water"}], "is_discard": false}
  ]
}"#;

const RIVERS_NEW: &str = r#"{
  "user_book_id": 2, "title": "On Rivers", "readable_title": "On Rivers",
  "author": "Ada Lovelace", "category": "articles",
  "highlights": [
    {"id": 21, "text": "The sea is patient.", "location": 4, "location_type": "order",
     "note": null, "highlighted_at": "2026-03-01T10:00:00Z", "tags": [], "is_discard": false}
  ]
}"#;

#[test]
fn reads_every_page_and_then_only_what_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let (url, seen) = server(vec![
        (200, "", page(WALDEN, Some("cursor-2"))),
        (
            429,
            "Retry-After: 1\r\n",
            "{\"detail\": \"slow down\"}".into(),
        ),
        (200, "", page(RIVERS, None)),
        (200, "", page(RIVERS_NEW, None)),
    ]);

    let before = Timestamp::now();
    let report = reading::import_readwise_api_from(&mut space, &url, TOKEN, None, &me()).unwrap();
    assert_eq!(report.created(), 2);
    assert_eq!(
        report.highlights(),
        2,
        "the discarded highlight is left out"
    );
    let walden = &report.imported[0];
    assert_eq!(walden.title, "Walden");
    assert_eq!(
        space.property(&walden.id, keys::SOURCE).unwrap().as_deref(),
        Some("readwise")
    );
    assert_eq!(
        space
            .property(&walden.id, keys::LAST_HIGHLIGHT)
            .unwrap()
            .as_deref(),
        Some("2026-01-05T21:15:02+00:00")
    );
    let rivers = space.get_node(&report.imported[1].id).unwrap().unwrap();
    assert!(
        rivers.body.contains("**Note:** A good line"),
        "{}",
        rivers.body
    );
    assert!(rivers.body.contains("#water"), "{}", rivers.body);
    assert_eq!(
        space.property(&rivers.id, keys::URL).unwrap().as_deref(),
        Some("https://example.com/on-rivers")
    );

    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].target, "/api/v2/export/");
        assert!(seen.iter().all(|s| s.auth == format!("Token {TOKEN}")));
        // The page Readwise refused is asked for again, with the same cursor.
        assert_eq!(seen[1].target, "/api/v2/export/?pageCursor=cursor-2");
        assert_eq!(seen[2].target, "/api/v2/export/?pageCursor=cursor-2");
    }
    let synced = space.setting(settings::READWISE_SYNCED).unwrap().unwrap();
    assert!(synced >= before.to_string(), "{synced}");

    // The next run asks only for what changed since, and adds the new highlight.
    let again = reading::import_readwise_api_from(&mut space, &url, TOKEN, None, &me()).unwrap();
    assert_eq!(again.imported.len(), 1);
    assert_eq!(again.imported[0].outcome, Outcome::Updated);
    assert_eq!(again.imported[0].id, rivers.id);
    assert_eq!(again.highlights(), 1);
    let last = seen.lock().unwrap()[3].target.clone();
    assert!(
        last.starts_with("/api/v2/export/?updatedAfter=")
            && last.contains(&synced.replace(':', "%3A")),
        "{last}"
    );
    // The token is never kept in the space.
    let settings = space.settings().unwrap();
    assert!(settings.values().all(|v| !v.contains(TOKEN)));
}

#[test]
fn merges_with_the_csv_export_without_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let csv = tmp.path().join("readwise-data.csv");
    std::fs::write(
        &csv,
        "Highlight,Book Title,Book Author,Amazon Book ID,Note,Color,Tags,Location Type,Location,Highlighted at,Document tags
\"I went to the woods because I wished to live deliberately.\",Walden,Henry David Thoreau,B000,,yellow,,location,40,2026-01-05 21:15:02+00:00,classics
",
    )
    .unwrap();
    let from_csv = reading::import_readwise_csv(&mut space, &csv, &me()).unwrap();
    let (url, _) = server(vec![(200, "", page(WALDEN, None))]);
    let since = Some(Timestamp(0));
    let report = reading::import_readwise_api_from(&mut space, &url, TOKEN, since, &me()).unwrap();
    assert_eq!(report.imported[0].id, from_csv.imported[0].id);
    assert_eq!(report.imported[0].outcome, Outcome::Unchanged);
    assert_eq!(space.node_count().unwrap(), 1);
}

#[test]
fn a_refused_token_is_an_error_without_the_token() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let (url, _) = server(vec![(401, "", "{\"detail\": \"Invalid token.\"}".into())]);
    let error = reading::import_readwise_api_from(&mut space, &url, TOKEN, None, &me())
        .unwrap_err()
        .to_string();
    assert!(error.contains("refused the token"), "{error}");
    assert!(!error.contains(TOKEN));
    assert_eq!(space.setting(settings::READWISE_SYNCED).unwrap(), None);
}
