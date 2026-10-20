//! `import.run`: what each source is found to be, and importing it through a real service,
//! from synthetic files. No network: a URL is served from this machine.

mod fixtures;

use std::path::{Path, PathBuf};
use std::time::Duration;

use fixtures::imports as fx;
use knode_format::{GrantLevel, Provenance};
use knode_service::import::{ImportError, ImportSource, prepare};
use knode_service::keys::{fingerprint, generate_key};
use knode_service::protocol::codes;
use knode_service::{
    ChangeKind, Client, Config, Error, Event, ImportOutcome, ServiceHandle, Subscription, start,
};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);

fn me() -> Provenance {
    Provenance::person("Ada")
}

struct Fixture {
    dir: TempDir,
    _service: ServiceHandle,
    person: Client,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        let person = Client::connect(service.socket_path()).unwrap();
        let fixture = Fixture {
            dir,
            _service: service,
            person,
        };
        fixture
            .person
            .create_space(fixture.space(), "Work")
            .unwrap();
        fixture
    }

    fn files(&self) -> PathBuf {
        let files = std::fs::canonicalize(self.dir.path())
            .unwrap()
            .join("files");
        std::fs::create_dir_all(&files).unwrap();
        files
    }

    fn space(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path())
            .unwrap()
            .join("Work.knode")
    }
}

fn rpc(result: Result<impl std::fmt::Debug, Error>) -> knode_service::RpcError {
    match result {
        Err(Error::Rpc(e)) => e,
        other => panic!("expected an error from the service, got {other:?}"),
    }
}

fn next_change(events: &Subscription) -> knode_service::Change {
    loop {
        match events.recv_timeout(WAIT) {
            Some(Event::Changed(change)) => return change,
            Some(_) => continue,
            None => panic!("no changed notification"),
        }
    }
}

/// What `path` is found to be, and how it is named for people.
fn found(path: &Path) -> (ImportSource, String) {
    let what = prepare(Some(path), None, None).unwrap();
    (what.source(), what.format())
}

#[test]
fn every_source_is_told_from_its_path_and_content() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let s = |path: &Path| found(path).0;

    // Agents and Markdown.
    assert_eq!(
        found(&fx::ingest("exports/claude")),
        (ImportSource::Export, "Claude export".into())
    );
    assert_eq!(s(&fx::ingest("exports/chatgpt")), ImportSource::Export);
    assert_eq!(s(&fx::ingest("exports/gemini-json")), ImportSource::Export);
    let session = fx::ingest(
        "claude/projects/-Users-example-project/0f8e2c1a-1111-4222-8333-444455556666.jsonl",
    );
    assert_eq!(s(&session), ImportSource::Session);
    let notes = dir.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("Plan.md"), "# Plan\n").unwrap();
    assert_eq!(
        found(&notes),
        (ImportSource::Markdown, "folder of Markdown files".into())
    );
    assert_eq!(
        found(&notes.join("Plan.md")),
        (ImportSource::Markdown, "Markdown file".into())
    );

    // Reading.
    assert_eq!(found(&fx::pdf(dir)), (ImportSource::Pdf, "PDF".into()));
    assert_eq!(
        found(&fx::saved_page(dir)),
        (ImportSource::Web, "saved web page".into())
    );
    assert_eq!(
        found(&fx::clippings(dir)),
        (ImportSource::Kindle, "Kindle clippings".into())
    );
    assert_eq!(
        found(&fx::notebook(dir)),
        (ImportSource::Kindle, "Kindle notebook".into())
    );
    assert_eq!(
        found(&fx::readwise_csv(dir)),
        (ImportSource::Readwise, "Readwise CSV export".into())
    );
    let readwise = fx::readwise_markdown(dir);
    assert_eq!(
        found(&readwise),
        (ImportSource::Readwise, "Readwise Markdown export".into())
    );
    assert_eq!(
        s(&readwise.join("Articles/On Rivers.md")),
        ImportSource::Readwise
    );
    let url = prepare(None, Some("https://example.com/essays/on-rivers"), None).unwrap();
    assert_eq!(
        (url.source(), url.format()),
        (ImportSource::Web, "web page".into())
    );

    // Note apps.
    let container = fx::apple_notes(dir);
    assert_eq!(
        found(&container),
        (ImportSource::AppleNotes, "Apple Notes".into())
    );
    assert_eq!(
        s(&container.join("NoteStore.sqlite")),
        ImportSource::AppleNotes
    );
    assert_eq!(s(&fx::bear_backup(dir)), ImportSource::Bear);
    assert_eq!(s(&fx::notion_zip(dir)), ImportSource::Notion);
    assert_eq!(s(&fx::enex(dir)), ImportSource::Evernote);

    // `from` names it.
    let named = |path: &Path, from| prepare(Some(path), None, Some(from)).map(|p| p.source());
    assert_eq!(
        named(&notes, ImportSource::AppleNotes).unwrap(),
        ImportSource::AppleNotes
    );
    let other_txt = dir.join("kindle.txt");
    std::fs::write(&other_txt, "no separators").unwrap();
    assert_eq!(
        named(&other_txt, ImportSource::Kindle).unwrap(),
        ImportSource::Kindle
    );
    assert!(named(&notes, ImportSource::Export).is_err());
    assert!(named(&notes, ImportSource::Pdf).is_err());

    // Nothing Knode reads, or nothing there.
    let unknown = dir.join("photo.heic");
    std::fs::write(&unknown, [0u8; 4]).unwrap();
    let error = prepare(Some(&unknown), None, None).unwrap_err();
    assert!(matches!(error, ImportError::Unknown(_)));
    assert!(error.to_string().contains("is not something Knode imports"));
    let error = prepare(Some(&dir.join("missing")), None, None).unwrap_err();
    assert!(matches!(error, ImportError::NotFound(_)));
    assert!(prepare(Some(Path::new("relative.md")), None, None).is_err());
    assert!(prepare(None, Some("file:///etc/hosts"), None).is_err());
    assert!(prepare(None, None, Some(ImportSource::Pdf)).is_err());
}

#[test]
fn every_source_imports_once_through_the_service() {
    let f = Fixture::new();
    let space = f.space();
    let files = f.files();
    let watcher = Client::connect(f.dir.path().join("home/service.sock")).unwrap();
    let events = watcher.subscribe(Some(&space)).unwrap();

    let notes = files.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("Plan.md"), "# Plan\n\nShip it.\n").unwrap();
    let single = files.join("single.md");
    std::fs::write(&single, "# Single\n\nAlone.\n").unwrap();

    let cases: Vec<(PathBuf, ImportSource, usize)> = vec![
        (fx::ingest("exports/chatgpt"), ImportSource::Export, 1),
        (notes, ImportSource::Markdown, 1),
        (single, ImportSource::Markdown, 1),
        (
            fx::ingest(
                "claude/projects/-Users-example-project/0f8e2c1a-1111-4222-8333-444455556666.jsonl",
            ),
            ImportSource::Session,
            1,
        ),
        (fx::pdf(&files), ImportSource::Pdf, 1),
        (fx::saved_page(&files), ImportSource::Web, 1),
        (fx::clippings(&files), ImportSource::Kindle, 2),
        (fx::readwise_markdown(&files), ImportSource::Readwise, 0),
        (fx::apple_notes(&files), ImportSource::AppleNotes, 1),
        (fx::bear_backup(&files), ImportSource::Bear, 2),
        (fx::notion_zip(&files), ImportSource::Notion, 2),
        (fx::enex(&files), ImportSource::Evernote, 2),
    ];
    let mut first = true;
    for (path, source, at_least) in cases {
        let result = f
            .person
            .import_path(&space, Some(&path), None, &me())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(result.source, source, "{}", path.display());
        assert!(result.skipped.is_empty(), "{result:?}");
        assert!(result.created >= at_least, "{}: {result:?}", path.display());
        if first {
            // Every node written is announced.
            let change = next_change(&events);
            assert_eq!(change.op, ChangeKind::Create);
            first = false;
        }
        // Importing again finds the same nodes.
        let again = f
            .person
            .import_path(&space, Some(&path), None, &me())
            .unwrap();
        assert_eq!(again.created, 0, "{}: {again:?}", path.display());
        assert!(again.read() > 0, "{again:?}");
    }

    // The Kindle notebook adds its new highlight to the book from the clippings.
    let kindle = f
        .person
        .import_path(&space, Some(&fx::notebook(&files)), None, &me())
        .unwrap();
    assert_eq!(kindle.format, "Kindle notebook");
    assert_eq!(kindle.nodes[0].outcome, ImportOutcome::Updated);
    assert_eq!(kindle.highlights, 1);

    // Named with `from`.
    let csv = f
        .person
        .import_path(
            &space,
            Some(&fx::readwise_csv(&files)),
            Some(ImportSource::Readwise),
            &me(),
        )
        .unwrap();
    assert_eq!(csv.nodes[0].title, "Walden");
    assert_eq!(csv.highlights, 1);

    // Not something Knode reads: invalid, with what it takes.
    let unknown = files.join("photo.heic");
    std::fs::write(&unknown, [0u8; 4]).unwrap();
    let refused = rpc(f.person.import_path(&space, Some(&unknown), None, &me()));
    assert_eq!(refused.code, codes::INVALID);
    assert!(refused.message.contains("Kindle"), "{}", refused.message);
}

#[test]
fn a_url_is_fetched_only_when_given() {
    let f = Fixture::new();
    let space = f.space();
    let (url, server) = fx::serve_once(fx::ARTICLE);
    let result = f.person.import_url(&space, &url, &me()).unwrap();
    assert_eq!(server.join().unwrap(), "GET /essays/on-rivers HTTP/1.1");
    assert_eq!(result.source, ImportSource::Web);
    assert_eq!(result.created, 1);
    assert!(result.nodes[0].title.starts_with("On Rivers"));

    let refused = rpc(f.person.import_url(&space, "ftp://example.com/x", &me()));
    assert_eq!(refused.code, codes::INVALID);
}

#[test]
fn only_people_import() {
    let f = Fixture::new();
    let space = f.space();
    let key = generate_key().unwrap();
    f.person
        .add_agent("writer", "claude-code", &fingerprint(&key), &me())
        .unwrap();
    f.person
        .set_grant("writer", &space, GrantLevel::Write, &me())
        .unwrap();
    let writer = Client::connect_as_agent(f.dir.path().join("home/service.sock"), &key).unwrap();
    let file = fx::clippings(&f.files());
    assert_eq!(
        rpc(writer.import_path(&space, Some(&file), None, &me())).code,
        codes::FORBIDDEN
    );
    assert_eq!(
        rpc(writer.import_url(&space, "https://example.com", &me())).code,
        codes::FORBIDDEN
    );
}

#[test]
fn apple_notes_without_access_says_how_to_give_it() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let space = f.space();
    let container = fx::apple_notes(&f.files());
    std::fs::set_permissions(&container, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = f.person.import_path(
        &space,
        Some(&container),
        Some(ImportSource::AppleNotes),
        &me(),
    );
    std::fs::set_permissions(&container, std::fs::Permissions::from_mode(0o755)).unwrap();
    let Err(Error::Rpc(error)) = result else {
        // Running as root: permissions do not apply.
        return;
    };
    assert_eq!(error.code, codes::NO_ACCESS);
    assert_eq!(error.kind(), Some("no_access"));
    assert!(
        error.message.contains("Full Disk Access"),
        "{}",
        error.message
    );
}
