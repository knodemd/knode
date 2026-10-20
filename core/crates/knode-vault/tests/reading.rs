//! Reading imports: PDFs, web pages, Kindle and Readwise, from synthetic files.

use std::fs;
use std::path::Path;

use knode_format::{Node, NodeId, Provenance, Space, kinds};
use knode_vault::reading::{self, Outcome, keys};
use lopdf::content::{Content, Operation};
use lopdf::{Bookmark, Document, Object, Stream, dictionary};

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn space(dir: &Path) -> Space {
    Space::create(dir.join("Reading.knode"), "Reading").unwrap()
}

fn node(space: &Space, id: &NodeId) -> Node {
    space.get_node(id).unwrap().unwrap()
}

fn prop(space: &Space, id: &NodeId, key: &str) -> Option<String> {
    space.property(id, key).unwrap()
}

/// A PDF with one page per entry of `pages` (lines of text each), a title, an author and an
/// outline entry for every line that starts with "Chapter".
fn pdf(pages: &[&[&str]], title: &str, author: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let mut kids = Vec::new();
    let mut bookmarks = Vec::new();
    for lines in pages {
        let mut ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("TL", vec![16.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
        ];
        for line in *lines {
            ops.push(Operation::new("Tj", vec![Object::string_literal(*line)]));
            ops.push(Operation::new("T*", vec![]));
        }
        ops.push(Operation::new("ET", vec![]));
        let content = Content { operations: ops };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        for line in *lines {
            if line.starts_with("Chapter") {
                bookmarks.push((line.to_string(), page_id));
            }
        }
        kids.push(page_id.into());
    }
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    for (title, page) in bookmarks {
        doc.add_bookmark(Bookmark::new(title, [0.0, 0.0, 0.0], 0, page), None);
    }
    let outline = doc.build_outline();
    let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => pages_id };
    if let Some(outline) = outline {
        catalog.set("Outlines", outline);
    }
    let catalog_id = doc.add_object(catalog);
    doc.trailer.set("Root", catalog_id);
    let info_id = doc.add_object(dictionary! {
        "Title" => Object::string_literal(title),
        "Author" => Object::string_literal(author),
        "CreationDate" => Object::string_literal("D:20260102103000Z"),
    });
    doc.trailer.set("Info", info_id);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

#[test]
fn a_pdf_becomes_a_source_with_its_text_pages_and_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let path = tmp.path().join("field-notes.pdf");
    let bytes = pdf(
        &[
            &["Chapter One", "The river was high in spring."],
            &["# not a heading", "See [[nothing]] here."],
        ],
        "Field Notes",
        "Ada Lovelace",
    );
    fs::write(&path, &bytes).unwrap();

    let report = reading::import_pdf(&mut space, &path, &me()).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let imported = &report.imported[0];
    assert_eq!(imported.outcome, Outcome::Created);
    assert_eq!(imported.title, "Field Notes");
    let n = node(&space, &imported.id);
    assert_eq!(n.kind, kinds::SOURCE);
    assert!(
        n.body.starts_with("Original file: [[field-notes.pdf]]"),
        "{}",
        n.body
    );
    assert!(n.body.contains("<!-- page 1 -->") && n.body.contains("<!-- page 2 -->"));
    assert!(n.body.contains("## Chapter One"), "{}", n.body);
    assert!(n.body.contains("The river was high in spring."));
    // Text that looks like Markdown stays text.
    assert!(n.body.contains("\\# not a heading"), "{}", n.body);
    assert!(
        knode_md::wiki_links(&n.body)
            .iter()
            .all(|l| l.target == "field-notes.pdf")
    );

    assert_eq!(
        prop(&space, &n.id, keys::AUTHOR).as_deref(),
        Some("Ada Lovelace")
    );
    assert_eq!(prop(&space, &n.id, keys::PAGES).as_deref(), Some("2"));
    assert_eq!(
        prop(&space, &n.id, keys::FILE).as_deref(),
        Some("field-notes.pdf")
    );
    assert_eq!(
        prop(&space, &n.id, keys::PUBLISHED).as_deref(),
        Some("2026-01-02T10:30:00Z")
    );
    assert_eq!(prop(&space, &n.id, keys::SOURCE).as_deref(), Some("pdf"));
    let attachment = space.attachment(&n.id, "field-notes.pdf").unwrap().unwrap();
    assert_eq!(space.read_blob(&attachment.hash).unwrap().unwrap(), bytes);

    // The same file again, even renamed, is the same node.
    let copy = tmp.path().join("copy.pdf");
    fs::copy(&path, &copy).unwrap();
    let again = reading::import_pdf(&mut space, &copy, &me()).unwrap();
    assert_eq!(again.imported[0].id, n.id);
    assert_eq!(again.imported[0].outcome, Outcome::Unchanged);
    assert_eq!(space.node_count().unwrap(), 1);
}

#[test]
fn a_pdf_without_text_still_becomes_a_node() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let path = tmp.path().join("scan.pdf");
    fs::write(&path, pdf(&[&[]], "", "")).unwrap();

    let report = reading::import_pdf(&mut space, &path, &me()).unwrap();
    let imported = &report.imported[0];
    assert_eq!(imported.title, "scan");
    assert!(imported.note.as_deref().unwrap().contains("no text"));
    let n = node(&space, &imported.id);
    assert!(
        n.body.contains("No text was found in this PDF"),
        "{}",
        n.body
    );
    assert!(space.attachment(&n.id, "scan.pdf").unwrap().is_some());
}

#[test]
fn a_file_that_is_not_a_pdf_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let path = tmp.path().join("broken.pdf");
    fs::write(&path, b"this is not a pdf").unwrap();
    let report = reading::import_pdf(&mut space, &path, &me()).unwrap();
    assert!(report.imported.is_empty());
    assert_eq!(report.skipped[0].path, "broken.pdf");
    assert_eq!(space.node_count().unwrap(), 0);
}

const ARTICLE: &str = r#"<!doctype html>
<html><head>
<title>On Rivers - The Example Review</title>
<link rel="canonical" href="https://example.com/essays/on-rivers">
<meta property="og:site_name" content="The Example Review">
<meta name="author" content="Ada Lovelace">
<script>window.location = "https://evil.example/";</script>
<style>body { color: red }</style>
</head><body>
<nav><a href="/">Home</a> <a href="/about">About</a></nav>
<article>
<h1>On Rivers</h1>
<p>Rivers carry water from the hills to the sea, and they carve the land as they go. This is a
long enough paragraph for a reader to find it, with <strong>strong words</strong> and a
<a href="https://example.com/deltas">link to deltas</a>.</p>
<p>Every river has a source, a course and a mouth. Some rivers dry up in summer and come back in
the autumn rains; others run all year, fed by snow and springs high in the mountains.</p>
<script>document.write("<p>never shown</p>")</script>
<h2>Deltas</h2>
<p>Where a river meets the sea it slows down and drops what it carries. Over the years, this makes
a delta: new land, made of mud and sand, that the river crosses in many small channels.</p>
<ul><li>The Nile</li><li>The Mekong</li></ul>
</article>
<footer>Copyright</footer>
</body></html>"#;

#[test]
fn a_saved_page_keeps_the_article_and_its_canonical_url() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let path = tmp.path().join("on-rivers.html");
    fs::write(&path, ARTICLE).unwrap();

    let report = reading::import_html(&mut space, &path, None, &me()).unwrap();
    let imported = &report.imported[0];
    assert_eq!(imported.outcome, Outcome::Created);
    let n = node(&space, &imported.id);
    assert_eq!(n.kind, kinds::SOURCE);
    assert!(n.title.contains("On Rivers"), "{}", n.title);
    assert!(n.body.contains("Rivers carry water"), "{}", n.body);
    assert!(n.body.contains("**strong words**"), "{}", n.body);
    assert!(
        n.body
            .contains("[link to deltas](https://example.com/deltas)")
    );
    assert!(n.body.contains("## Deltas"), "{}", n.body);
    assert!(n.body.contains("- The Nile"), "{}", n.body);
    assert!(!n.body.contains("never shown") && !n.body.contains("evil.example"));
    assert!(!n.body.contains("color: red"));
    assert_eq!(
        prop(&space, &n.id, keys::URL).as_deref(),
        Some("https://example.com/essays/on-rivers")
    );
    assert_eq!(prop(&space, &n.id, keys::SOURCE).as_deref(), Some("web"));
    assert!(space.attachment(&n.id, "on-rivers.html").unwrap().is_some());

    // The same page fetched later, from another URL, is the same node.
    let again = reading::import_page(
        &mut space,
        ARTICLE,
        Some("https://example.com/x?utm=1"),
        &me(),
    )
    .unwrap();
    assert_eq!(again.imported[0].id, n.id);
    assert_eq!(space.node_count().unwrap(), 1);
}

#[test]
fn a_page_that_changed_updates_its_node_unless_it_was_edited() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let url = Some("https://example.com/essays/on-rivers");
    let first = reading::import_page(&mut space, ARTICLE, url, &me()).unwrap();
    let id = first.imported[0].id;

    let same = reading::import_page(&mut space, ARTICLE, url, &me()).unwrap();
    assert_eq!(same.imported[0].outcome, Outcome::Unchanged);

    let changed = ARTICLE.replace("The Mekong", "The Danube");
    let updated = reading::import_page(&mut space, &changed, url, &me()).unwrap();
    assert_eq!(updated.imported[0].outcome, Outcome::Updated);
    assert!(node(&space, &id).body.contains("The Danube"));

    // Edited in Knode: a newer page leaves it as it is.
    let mut body = node(&space, &id).body;
    body.push_str("\n\nMy own note.");
    space
        .update_node(
            &id,
            knode_format::NodeUpdate {
                body: Some(body.clone()),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();
    let newer = ARTICLE.replace("The Mekong", "The Rhine");
    let kept = reading::import_page(&mut space, &newer, url, &me()).unwrap();
    assert_eq!(kept.imported[0].outcome, Outcome::Edited);
    assert_eq!(node(&space, &id).body, body);
}

#[cfg(feature = "fetch")]
#[test]
fn a_page_is_fetched_with_a_clear_user_agent() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut agent = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some(value) = line.to_lowercase().strip_prefix("user-agent:") {
                agent = value.trim().to_string();
            }
        }
        let page = ARTICLE.replace("https://example.com/essays/on-rivers", "/essays/on-rivers");
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        )
        .unwrap();
        agent
    });

    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let url = format!("http://127.0.0.1:{port}/essays/on-rivers?ref=feed");
    let report = reading::import_url(&mut space, &url, &me()).unwrap();
    let agent = server.join().unwrap();
    assert!(agent.starts_with("knode/"), "{agent}");

    let id = report.imported[0].id;
    // The canonical URL, made absolute against the page's URL.
    assert_eq!(
        prop(&space, &id, keys::URL),
        Some(format!("http://127.0.0.1:{port}/essays/on-rivers"))
    );
    assert!(prop(&space, &id, keys::FETCHED).is_some());
    assert!(node(&space, &id).body.contains("Rivers carry water"));

    assert!(reading::fetch("file:///etc/hosts").is_err());
}

const CLIPPINGS: &str = "\u{feff}Walden (Henry David Thoreau)\r
- Your Highlight on page 3 | Location 40-41 | Added on Monday, January 5, 2026 9:15:02 PM\r
\r
I went to the woods because I wished to live deliberately.\r
==========\r
Walden (Henry David Thoreau)\r
- Your Note on page 3 | Location 41 | Added on Monday, January 5, 2026 9:16:00 PM\r
\r
Compare with [[Emerson]].\r
==========\r
Walden (Henry David Thoreau)\r
- Your Bookmark on page 9 | Location 120 | Added on Monday, January 5, 2026 9:20:00 PM\r
\r
\r
==========\r
On Liberty (John Stuart Mill)\r
- Your Highlight on Location 200-202 | Added on Tuesday, January 6, 2026 8:00:00 AM\r
\r
The only freedom which deserves the name is that of pursuing our own good in our own way.\r
==========\r
";

#[test]
fn kindle_clippings_become_one_node_per_book() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let path = tmp.path().join("My Clippings.txt");
    fs::write(&path, CLIPPINGS).unwrap();

    let report = reading::import_kindle_clippings(&mut space, &path, &me()).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(report.created(), 2);
    assert_eq!(report.highlights(), 2);
    let walden = &report.imported[0];
    assert_eq!(walden.title, "Walden");
    let n = node(&space, &walden.id);
    assert_eq!(n.kind, kinds::SOURCE);
    assert_eq!(
        n.body,
        "## Page 3, Location 40-41\n\n\
         > I went to the woods because I wished to live deliberately.\n\n\
         **Note:** Compare with \\[\\[Emerson\\]\\].\n\n\
         *Highlighted 2026-01-05T21:15:02*"
    );
    assert!(knode_md::wiki_links(&n.body).is_empty());
    assert_eq!(
        prop(&space, &n.id, keys::AUTHOR).as_deref(),
        Some("Henry David Thoreau")
    );
    assert_eq!(prop(&space, &n.id, keys::SOURCE).as_deref(), Some("kindle"));
    assert_eq!(
        prop(&space, &n.id, keys::LAST_HIGHLIGHT).as_deref(),
        Some("2026-01-05T21:15:02")
    );

    // Again: nothing new.
    let again = reading::import_kindle_clippings(&mut space, &path, &me()).unwrap();
    assert_eq!(again.highlights(), 0);
    assert!(
        again
            .imported
            .iter()
            .all(|i| i.outcome == Outcome::Unchanged)
    );

    // A new highlight is added at the end; one removed by hand does not come back.
    space
        .update_node(
            &walden.id,
            knode_format::NodeUpdate {
                body: Some("My summary.".into()),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();
    let more = format!(
        "{CLIPPINGS}Walden (Henry David Thoreau)\r
- Your Highlight on page 10 | Location 150-151 | Added on Friday, January 9, 2026 7:00:00 AM\r
\r
Simplify, simplify.\r
==========\r
"
    );
    fs::write(&path, more).unwrap();
    let report = reading::import_kindle_clippings(&mut space, &path, &me()).unwrap();
    assert_eq!(report.highlights(), 1);
    assert_eq!(report.imported[0].outcome, Outcome::Updated);
    let body = node(&space, &walden.id).body;
    assert!(
        body.starts_with("My summary.\n\n## Page 10, Location 150-151"),
        "{body}"
    );
    assert!(!body.contains("deliberately"));
    assert_eq!(space.node_count().unwrap(), 2);
}

const NOTEBOOK: &str = r#"<html><head><meta charset="UTF-8"><title>Notebook</title>
<script>alert("no")</script></head><body><div class="bodyContainer">
<div class="notebookFor">Notebook for</div>
<div class="bookTitle">Walden</div>
<div class="authors">Henry David Thoreau</div>
<div class="citation">Citation (APA): Thoreau, H. D. Walden [Kindle Android version].</div>
<hr/>
<div class="sectionHeading">Where I Lived</div>
<div class="noteHeading">Highlight(<span class="highlight_yellow">yellow</span>) - Page 3 &middot; Location 40</div>
<div class="noteText">I went to the woods because I wished to live
deliberately.</div>
<div class="noteHeading">Highlight(<span class="highlight_blue">blue</span>) - Page 4 &middot; Location 52</div>
<div class="noteText">Our life is frittered away by detail.</div>
<div class="noteHeading">Note - Page 4 &middot; Location 52</div>
<div class="noteText">So true.</div>
</div></body></html>"#;

#[test]
fn a_kindle_notebook_joins_the_same_book_from_clippings() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let clippings = tmp.path().join("My Clippings.txt");
    fs::write(&clippings, CLIPPINGS).unwrap();
    reading::import_kindle_clippings(&mut space, &clippings, &me()).unwrap();

    let path = tmp.path().join("Walden - Notebook.html");
    fs::write(&path, NOTEBOOK).unwrap();
    let report = reading::import_kindle_notebook(&mut space, &path, &me()).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let walden = &report.imported[0];
    assert_eq!(walden.outcome, Outcome::Updated);
    // The first highlight was already there, from the clippings.
    assert_eq!(walden.highlights, 1);
    let body = node(&space, &walden.id).body;
    assert!(
        body.ends_with(
            "## Where I Lived - Page 4, Location 52\n\n\
             > Our life is frittered away by detail.\n\n\
             **Note:** So true."
        ),
        "{body}"
    );
    assert_eq!(space.node_count().unwrap(), 2);
}

const READWISE_CSV: &str = "Highlight,Book Title,Book Author,Amazon Book ID,Note,Color,Tags,Location Type,Location,Highlighted at,Document tags
\"I went to the woods because I wished to live deliberately.\",Walden,Henry David Thoreau,B000,,yellow,,location,40,2026-01-05 21:15:02+00:00,classics
\"Rivers carry water, and \"\"stories\"\".\",On Rivers,Ada Lovelace,,A good line,yellow,\"water,favorite\",order,3,2026-02-01 10:00:00+00:00,\"nature, essays\"
";

#[test]
fn readwise_csv_merges_with_kindle_and_adds_only_new_highlights() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let clippings = tmp.path().join("My Clippings.txt");
    fs::write(&clippings, CLIPPINGS).unwrap();
    reading::import_kindle_clippings(&mut space, &clippings, &me()).unwrap();

    let path = tmp.path().join("readwise-data.csv");
    fs::write(&path, READWISE_CSV).unwrap();
    let report = reading::import_readwise_csv(&mut space, &path, &me()).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(report.imported.len(), 2);
    // Walden: the same highlight as Kindle; only the document tag is new.
    let walden = &report.imported[0];
    assert_eq!(walden.highlights, 0);
    assert_eq!(node(&space, &walden.id).tags, vec!["classics".to_string()]);

    let rivers = &report.imported[1];
    assert_eq!(rivers.outcome, Outcome::Created);
    let n = node(&space, &rivers.id);
    assert_eq!(n.tags, vec!["essays".to_string(), "nature".to_string()]);
    assert_eq!(
        n.body,
        "## Highlight\n\n\
         > Rivers carry water, and \"stories\".\n\n\
         **Note:** A good line\n\n\
         *Tags: #favorite #water - Highlighted 2026-02-01T10:00:00+00:00*"
    );
    assert_eq!(
        prop(&space, &n.id, keys::SOURCE).as_deref(),
        Some("readwise")
    );

    let again = reading::import_readwise_csv(&mut space, &path, &me()).unwrap();
    assert_eq!(again.highlights(), 0);
    assert!(
        again
            .imported
            .iter()
            .all(|i| i.outcome == Outcome::Unchanged)
    );
    assert_eq!(space.node_count().unwrap(), 3);
}

const READWISE_MD: &str = "# On Rivers

![rw-book-cover](https://example.com/cover.png)

## Metadata
- Author: [[Ada Lovelace]]
- Full Title: On Rivers
- Category: #articles
- Document Tags: [[nature]] [[essays]]
- URL: https://example.com/essays/on-rivers

## Highlights
- Rivers carry water, and \"stories\". ([Location 3](https://readwise.io/to_kindle?x=1))
    - Note: A good line
- Every river has a source, a course and a mouth.
  It ends at the sea.
    - Tags: [[geography]]
";

#[test]
fn readwise_markdown_reads_metadata_and_highlights() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = space(tmp.path());
    let dir = tmp.path().join("Readwise");
    fs::create_dir_all(dir.join("Articles")).unwrap();
    fs::write(dir.join("Articles/On Rivers.md"), READWISE_MD).unwrap();
    fs::write(dir.join("Articles/Empty.md"), "# Empty\n\nNothing here.\n").unwrap();

    let report = reading::import_readwise_markdown(&mut space, &dir, &me()).unwrap();
    assert_eq!(report.imported.len(), 1);
    assert_eq!(report.skipped[0].path, "Articles/Empty.md");
    let rivers = &report.imported[0];
    assert_eq!(rivers.title, "On Rivers");
    assert_eq!(rivers.highlights, 2);
    let n = node(&space, &rivers.id);
    assert_eq!(n.tags, vec!["essays".to_string(), "nature".to_string()]);
    assert_eq!(
        prop(&space, &n.id, keys::URL).as_deref(),
        Some("https://example.com/essays/on-rivers")
    );
    assert_eq!(
        prop(&space, &n.id, keys::AUTHOR).as_deref(),
        Some("Ada Lovelace")
    );
    assert_eq!(
        prop(&space, &n.id, keys::CATEGORY).as_deref(),
        Some("articles")
    );
    assert_eq!(
        n.body,
        "## Location 3\n\n\
         > Rivers carry water, and \"stories\".\n\n\
         **Note:** A good line\n\n\
         ## Highlight\n\n\
         > Every river has a source, a course and a mouth.\n\
         > It ends at the sea.\n\n\
         *Tags: #geography*"
    );

    // The same article from the CSV export: the shared highlight is not added twice.
    let csv = tmp.path().join("readwise-data.csv");
    fs::write(&csv, READWISE_CSV).unwrap();
    let report = reading::import_readwise_csv(&mut space, &csv, &me()).unwrap();
    let rivers_again = report.imported.iter().find(|i| i.id == n.id).unwrap();
    assert_eq!(rivers_again.highlights, 0);

    let again = reading::import_readwise_markdown(&mut space, &dir, &me()).unwrap();
    assert_eq!(again.highlights(), 0);
}
