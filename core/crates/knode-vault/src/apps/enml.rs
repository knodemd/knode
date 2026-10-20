//! ENML, the XHTML Evernote keeps note bodies in, as Markdown.
//!
//! Read: paragraphs (`div`, `p`), line breaks, headings, bold, italic, strikethrough, inline
//! code, links, lists, checkboxes (`en-todo`, and Evernote 10's `--en-todo` lists), tables,
//! code blocks (`pre` and Evernote 10's `--en-codeblock`), quotes, rules and attachments
//! (`en-media`, by the MD5 hash of their data). Encrypted text (`en-crypt`) is left out, with a
//! line saying so. Colors, fonts and underlining are dropped.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::markdown::{escape, wrap};

/// What [`to_markdown`] asks of the note's attachments.
pub(crate) trait Media {
    /// The Markdown for the attachment with this MD5 hash (hexadecimal), if the note has it.
    fn media(&mut self, hash: &str) -> Option<String>;
}

/// Decodes the XML entities and character references of text Evernote wrote, including the
/// HTML ones its DTD declares, such as `&nbsp;`.
pub(crate) fn entity(name: &str) -> Option<String> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(code).map(String::from);
    }
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        "middot" => '·',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "euro" => '€',
        "times" => '×',
        "deg" => '°',
        _ => return None,
    };
    Some(c.to_string())
}

/// The value of an attribute, entities decoded.
pub(crate) fn attr(tag: &BytesStart<'_>, name: &str) -> Option<String> {
    tag.attributes()
        .flatten()
        .find_map(|a| (a.key.local_name().as_ref() == name).then(|| decode(&a.value)))
}

/// Text with `&name;` references decoded; unknown ones are kept as written.
pub(crate) fn decode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        match tail.find(';').filter(|end| *end <= 10) {
            Some(end) => match entity(&tail[..end]) {
                Some(text) => {
                    out.push_str(&text);
                    rest = &tail[end + 1..];
                }
                None => {
                    out.push('&');
                    rest = tail;
                }
            },
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

#[derive(Debug, Clone)]
enum List {
    Bullets { todo: bool },
    Numbers(u32),
}

/// A buffer being written: the note, or a table cell, a quote or a link being collected.
#[derive(Debug, Default)]
struct Frame {
    kind: FrameKind,
    text: String,
}

#[derive(Debug, Default, PartialEq)]
enum FrameKind {
    #[default]
    Root,
    Quote,
    Cell,
    Link(String),
    Code,
    Mark(&'static str),
    Heading(usize),
}

struct Writer<'m> {
    frames: Vec<Frame>,
    lists: Vec<List>,
    rows: Vec<Vec<String>>,
    tables: Vec<Vec<Vec<String>>>,
    /// Elements opened, to know which one an end closes.
    open: Vec<Opened>,
    media: &'m mut dyn Media,
    in_pre: usize,
}

/// What an opened element did, undone at its end.
#[derive(Debug, PartialEq)]
enum Opened {
    Nothing,
    Block,
    Frame,
    List,
    Item,
    Table,
    Row,
    Pre,
}

impl Writer<'_> {
    fn top(&mut self) -> &mut String {
        &mut self.frames.last_mut().expect("the root frame").text
    }

    /// Ends the current line, as a block ends.
    fn block_break(&mut self) {
        let in_cell = self.frames.iter().any(|f| f.kind == FrameKind::Cell);
        let top = self.top();
        if top.is_empty() || top.ends_with("\n\n") {
            return;
        }
        if in_cell {
            if !top.ends_with("<br>") {
                top.push_str("<br>");
            }
            return;
        }
        if top.ends_with('\n') {
            top.push('\n');
        } else {
            top.push_str("\n\n");
        }
    }

    fn line_break(&mut self) {
        if self.in_pre > 0 {
            self.top().push('\n');
        } else if !self.lists.is_empty() || self.frames.iter().any(|f| f.kind == FrameKind::Cell) {
            self.top().push(' ');
        } else {
            self.block_break();
        }
    }

    fn text(&mut self, text: &str) {
        if self.in_pre > 0 {
            self.top().push_str(text);
            return;
        }
        let collapsed: String = {
            let mut out = String::new();
            let mut space = false;
            for c in text.chars() {
                if c.is_whitespace() && c != '\u{a0}' {
                    space = true;
                } else {
                    if space {
                        out.push(' ');
                    }
                    space = false;
                    out.push(c);
                }
            }
            if space {
                out.push(' ');
            }
            out
        };
        let top = self.top();
        let at_line_start = top.is_empty() || top.ends_with('\n');
        let collapsed = if at_line_start {
            collapsed.trim_start().to_string()
        } else {
            collapsed
        };
        if collapsed.is_empty() {
            return;
        }
        let escaped = escape(&collapsed.replace('\u{a0}', " "));
        self.top().push_str(&escaped);
    }

    /// Starts a list item line.
    fn item(&mut self, checked: Option<bool>) {
        let depth = self.lists.len().saturating_sub(1);
        let top = self.top();
        if !top.is_empty() && !top.ends_with('\n') {
            top.push('\n');
        }
        let marker = match self.lists.last_mut() {
            Some(List::Numbers(n)) => {
                *n += 1;
                format!("{}{n}. ", "   ".repeat(depth))
            }
            Some(List::Bullets { todo }) => {
                let todo = *todo;
                let mark = match (checked, todo) {
                    (Some(true), _) => "[x] ",
                    (Some(false), _) | (None, true) => "[ ] ",
                    _ => "",
                };
                format!("{}- {mark}", "  ".repeat(depth))
            }
            None => "- ".to_string(),
        };
        self.top().push_str(&marker);
    }

    fn push_frame(&mut self, kind: FrameKind) {
        self.frames.push(Frame {
            kind,
            text: String::new(),
        });
    }

    fn pop_frame(&mut self) {
        let Some(frame) = self.frames.pop() else {
            return;
        };
        if self.frames.is_empty() {
            self.frames.push(frame);
            return;
        }
        let text = frame.text;
        match frame.kind {
            FrameKind::Root => self.top().push_str(&text),
            FrameKind::Quote => {
                self.block_break();
                let quoted: Vec<String> = text
                    .trim()
                    .lines()
                    .map(|l| {
                        if l.is_empty() {
                            ">".into()
                        } else {
                            format!("> {l}")
                        }
                    })
                    .collect();
                let quoted = quoted.join("\n");
                self.top().push_str(&quoted);
                self.block_break();
            }
            FrameKind::Cell => {
                let cell = text
                    .trim()
                    .trim_end_matches("<br>")
                    .replace('\n', " ")
                    .replace('|', "\\|");
                if let Some(row) = self.rows.last_mut() {
                    row.push(cell);
                }
            }
            FrameKind::Link(href) => {
                let label = text.trim();
                let link = if label.is_empty() {
                    format!("<{href}>")
                } else {
                    format!("[{label}](<{href}>)")
                };
                self.top().push_str(&link);
            }
            FrameKind::Code => {
                self.block_break();
                let code = text.trim_matches('\n');
                self.top().push_str(&format!("```\n{code}\n```"));
                self.block_break();
            }
            FrameKind::Mark(marker) => {
                let wrapped = wrap(&text, marker);
                self.top().push_str(&wrapped);
            }
            FrameKind::Heading(level) => {
                self.block_break();
                let line = format!("{} {}", "#".repeat(level), text.trim());
                self.top().push_str(&line);
                self.block_break();
            }
        }
    }

    fn start(&mut self, tag: &BytesStart<'_>, empty: bool) {
        let name = tag.local_name().as_ref().to_lowercase();
        let style = attr(tag, "style").unwrap_or_default();
        let opened = match name.as_str() {
            "en-note" => Opened::Nothing,
            "br" => {
                self.line_break();
                Opened::Nothing
            }
            "hr" => {
                self.block_break();
                self.top().push_str("---");
                self.block_break();
                Opened::Nothing
            }
            "en-todo" => {
                let checked = attr(tag, "checked").is_some_and(|v| v == "true");
                let top = self.top();
                if !top.is_empty() && !top.ends_with('\n') {
                    top.push('\n');
                }
                let mark = if checked { "- [x] " } else { "- [ ] " };
                self.top().push_str(mark);
                Opened::Nothing
            }
            "en-media" => {
                let hash = attr(tag, "hash").unwrap_or_default().to_lowercase();
                let md = self.media.media(&hash).unwrap_or_default();
                self.top().push_str(&md);
                Opened::Nothing
            }
            "en-crypt" => {
                self.block_break();
                self.top()
                    .push_str("*(Encrypted text from Evernote, not imported.)*");
                self.block_break();
                // Its content is skipped: see `skip`.
                Opened::Nothing
            }
            "img" => {
                if let Some(src) = attr(tag, "src") {
                    let alt = attr(tag, "alt").unwrap_or_default();
                    self.top()
                        .push_str(&format!("![{}](<{src}>)", escape(&alt)));
                }
                Opened::Nothing
            }
            "div" | "p" | "section" | "article" | "header" | "footer" | "center" => {
                if style.contains("en-codeblock") {
                    self.block_break();
                    self.in_pre += 1;
                    self.push_frame(FrameKind::Code);
                    Opened::Pre
                } else {
                    if self.in_pre > 0 {
                        let top = self.top();
                        if !top.is_empty() && !top.ends_with('\n') {
                            top.push('\n');
                        }
                    } else if self.lists.is_empty() {
                        self.block_break();
                    }
                    Opened::Block
                }
            }
            "pre" => {
                self.block_break();
                self.in_pre += 1;
                self.push_frame(FrameKind::Code);
                Opened::Pre
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name[1..].parse().unwrap_or(1);
                self.push_frame(FrameKind::Heading(level));
                Opened::Frame
            }
            "b" | "strong" => {
                self.push_frame(FrameKind::Mark("**"));
                Opened::Frame
            }
            "i" | "em" => {
                self.push_frame(FrameKind::Mark("*"));
                Opened::Frame
            }
            "s" | "strike" | "del" => {
                self.push_frame(FrameKind::Mark("~~"));
                Opened::Frame
            }
            "code" if self.in_pre == 0 => {
                self.push_frame(FrameKind::Mark("`"));
                Opened::Frame
            }
            "a" => match attr(tag, "href").filter(|h| !h.is_empty()) {
                Some(href) => {
                    self.push_frame(FrameKind::Link(href.replace(['<', '>'], "")));
                    Opened::Frame
                }
                None => Opened::Nothing,
            },
            "blockquote" => {
                self.push_frame(FrameKind::Quote);
                Opened::Frame
            }
            "ul" | "ol" => {
                if self.lists.is_empty() {
                    self.block_break();
                }
                self.lists.push(if name == "ol" {
                    List::Numbers(0)
                } else {
                    List::Bullets {
                        todo: style.contains("en-todo:true"),
                    }
                });
                Opened::List
            }
            "li" => {
                let checked = style
                    .contains("--en-checked:")
                    .then(|| style.contains("--en-checked:true"));
                self.item(checked);
                Opened::Item
            }
            "table" => {
                self.block_break();
                self.tables.push(std::mem::take(&mut self.rows));
                Opened::Table
            }
            "tr" => {
                self.rows.push(Vec::new());
                Opened::Row
            }
            "td" | "th" => {
                self.push_frame(FrameKind::Cell);
                Opened::Frame
            }
            _ => Opened::Nothing,
        };
        if empty {
            self.finish(opened);
        } else {
            self.open.push(opened);
        }
    }

    fn end(&mut self) {
        let opened = self.open.pop().unwrap_or(Opened::Nothing);
        self.finish(opened);
    }

    fn finish(&mut self, opened: Opened) {
        match opened {
            Opened::Nothing => {}
            Opened::Block => {
                if self.in_pre == 0 && self.lists.is_empty() {
                    self.block_break();
                }
            }
            Opened::Frame => self.pop_frame(),
            Opened::Pre => {
                self.in_pre = self.in_pre.saturating_sub(1);
                self.pop_frame();
            }
            Opened::List => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.block_break();
                }
            }
            Opened::Item => {
                let top = self.top();
                if !top.ends_with('\n') {
                    top.push('\n');
                }
            }
            Opened::Row => {}
            Opened::Table => {
                let rows = std::mem::replace(&mut self.rows, self.tables.pop().unwrap_or_default());
                let width = rows.iter().map(Vec::len).max().unwrap_or(0);
                if width > 0 {
                    let mut table = String::new();
                    for (i, row) in rows.iter().enumerate() {
                        let cells: Vec<String> = (0..width)
                            .map(|c| row.get(c).cloned().unwrap_or_default())
                            .collect();
                        table.push_str(&format!("| {} |\n", cells.join(" | ")));
                        if i == 0 {
                            table.push_str(&format!("|{}\n", " --- |".repeat(width)));
                        }
                    }
                    self.block_break();
                    self.top().push_str(&table);
                    self.block_break();
                }
            }
        }
    }
}

/// The Markdown of an ENML note body.
pub(crate) fn to_markdown(enml: &str, media: &mut dyn Media) -> String {
    let mut reader = Reader::from_str(enml);
    let config = reader.config_mut();
    config.check_end_names = false;
    config.allow_unmatched_ends = true;
    config.allow_dangling_amp = true;
    let mut w = Writer {
        frames: vec![Frame::default()],
        lists: Vec::new(),
        rows: Vec::new(),
        tables: Vec::new(),
        open: Vec::new(),
        media,
        in_pre: 0,
    };
    let mut skip = 0usize;
    loop {
        let event = match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(event) => event,
        };
        if skip > 0 {
            match event {
                Event::Start(_) => skip += 1,
                Event::End(_) => skip -= 1,
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(tag) => {
                let crypt = tag.local_name().as_ref() == "en-crypt";
                w.start(&tag, false);
                if crypt {
                    w.open.pop();
                    skip = 1;
                }
            }
            Event::Empty(tag) => w.start(&tag, true),
            Event::End(_) => w.end(),
            Event::Text(text) => {
                let raw = text.xml10_content();
                w.text(&decode(&raw));
            }
            Event::CData(data) => {
                let raw = data.into_inner().into_owned();
                w.text(&raw);
            }
            Event::GeneralRef(r) => {
                let name = r.xml10_content();
                match entity(&name) {
                    Some(text) => w.text(&text),
                    None => w.text(&format!("&{name};")),
                }
            }
            _ => {}
        }
    }
    while w.frames.len() > 1 {
        w.pop_frame();
    }
    w.frames.pop().map(|f| f.text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoMedia;
    impl Media for NoMedia {
        fn media(&mut self, hash: &str) -> Option<String> {
            Some(format!("![[{hash}.png]]"))
        }
    }

    fn md(enml: &str) -> String {
        super::super::markdown::tidy(&to_markdown(enml, &mut NoMedia))
    }

    #[test]
    fn converts_enml() {
        assert_eq!(
            md(
                "<en-note><div>Hello <b>bold</b> and <i>it</i>&nbsp;now</div><div><br/></div><div>Two</div></en-note>"
            ),
            "Hello **bold** and *it* now\n\nTwo\n"
        );
        assert_eq!(
            md(
                "<en-note><h2>Plan</h2><ul><li><div>one</div></li><li>two<ol><li>a</li></ol></li></ul></en-note>"
            ),
            "## Plan\n\n- one\n- two\n   1. a\n"
        );
        assert_eq!(
            md(
                "<en-note><div><en-todo checked=\"true\"/>done</div><div><en-todo/>open</div></en-note>"
            ),
            "- [x] done\n\n- [ ] open\n"
        );
        assert_eq!(
            md(
                "<en-note><ul style=\"--en-todo:true;\"><li style=\"--en-checked:true;\">a</li><li style=\"--en-checked:false;\">b</li></ul></en-note>"
            ),
            "- [x] a\n- [ ] b\n"
        );
        assert_eq!(
            md(
                "<en-note><table><tr><td>A</td><td>B</td></tr><tr><td>1</td><td>x|y</td></tr></table></en-note>"
            ),
            "| A | B |\n| --- | --- |\n| 1 | x\\|y |\n"
        );
        assert_eq!(
            md(
                "<en-note><div style=\"--en-codeblock:true;\"><div>let a = 1;</div><div>a * 2</div></div></en-note>"
            ),
            "```\nlet a = 1;\na * 2\n```\n"
        );
        assert_eq!(
            md(
                "<en-note><div>See <a href=\"https://x.y\">this</a> <en-media hash=\"ABC\" type=\"image/png\"/></div><en-crypt>secret</en-crypt><blockquote>q</blockquote></en-note>"
            ),
            "See [this](<https://x.y>) ![[abc.png]]\n\n*(Encrypted text from Evernote, not imported.)*\n\n> q\n"
        );
    }
}
