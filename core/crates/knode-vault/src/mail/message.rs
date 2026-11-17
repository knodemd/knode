//! One message, read with `mail-parser`: its headers decoded (encoded words, charsets), its
//! text as Markdown, and the files it carries.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use chrono::{DateTime, FixedOffset, Utc};
use mail_parser::{Address, HeaderName, HeaderValue, MessageParser, MimeHeaders, PartType};
use sha2::{Digest, Sha256};

use super::MailOptions;
use crate::reading::markdown::from_html;

/// Gmail's labels, in a Takeout `.mbox`.
const GMAIL_LABELS: &str = "X-Gmail-Labels";

/// Gmail labels that are the state of a message, not a folder it is in.
const STATE_LABELS: &[&str] = &["opened", "unread"];

/// Folders whose messages are left out unless a filter names them: spam and trash.
const JUNK: &[&str] = &[
    "spam",
    "junk",
    "trash",
    "bin",
    "deleted messages",
    "deleted items",
];

/// Where the bytes of a message are, to read its files again when they are attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Origin {
    /// In an `mbox` file: the bytes after the `From ` line, still escaped.
    Mbox {
        file: PathBuf,
        offset: u64,
        len: u64,
    },
    /// An `.emlx` file.
    Emlx(PathBuf),
    /// Nowhere Knode reads files from: a `.partial.emlx`, whose files Apple Mail keeps apart.
    Elsewhere,
}

/// Someone a message is from or to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Person {
    pub name: Option<String>,
    /// In lower case.
    pub address: String,
}

impl Person {
    /// The name, else the address.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.address)
    }
}

/// A file a message carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attached {
    pub name: String,
    pub image: bool,
}

/// One message, ready to be threaded and written.
#[derive(Debug, Clone)]
pub(crate) struct Mail {
    /// The `Message-ID` without `<` and `>`, or `knode-<hash>` when the message has none.
    pub id: String,
    /// The ids in `References`, then `In-Reply-To`, each once.
    pub parents: Vec<String>,
    /// The subject, decoded.
    pub subject: String,
    /// The subject without `Re:` and `Fwd:`.
    pub topic: String,
    pub from: Vec<Person>,
    pub to: Vec<Person>,
    pub cc: Vec<Person>,
    pub date: Option<DateTime<FixedOffset>>,
    /// The text, as Markdown.
    pub body: String,
    pub attachments: Vec<Attached>,
    pub folders: BTreeSet<String>,
    pub origin: Origin,
    /// The order it was read in.
    pub order: usize,
}

impl Mail {
    /// Whether the message names the messages it answers.
    pub fn has_parents(&self) -> bool {
        !self.parents.is_empty()
    }

    /// Whether the subject says it is a reply or a forward.
    pub fn is_reply(&self) -> bool {
        self.subject.trim() != self.topic.trim()
    }

    /// The message that started the thread, as this message names it: the first id of
    /// `References`, else `In-Reply-To`, else the message itself.
    pub fn root(&self) -> &str {
        self.parents.first().unwrap_or(&self.id)
    }

    /// When it was sent, in UTC.
    pub fn utc(&self) -> Option<DateTime<Utc>> {
        self.date.map(|d| d.with_timezone(&Utc))
    }

    /// Everyone it is from or to.
    pub fn people(&self) -> impl Iterator<Item = &Person> {
        self.from.iter().chain(&self.to).chain(&self.cc)
    }
}

fn parser() -> MessageParser {
    MessageParser::new()
        .with_minimal_headers()
        .with_message_ids()
        .header_text(HeaderName::Other(GMAIL_LABELS.into()))
}

/// Reads a message; `None` when it has no headers at all.
pub(crate) fn parse(
    raw: &[u8],
    origin: Origin,
    folder: Option<&str>,
    order: usize,
) -> Option<Mail> {
    let message = parser().parse(raw)?;
    if message.headers().is_empty() {
        return None;
    }
    let subject = clean_line(message.subject().unwrap_or_default());
    let topic = clean_line(message.thread_name().unwrap_or_default());
    let from = people(message.from());
    let to = message.all_to().flat_map(|a| people(Some(a))).collect();
    let mut cc: Vec<Person> = message.all_cc().flat_map(|a| people(Some(a))).collect();
    cc.extend(message.all_bcc().flat_map(|a| people(Some(a))));
    let date = message.date().filter(|d| d.is_valid()).and_then(|d| {
        let seconds = (d.tz_hour as i32 * 3600 + d.tz_minute as i32 * 60)
            * if d.tz_before_gmt { -1 } else { 1 };
        let offset = FixedOffset::east_opt(seconds)?;
        DateTime::from_timestamp(d.to_timestamp(), 0).map(|t| t.with_timezone(&offset))
    });
    let body = body(&message);

    let mut parents = Vec::new();
    for value in [message.references(), message.in_reply_to()] {
        for id in ids(value) {
            if !parents.contains(&id) {
                parents.push(id);
            }
        }
    }
    let mut folders = BTreeSet::new();
    match message.header(GMAIL_LABELS).and_then(HeaderValue::as_text) {
        Some(labels) => folders.extend(
            gmail_labels(labels)
                .into_iter()
                .filter(|l| !STATE_LABELS.contains(&l.to_lowercase().as_str())),
        ),
        None => folders.extend(folder.map(str::to_string)),
    }
    let attachments = message
        .attachments()
        .enumerate()
        .map(|(i, part)| {
            let name = attachment_name(part, i);
            let image = part
                .content_type()
                .is_some_and(|t| t.ctype().eq_ignore_ascii_case("image"));
            Attached { name, image }
        })
        .collect();

    let id = match message
        .message_id()
        .map(clean_id)
        .filter(|id| !id.is_empty())
    {
        Some(id) => id,
        None => {
            // The same message read again gets the same id.
            let mut hash = Sha256::new();
            for part in [
                from.first()
                    .map(|p: &Person| p.address.as_str())
                    .unwrap_or(""),
                &date.map(|d| d.to_rfc3339()).unwrap_or_default(),
                &subject,
                &body,
            ] {
                hash.update(part.as_bytes());
                hash.update([0]);
            }
            let hex: String = hash.finalize()[..16]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("knode-{hex}")
        }
    };
    Some(Mail {
        id,
        parents,
        subject,
        topic,
        from,
        to,
        cc,
        date,
        body,
        attachments,
        folders,
        origin,
        order,
    })
}

/// The files of a message, with the names [`parse`] gave them.
pub(crate) fn attachment_bytes(raw: &[u8]) -> Vec<(String, Vec<u8>)> {
    let Some(message) = parser().parse(raw) else {
        return Vec::new();
    };
    message
        .attachments()
        .enumerate()
        .map(|(i, part)| {
            let bytes = match &part.body {
                PartType::Message(inner) => inner.raw_message().to_vec(),
                _ => part.contents().to_vec(),
            };
            (attachment_name(part, i), bytes)
        })
        .collect()
}

/// The file name of an attachment, else `attachment-<n>` with an extension from its type.
fn attachment_name(part: &mail_parser::MessagePart<'_>, index: usize) -> String {
    if let Some(name) = part
        .attachment_name()
        .map(clean_line)
        .filter(|n| !n.is_empty())
    {
        // Only the file name: never a path.
        return name.rsplit(['/', '\\']).next().unwrap_or(&name).to_string();
    }
    let extension = match &part.body {
        PartType::Message(_) => Some("eml".to_string()),
        _ => part.content_type().and_then(|t| {
            let sub = t.subtype()?.to_ascii_lowercase();
            Some(match sub.as_str() {
                "jpeg" => "jpg".to_string(),
                "plain" => "txt".to_string(),
                other => other
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .take(8)
                    .collect(),
            })
        }),
    };
    match extension.filter(|e| !e.is_empty()) {
        Some(ext) => format!("attachment-{}.{ext}", index + 1),
        None => format!("attachment-{}", index + 1),
    }
}

/// The people of an address header; groups are flattened.
fn people(address: Option<&Address<'_>>) -> Vec<Person> {
    let Some(address) = address else {
        return Vec::new();
    };
    address
        .iter()
        .filter_map(|a| {
            let addr = a.address()?.trim().to_lowercase();
            if !addr.contains('@') {
                return None;
            }
            let name = a
                .name()
                .map(clean_line)
                .map(|n| n.trim_matches(['"', '\'']).trim().to_string())
                .filter(|n| !n.is_empty() && n.to_lowercase() != addr);
            Some(Person {
                name,
                address: addr,
            })
        })
        .collect()
}

/// The message ids of a `References` or `In-Reply-To` header.
fn ids(value: &HeaderValue<'_>) -> Vec<String> {
    let raw: Vec<String> = match value {
        HeaderValue::Text(text) => vec![text.to_string()],
        HeaderValue::TextList(list) => list.iter().map(|t| t.to_string()).collect(),
        _ => Vec::new(),
    };
    raw.iter()
        .flat_map(|t| t.split_whitespace().map(clean_id).collect::<Vec<_>>())
        .filter(|id| !id.is_empty())
        .collect()
}

/// A message id without `<`, `>` and spaces.
fn clean_id(id: &str) -> String {
    id.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '<' | '>'))
        .collect()
}

/// One line: runs of whitespace become one space.
fn clean_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Gmail's labels: separated by commas, a label with a comma in quotes.
pub(crate) fn gmail_labels(text: &str) -> Vec<String> {
    let mut labels = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => labels.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    labels.push(current);
    labels
        .into_iter()
        .map(|l| clean_line(&l))
        .filter(|l| !l.is_empty())
        .collect()
}

/// The text of a message as Markdown: its text parts, else its HTML parts turned into Markdown.
fn body(message: &mail_parser::Message<'_>) -> String {
    let texts: Vec<String> = message
        .text_bodies()
        .filter_map(|part| match &part.body {
            PartType::Text(text) => Some(plain_to_markdown(text)),
            _ => None,
        })
        .filter(|t| !t.is_empty())
        .collect();
    if !texts.is_empty() {
        return texts.join("\n\n");
    }
    let htmls: Vec<String> = message
        .html_bodies()
        .filter_map(|part| match &part.body {
            PartType::Html(html) => Some(shift_headings(&from_html(html))),
            PartType::Text(text) => Some(plain_to_markdown(text)),
            _ => None,
        })
        .filter(|t| !t.trim().is_empty())
        .collect();
    htmls.join("\n\n").trim().to_string()
}

/// Plain text as Markdown that reads back as the same text: quotes (`>`) and lists stay what
/// they are, but a line does not become a heading, a rule or a marker of a section, `[[` does
/// not become a link and `<tag>` is not HTML.
pub(crate) fn plain_to_markdown(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let line = line.trim_end();
        let mut out = escape_inline(line);
        let trimmed = out.trim_start();
        let rule = trimmed.len() >= 2
            && trimmed
                .chars()
                .all(|c| matches!(c, '-' | '=' | '_' | '*' | ' '));
        if trimmed.starts_with('#') || rule {
            let indent = out.len() - trimmed.len();
            out.insert(indent, '\\');
        }
        if line.is_empty() && lines.last().is_some_and(String::is_empty) {
            continue;
        }
        lines.push(out);
    }
    lines.join("\n").trim().to_string()
}

/// Escapes `[[`, `]]` and `<` that does not open an autolink (`<ada@example.com>`,
/// `<https://example.com>`).
fn escape_inline(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '[' | ']' if chars.get(i + 1) == Some(&c) => {
                out.push('\\');
                out.push(c);
                out.push('\\');
                out.push(c);
                i += 2;
                continue;
            }
            '<' => {
                let rest: String = chars[i + 1..].iter().collect();
                let autolink = rest.find('>').is_some_and(|end| {
                    let inner = &rest[..end];
                    !inner.is_empty()
                        && !inner.contains(char::is_whitespace)
                        && (inner.contains('@') || inner.contains("://"))
                        && !inner.starts_with('!')
                });
                if !autolink {
                    out.push('\\');
                }
                out.push('<');
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

/// Moves headings two levels down, so a message's own headings stay inside its section.
fn shift_headings(markdown: &str) -> String {
    let mut fence: Option<&str> = None;
    let mut out = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            out.push(line.to_string());
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(&trimmed[..3]);
            out.push(line.to_string());
            continue;
        }
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&level) && trimmed[level..].starts_with(' ') {
            let level = (level + 2).min(6);
            out.push(format!(
                "{} {}",
                "#".repeat(level),
                trimmed[..].trim_start_matches('#').trim()
            ));
        } else if trimmed.starts_with("<!--") {
            out.push(format!("\\{trimmed}"));
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n")
}

/// Gathers messages as they are read: leaves out what the filters and spam rules say, and reads
/// each `Message-ID` once, keeping every folder it was found in.
pub(crate) struct Collector<'a> {
    options: &'a MailOptions,
    folders: Vec<String>,
    mails: Vec<Mail>,
    index: HashMap<String, usize>,
    pub filtered: usize,
    pub junk: usize,
    pub duplicates: usize,
    read: usize,
}

impl<'a> Collector<'a> {
    pub fn new(options: &'a MailOptions) -> Self {
        Self {
            options,
            folders: options
                .folders
                .iter()
                .map(|f| clean_line(f).to_lowercase())
                .filter(|f| !f.is_empty())
                .collect(),
            mails: Vec::new(),
            index: HashMap::new(),
            filtered: 0,
            junk: 0,
            duplicates: 0,
            read: 0,
        }
    }

    /// The order the next message is read in.
    pub fn next_order(&mut self) -> usize {
        self.read += 1;
        self.read
    }

    /// Whether a filter names this folder: the whole path or its last part.
    fn wanted(&self, folder: &str) -> bool {
        let folder = folder.to_lowercase();
        let last = folder.rsplit('/').next().unwrap_or(&folder).to_string();
        self.folders.iter().any(|f| *f == folder || *f == last)
    }

    fn is_junk(&self, folder: &str) -> bool {
        let folder = folder.to_lowercase();
        let last = folder.rsplit('/').next().unwrap_or(&folder);
        JUNK.contains(&last) && !self.wanted(&folder)
    }

    /// Adds a message read.
    pub fn add(&mut self, mail: Mail) {
        if mail.folders.iter().any(|f| self.is_junk(f)) {
            self.junk += 1;
            return;
        }
        if !self.folders.is_empty() && !mail.folders.iter().any(|f| self.wanted(f)) {
            self.filtered += 1;
            return;
        }
        if self.options.since.is_some() || self.options.until.is_some() {
            let day = mail.utc().map(|d| d.date_naive());
            let inside = day.is_some_and(|day| {
                self.options.since.is_none_or(|since| day >= since)
                    && self.options.until.is_none_or(|until| day <= until)
            });
            if !inside {
                self.filtered += 1;
                return;
            }
        }
        if let Some(&at) = self.index.get(&mail.id) {
            self.duplicates += 1;
            self.mails[at].folders.extend(mail.folders);
            return;
        }
        self.index.insert(mail.id.clone(), self.mails.len());
        self.mails.push(mail);
    }

    pub fn into_mails(self) -> Vec<Mail> {
        self.mails
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin::Emlx(PathBuf::from("/m.emlx"))
    }

    #[test]
    fn decodes_headers_and_quoted_printable() {
        let raw = b"Message-ID: <a1@example.com>\r\n\
From: =?UTF-8?B?w4FuZ2VsYSBSdWl6?= <Angela@Example.com>\r\n\
To: \"Bob Stone\" <bob@example.org>, carol@example.org\r\n\
Subject: =?ISO-8859-1?Q?Re:_Caf=E9_plans?=\r\n\
Date: Tue, 3 Mar 2026 10:15:00 +0100\r\n\
In-Reply-To: <a0@example.com>\r\n\
References: <root@example.com> <a0@example.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
Caf=C3=A9 at ten?\r\n\
# not a heading\r\n\
--\r\n\
See [[nothing]] <b>here</b> <bob@example.org>\r\n";
        let mail = parse(raw, origin(), None, 1).unwrap();
        assert_eq!(mail.id, "a1@example.com");
        assert_eq!(mail.subject, "Re: Café plans");
        assert_eq!(mail.topic, "Café plans");
        assert!(mail.is_reply());
        assert_eq!(mail.from[0].name.as_deref(), Some("Ángela Ruiz"));
        assert_eq!(mail.from[0].address, "angela@example.com");
        assert_eq!(mail.to.len(), 2);
        assert_eq!(mail.to[1].name, None);
        assert_eq!(mail.parents, ["root@example.com", "a0@example.com"]);
        assert_eq!(mail.root(), "root@example.com");
        assert_eq!(
            mail.utc().unwrap().to_rfc3339(),
            "2026-03-03T09:15:00+00:00"
        );
        assert_eq!(
            mail.body,
            "Café at ten?\n\\# not a heading\n\\--\nSee \\[\\[nothing\\]\\] \\<b>here\\</b> <bob@example.org>"
        );
    }

    #[test]
    fn prefers_text_and_turns_html_into_markdown() {
        let both = b"Message-ID: <b@example.com>\r\nFrom: a@example.com\r\n\
Content-Type: multipart/alternative; boundary=x\r\n\r\n--x\r\n\
Content-Type: text/plain\r\n\r\nThe text.\r\n--x\r\nContent-Type: text/html\r\n\r\n\
<p>The <b>HTML</b>.</p>\r\n--x--\r\n";
        assert_eq!(parse(both, origin(), None, 1).unwrap().body, "The text.");

        let html = b"Message-ID: <c@example.com>\r\nFrom: a@example.com\r\n\
Content-Type: text/html; charset=windows-1252\r\nContent-Transfer-Encoding: base64\r\n\r\n\
PGgxPkFnZW5kYTwvaDE+PHA+T25lIDxhIGhyZWY9Imh0dHBzOi8vZXhhbXBsZS5jb20iPmxpbms8L2E+IJY8L3A+\r\n";
        let body = parse(html, origin(), None, 1).unwrap().body;
        assert!(body.starts_with("### Agenda"), "{body}");
        assert!(body.contains("[link](https://example.com)"), "{body}");
        assert!(body.contains('\u{2013}'), "windows-1252 en dash: {body}");
    }

    #[test]
    fn reads_gmail_labels_and_names_attachments() {
        let raw =
            b"From: a@example.com\r\nX-Gmail-Labels: Inbox,Opened,\"Clients, 2026\",Work/Plans\r\n\
Subject: Files\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\n\
Content-Type: text/plain\r\n\r\nSee attached.\r\n--x\r\n\
Content-Type: image/png\r\nContent-Disposition: attachment; filename=\"../plan.png\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--x\r\nContent-Type: application/pdf\r\n\
Content-Disposition: attachment\r\nContent-Transfer-Encoding: base64\r\n\r\naGk=\r\n--x--\r\n";
        let mail = parse(raw, origin(), Some("ignored"), 1).unwrap();
        assert_eq!(
            mail.folders.iter().collect::<Vec<_>>(),
            ["Clients, 2026", "Inbox", "Work/Plans"]
        );
        assert!(mail.id.starts_with("knode-"));
        assert_eq!(
            mail.attachments,
            [
                Attached {
                    name: "plan.png".into(),
                    image: true
                },
                Attached {
                    name: "attachment-2.pdf".into(),
                    image: false
                }
            ]
        );
        let files = attachment_bytes(raw);
        assert_eq!(files[0], ("plan.png".to_string(), b"hello".to_vec()));
        assert_eq!(files[1], ("attachment-2.pdf".to_string(), b"hi".to_vec()));
        // The same message read again gets the same id.
        assert_eq!(parse(raw, origin(), None, 2).unwrap().id, mail.id);
    }

    #[test]
    fn splits_labels() {
        assert_eq!(gmail_labels("A, \"B, C\" ,D"), ["A", "B, C", "D"]);
        assert!(gmail_labels("").is_empty());
    }
}
