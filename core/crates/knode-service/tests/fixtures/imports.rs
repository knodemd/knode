//! Synthetic files of every source `import.run` reads, written into a test folder: the same
//! shapes as the knode-vault tests use, smaller. Shared by the knode-service and knode-cli
//! tests. The data exports of Claude, ChatGPT and Gemini are files in knode-ingest's fixtures.

#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};

pub const PNG: &[u8] = b"\x89PNG\r\n\x1a\n synthetic image";

/// A file of knode-ingest's fixtures.
pub fn ingest(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../knode-ingest/tests/fixtures")
        .join(rel)
}

fn put(path: &Path, content: impl AsRef<[u8]>) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
    path.to_path_buf()
}

/// A zip of `entries`.
pub fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
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

// Reading -----------------------------------------------------------------------------------

/// `rivers.pdf`: two pages of text, with a title and an author.
pub fn pdf(dir: &Path) -> PathBuf {
    let pages: [&[&str]; 2] = [
        &["Chapter 1: Sources", "Rivers start in the hills."],
        &["Chapter 2: Deltas", "Rivers end at the sea."],
    ];
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
    for lines in pages {
        let mut ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("TL", vec![16.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
        ];
        for line in lines {
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
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    let info_id = doc.add_object(dictionary! {
        "Title" => Object::string_literal("On Rivers"),
        "Author" => Object::string_literal("Ada Lovelace"),
    });
    doc.trailer.set("Info", info_id);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    put(&dir.join("rivers.pdf"), out)
}

/// A web page with an article, a canonical URL and scripts that are never run.
pub const ARTICLE: &str = r#"<!doctype html>
<html><head>
<title>On Rivers - The Example Review</title>
<link rel="canonical" href="https://example.com/essays/on-rivers">
<meta property="og:site_name" content="The Example Review">
<meta name="author" content="Ada Lovelace">
<script>window.location = "https://evil.example/";</script>
</head><body>
<nav><a href="/">Home</a> <a href="/about">About</a></nav>
<article>
<h1>On Rivers</h1>
<p>Rivers carry water from the hills to the sea, and they carve the land as they go. This is a
long enough paragraph for a reader to find it, with <strong>strong words</strong> and a
<a href="https://example.com/deltas">link to deltas</a>.</p>
<p>Every river has a source, a course and a mouth. Some rivers dry up in summer and come back in
the autumn rains; others run all year, fed by snow and springs high in the mountains.</p>
<h2>Deltas</h2>
<p>Where a river meets the sea it slows down and drops what it carries. Over the years, this makes
a delta: new land, made of mud and sand, that the river crosses in many small channels.</p>
</article>
<footer>Copyright</footer>
</body></html>"#;

/// `on-rivers.html`: [`ARTICLE`] saved.
pub fn saved_page(dir: &Path) -> PathBuf {
    put(&dir.join("on-rivers.html"), ARTICLE)
}

/// `My Clippings.txt` with two books.
pub fn clippings(dir: &Path) -> PathBuf {
    put(
        &dir.join("My Clippings.txt"),
        "\u{feff}Walden (Henry David Thoreau)\r
- Your Highlight on page 3 | Location 40-41 | Added on Monday, January 5, 2026 9:15:02 PM\r
\r
I went to the woods because I wished to live deliberately.\r
==========\r
On Liberty (John Stuart Mill)\r
- Your Highlight on Location 200-202 | Added on Tuesday, January 6, 2026 8:00:00 AM\r
\r
The only freedom which deserves the name is that of pursuing our own good in our own way.\r
==========\r
",
    )
}

/// A Kindle notebook export of Walden.
pub fn notebook(dir: &Path) -> PathBuf {
    put(
        &dir.join("Walden - Notebook.html"),
        r#"<html><head><meta charset="UTF-8"><title>Notebook</title></head><body>
<div class="bodyContainer">
<div class="notebookFor">Notebook for</div>
<div class="bookTitle">Walden</div>
<div class="authors">Henry David Thoreau</div>
<hr/>
<div class="sectionHeading">Where I Lived</div>
<div class="noteHeading">Highlight(<span class="highlight_blue">blue</span>) - Page 4 &middot; Location 52</div>
<div class="noteText">Our life is frittered away by detail.</div>
</div></body></html>"#,
    )
}

/// A Readwise CSV export with one book.
pub fn readwise_csv(dir: &Path) -> PathBuf {
    put(
        &dir.join("readwise-data.csv"),
        "Highlight,Book Title,Book Author,Amazon Book ID,Note,Color,Tags,Location Type,Location,Highlighted at,Document tags
\"Simplify, simplify.\",Walden,Henry David Thoreau,B000,,yellow,,location,90,2026-01-05 21:15:02+00:00,classics
",
    )
}

/// The Readwise Markdown export, unzipped: `Readwise/Articles/On Rivers.md`. Returns the folder.
pub fn readwise_markdown(dir: &Path) -> PathBuf {
    let folder = dir.join("Readwise");
    put(
        &folder.join("Articles/On Rivers.md"),
        "# On Rivers

## Metadata
- Author: [[Ada Lovelace]]
- Full Title: On Rivers
- Category: #articles
- URL: https://example.com/essays/on-rivers

## Highlights
- Every river has a source, a course and a mouth.
",
    );
    folder
}

/// Serves `page` once on a local port; returns its URL and the request line it received.
pub fn serve_once(page: &str) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let page = page.to_string();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
        }
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        )
        .unwrap();
        request.trim().to_string()
    });
    (format!("http://127.0.0.1:{port}/essays/on-rivers"), server)
}

// Note apps ---------------------------------------------------------------------------------

/// A tiny protobuf writer, for Apple Notes bodies.
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

/// A plain note body as Notes stores it: gzipped `NoteStoreProto`.
fn note_data(text: &str) -> Vec<u8> {
    let run = Proto::default().int(1, text.encode_utf16().count() as u64);
    let note = Proto::default().bytes(2, text.as_bytes()).msg(5, run);
    let store = Proto::default().msg(2, Proto::default().int(2, 1).msg(3, note));
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&store.0).unwrap();
    gz.finish().unwrap()
}

/// The Apple Notes group container, `group.com.apple.notes`, with one note in a folder.
pub fn apple_notes(dir: &Path) -> PathBuf {
    let container = dir.join("group.com.apple.notes");
    fs::create_dir_all(&container).unwrap();
    let conn = rusqlite::Connection::open(container.join("NoteStore.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE ZICCLOUDSYNCINGOBJECT (Z_PK INTEGER PRIMARY KEY, ZIDENTIFIER TEXT,
            ZTITLE1 TEXT, ZCREATIONDATE1 REAL, ZCREATIONDATE3 REAL, ZMODIFICATIONDATE1 REAL,
            ZFOLDER INTEGER, ZMARKEDFORDELETION INTEGER, ZISPASSWORDPROTECTED INTEGER,
            ZTITLE2 TEXT, ZPARENT INTEGER, ZFOLDERTYPE INTEGER, ZTYPEUTI TEXT, ZALTTEXT TEXT,
            ZTOKENCONTENTIDENTIFIER TEXT, ZURLSTRING TEXT, ZTITLE TEXT, ZMEDIA INTEGER,
            ZFILENAME TEXT, ZNOTE INTEGER, ZNOTEDATA INTEGER);
         CREATE TABLE ZICNOTEDATA (Z_PK INTEGER PRIMARY KEY, ZNOTE INTEGER, ZDATA BLOB);
         INSERT INTO ZICCLOUDSYNCINGOBJECT (Z_PK, ZIDENTIFIER, ZTITLE2, ZPARENT, ZFOLDERTYPE)
            VALUES (1, 'F-WORK', 'Work', NULL, 0);
         INSERT INTO ZICCLOUDSYNCINGOBJECT (Z_PK, ZIDENTIFIER, ZTITLE1, ZCREATIONDATE3,
            ZMODIFICATIONDATE1, ZFOLDER, ZMARKEDFORDELETION, ZISPASSWORDPROTECTED, ZNOTEDATA)
            VALUES (10, 'N-PLAN', 'Garden plan', 814182600.0, 814182660.0, 1, 0, 0, 10);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ZICNOTEDATA (Z_PK, ZNOTE, ZDATA) VALUES (10, 10, ?1)",
        rusqlite::params![note_data("Garden plan\nWater the tomatoes.")],
    )
    .unwrap();
    container
}

/// A Bear backup (`.bear2bk`) with two notes, one linking the other.
pub fn bear_backup(dir: &Path) -> PathBuf {
    let info = |id: &str| {
        format!(
            r#"{{"version": 2, "type": "net.daringfireball.markdown",
                "net.shinyfrog.bear": {{"uniqueIdentifier": "{id}", "trashed": 0,
                "creationDate": "2026-10-20T09:50:00Z", "modificationDate": "2026-10-21T09:50:00Z"}}}}"#
        )
    };
    let (plan, ideas) = (info("B-PLAN"), info("B-IDEAS"));
    put(
        &dir.join("Notes.bear2bk"),
        zip(&[
            (
                "Bear Notes/Plan.textbundle/text.md",
                b"# Plan\nWater the beds. #garden\n\nSee [[Ideas]].\n",
            ),
            ("Bear Notes/Plan.textbundle/info.json", plan.as_bytes()),
            (
                "Bear Notes/Ideas.textbundle/text.md",
                b"# Ideas\nA greenhouse.\n",
            ),
            ("Bear Notes/Ideas.textbundle/info.json", ideas.as_bytes()),
        ]),
    )
}

const ROOT: &str = "0123456789abcdef0123456789abcdef";
const CHILD: &str = "11111111111111111111111111111111";

/// A Notion "Markdown & CSV" export, zipped, with a page and its sub-page.
pub fn notion_zip(dir: &Path) -> PathBuf {
    let root = format!("# Garden\n\nSee [Soil notes](Garden%20{ROOT}/Soil%20{CHILD}.md).\n");
    let child = format!("# Soil\n\nBack to [Garden](../Garden%20{ROOT}.md).\n");
    put(
        &dir.join("Export-notion.zip"),
        zip(&[
            (&format!("Garden {ROOT}.md"), root.as_bytes()),
            (&format!("Garden {ROOT}/Soil {CHILD}.md"), child.as_bytes()),
        ]),
    )
}

/// An Evernote export, `Work.enex`, with two notes and an image.
pub fn enex(dir: &Path) -> PathBuf {
    use base64::Engine;
    let hash = format!("{:x}", md5::compute(PNG));
    let data = base64::engine::general_purpose::STANDARD.encode(PNG);
    put(
        &dir.join("Work.enex"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <en-export export-date=\"20261020T000000Z\" application=\"Evernote\" version=\"10.0\">\n\
             <note><title>Garden plan</title><content><![CDATA[<en-note><div>Water the <b>tomatoes</b>.</div>\
             <div><en-media hash=\"{hash}\" type=\"image/png\"/></div></en-note>]]></content>\
             <created>20261020T095000Z</created><tag>Garden</tag>\
             <resource><data encoding=\"base64\">{data}</data><mime>image/png</mime>\
             <resource-attributes><file-name>beds.png</file-name></resource-attributes></resource></note>\n\
             <note><title>Ideas</title><content><![CDATA[<en-note><div>A greenhouse.</div></en-note>]]></content>\
             <created>20261020T100000Z</created></note>\n</en-export>\n"
        ),
    )
}
