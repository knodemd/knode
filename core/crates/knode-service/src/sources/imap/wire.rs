//! IMAP over TLS (RFC 9051 and RFC 3501), only what the connector needs: sign in, list the
//! mailboxes, `STATUS`, `EXAMINE`, `UID SEARCH` and `UID FETCH`. Always TLS from the first
//! byte (port 993), with the system-independent Mozilla roots of `webpki-roots`; never
//! plain text and never `STARTTLS`. Every read and write has a timeout, so a run of the
//! connector always ends.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate};
use knode_format::Timestamp;

use super::client::{
    Account, FetchedMessage, ImapConnect, ImapError, ImapSession, Mailbox, MailboxStatus,
};

/// How long connecting may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long one read or write may wait.
const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest message read; larger ones are left out (and named as such).
pub const MAX_MESSAGE: u64 = 40 * 1024 * 1024;
/// The largest literal read for anything else.
const MAX_LITERAL: usize = 64 * 1024 * 1024;

/// The real network: IMAP over TLS.
#[derive(Debug, Clone, Copy, Default)]
pub struct TlsImap;

impl ImapConnect for TlsImap {
    fn connect(
        &self,
        account: &Account,
        password: &str,
    ) -> Result<Box<dyn ImapSession>, ImapError> {
        let stream = tls_stream(account)?;
        let mut session = Session::new(stream);
        session.greeting()?;
        session.login(&account.username, password)?;
        Ok(Box::new(session))
    }
}

fn net(error: impl std::fmt::Display) -> ImapError {
    ImapError::Network(error.to_string())
}

type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

fn tls_stream(account: &Account) -> Result<Tls, ImapError> {
    let addrs = (account.host.as_str(), account.port)
        .to_socket_addrs()
        .map_err(|e| {
            net(format!(
                "{} is not a server Knode can find: {e}",
                account.host
            ))
        })?;
    let mut last = None;
    let mut tcp = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                tcp = Some(stream);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let tcp = tcp.ok_or_else(|| {
        net(match last {
            Some(e) => format!("{}:{}: {e}", account.host, account.port),
            None => format!("{} has no address", account.host),
        })
    })?;
    tcp.set_read_timeout(Some(IO_TIMEOUT)).map_err(net)?;
    tcp.set_write_timeout(Some(IO_TIMEOUT)).map_err(net)?;
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(net)?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(account.host.clone())
        .map_err(|e| net(format!("{} is not a server name: {e}", account.host)))?;
    let conn = rustls::ClientConnection::new(Arc::new(config), name).map_err(net)?;
    Ok(rustls::StreamOwned::new(conn, tcp))
}

/// A part of what the server sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tok {
    Atom(String),
    /// A quoted string or a literal.
    Bytes(Vec<u8>),
    List(Vec<Tok>),
    Nil,
}

impl Tok {
    fn text(&self) -> Option<String> {
        match self {
            Tok::Atom(a) => Some(a.clone()),
            Tok::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        }
    }

    fn number(&self) -> Option<u64> {
        match self {
            Tok::Atom(a) => a.parse().ok(),
            _ => None,
        }
    }

    fn is(&self, word: &str) -> bool {
        matches!(self, Tok::Atom(a) if a.eq_ignore_ascii_case(word))
    }
}

/// One response of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Response {
    /// `OK`, `NO`, `BAD`, `BYE` or `PREAUTH`, tagged or not (`*`), with its response code.
    Status {
        tag: String,
        status: String,
        code: Vec<Tok>,
        text: String,
    },
    /// Untagged data: `* 3 FETCH (...)` is `[3, FETCH, (...)]`.
    Data(Vec<Tok>),
    /// `+`: go on with the literal.
    Continue,
}

/// Reads responses from the server, literals included.
struct Parser<'r, R> {
    reader: &'r mut R,
    line: Vec<u8>,
    pos: usize,
}

fn protocol(what: impl Into<String>) -> ImapError {
    ImapError::Protocol(what.into())
}

fn io_error(error: io::Error) -> ImapError {
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            net("the server took too long to answer")
        }
        _ => net(error),
    }
}

impl<'r, R: BufRead> Parser<'r, R> {
    fn new(reader: &'r mut R) -> Self {
        Parser {
            reader,
            line: Vec::new(),
            pos: 0,
        }
    }

    fn next_line(&mut self) -> Result<(), ImapError> {
        self.line.clear();
        self.pos = 0;
        let n = self
            .reader
            .read_until(b'\n', &mut self.line)
            .map_err(io_error)?;
        if n == 0 {
            return Err(net("the server closed the connection"));
        }
        while matches!(self.line.last(), Some(b'\n' | b'\r')) {
            self.line.pop();
        }
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.line.get(self.pos).copied()
    }

    fn skip_spaces(&mut self) {
        while self.peek() == Some(b' ') {
            self.pos += 1;
        }
    }

    fn rest(&mut self) -> String {
        let text = String::from_utf8_lossy(&self.line[self.pos.min(self.line.len())..])
            .trim()
            .to_string();
        self.pos = self.line.len();
        text
    }

    fn read(&mut self) -> Result<Response, ImapError> {
        self.next_line()?;
        if self.peek() == Some(b'+') {
            return Ok(Response::Continue);
        }
        let tag = self.atom(None);
        self.skip_spaces();
        let word = self.atom(None);
        let upper = word.to_ascii_uppercase();
        if matches!(upper.as_str(), "OK" | "NO" | "BAD" | "BYE" | "PREAUTH") {
            self.skip_spaces();
            let code = if self.peek() == Some(b'[') {
                self.pos += 1;
                self.tokens(Some(b']'))?
            } else {
                Vec::new()
            };
            let text = self.rest();
            return Ok(Response::Status {
                tag,
                status: upper,
                code,
                text,
            });
        }
        let mut tokens = vec![Tok::Atom(word)];
        tokens.extend(self.tokens(None)?);
        Ok(Response::Data(tokens))
    }

    /// Tokens up to the end of the response, or up to `close`.
    fn tokens(&mut self, close: Option<u8>) -> Result<Vec<Tok>, ImapError> {
        let mut out = Vec::new();
        loop {
            self.skip_spaces();
            let Some(c) = self.peek() else {
                return match close {
                    None => Ok(out),
                    Some(_) => Err(protocol("a list that never ends")),
                };
            };
            if Some(c) == close {
                self.pos += 1;
                return Ok(out);
            }
            match c {
                b'(' => {
                    self.pos += 1;
                    out.push(Tok::List(self.tokens(Some(b')'))?));
                }
                b'[' => {
                    self.pos += 1;
                    out.push(Tok::List(self.tokens(Some(b']'))?));
                }
                b')' | b']' => return Err(protocol("a list closed that was never opened")),
                b'"' => out.push(Tok::Bytes(self.quoted()?)),
                b'{' => out.push(Tok::Bytes(self.literal()?)),
                _ => {
                    let atom = self.atom(close);
                    if atom.eq_ignore_ascii_case("NIL") {
                        out.push(Tok::Nil);
                    } else {
                        out.push(Tok::Atom(atom));
                    }
                }
            }
        }
    }

    /// An atom; `[...]` inside it (`BODY[]`) is part of it.
    fn atom(&mut self, close: Option<u8>) -> String {
        let start = self.pos;
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            match c {
                b'[' => depth += 1,
                b']' if depth > 0 => depth -= 1,
                b' ' | b'(' | b')' if depth == 0 => break,
                c if depth == 0 && Some(c) == close => break,
                _ => {}
            }
            self.pos += 1;
        }
        String::from_utf8_lossy(&self.line[start..self.pos]).into_owned()
    }

    fn quoted(&mut self) -> Result<Vec<u8>, ImapError> {
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None => return Err(protocol("a quoted string that never ends")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    if let Some(c) = self.peek() {
                        out.push(c);
                        self.pos += 1;
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn literal(&mut self) -> Result<Vec<u8>, ImapError> {
        self.pos += 1;
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        let len: usize = std::str::from_utf8(&self.line[start..self.pos])
            .ok()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| protocol("a literal without a length"))?;
        if self.peek() == Some(b'+') {
            self.pos += 1;
        }
        if self.peek() != Some(b'}') || self.pos + 1 != self.line.len() {
            return Err(protocol("a literal that is not at the end of its line"));
        }
        if len > MAX_LITERAL {
            return Err(protocol(format!("a literal of {len} bytes is too large")));
        }
        let mut bytes = vec![0; len];
        self.reader.read_exact(&mut bytes).map_err(io_error)?;
        // The response goes on in the next line.
        self.next_line()?;
        Ok(bytes)
    }
}

/// A piece of a command.
enum Arg<'a> {
    /// Sent as it is: a command name, a number, a list of items.
    Raw(&'a str),
    /// A string: quoted, or sent as a literal when it cannot be quoted.
    Str(&'a str),
}

/// A signed-in (or signing-in) connection over any stream: TLS for the real thing, a script
/// in tests.
pub(crate) struct Session<S: Read + Write> {
    stream: BufReader<S>,
    tag: u32,
}

impl<S: Read + Write + Send> Session<S> {
    pub(crate) fn new(stream: S) -> Self {
        Session {
            stream: BufReader::new(stream),
            tag: 0,
        }
    }

    pub(crate) fn greeting(&mut self) -> Result<(), ImapError> {
        match Parser::new(&mut self.stream).read()? {
            Response::Status { status, text, .. } if status == "BYE" => {
                Err(protocol(format!("the server said goodbye: {text}")))
            }
            Response::Status { .. } => Ok(()),
            _ => Err(protocol("the server did not greet as an IMAP server")),
        }
    }

    pub(crate) fn login(&mut self, username: &str, password: &str) -> Result<(), ImapError> {
        match self.command(&[Arg::Raw("LOGIN"), Arg::Str(username), Arg::Str(password)]) {
            Ok(_) => Ok(()),
            Err(ImapError::Protocol(text)) => Err(ImapError::Login(text)),
            Err(e) => Err(e),
        }
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), ImapError> {
        let stream = self.stream.get_mut();
        stream.write_all(bytes).map_err(io_error)?;
        stream.flush().map_err(io_error)
    }

    /// Sends a command and reads until its tagged answer. Returns the untagged data.
    fn command(&mut self, args: &[Arg<'_>]) -> Result<Vec<Vec<Tok>>, ImapError> {
        self.tag += 1;
        let tag = format!("k{}", self.tag);
        let mut line = tag.clone().into_bytes();
        for arg in args {
            line.push(b' ');
            match arg {
                Arg::Raw(text) => line.extend_from_slice(text.as_bytes()),
                Arg::Str(text) if quotable(text) => {
                    line.push(b'"');
                    for c in text.bytes() {
                        if c == b'"' || c == b'\\' {
                            line.push(b'\\');
                        }
                        line.push(c);
                    }
                    line.push(b'"');
                }
                Arg::Str(text) => {
                    line.extend_from_slice(format!("{{{}}}\r\n", text.len()).as_bytes());
                    self.send(&line)?;
                    line.clear();
                    loop {
                        match Parser::new(&mut self.stream).read()? {
                            Response::Continue => break,
                            Response::Status {
                                tag: t,
                                status,
                                text,
                                ..
                            } if t == tag => {
                                return Err(protocol(format!("{status} {text}")));
                            }
                            _ => {}
                        }
                    }
                    line.extend_from_slice(text.as_bytes());
                }
            }
        }
        line.extend_from_slice(b"\r\n");
        self.send(&line)?;
        let mut data = Vec::new();
        loop {
            match Parser::new(&mut self.stream).read()? {
                Response::Status {
                    tag: t,
                    status,
                    text,
                    code,
                } if t == tag => {
                    if status == "OK" {
                        // The response code of a tagged OK (`[READ-ONLY]`) is data too.
                        if !code.is_empty() {
                            data.push(vec![Tok::Atom("OK".into()), Tok::List(code)]);
                        }
                        return Ok(data);
                    }
                    return Err(protocol(format!("{status} {text}")));
                }
                Response::Status { status, text, .. } if status == "BYE" => {
                    return Err(net(format!("the server closed the connection: {text}")));
                }
                Response::Status { status, code, .. } => {
                    if !code.is_empty() {
                        data.push(vec![Tok::Atom(status), Tok::List(code)]);
                    }
                }
                Response::Data(tokens) => data.push(tokens),
                Response::Continue => {}
            }
        }
    }
}

/// Whether a string can be sent quoted: ASCII without line breaks or NUL.
fn quotable(text: &str) -> bool {
    text.bytes()
        .all(|c| c.is_ascii() && c != b'\r' && c != b'\n' && c != 0)
}

/// A mailbox name as a command argument.
fn mailbox_arg(name: &str) -> Arg<'_> {
    Arg::Str(name)
}

fn status_of(data: &[Vec<Tok>]) -> Result<MailboxStatus, ImapError> {
    let mut validity = None;
    let mut next = None;
    let mut pairs = |items: &[Tok]| {
        for pair in items.chunks(2) {
            if let [key, value] = pair {
                if key.is("UIDVALIDITY") {
                    validity = value.number().map(|n| n as u32);
                } else if key.is("UIDNEXT") {
                    next = value.number().map(|n| n as u32);
                }
            }
        }
    };
    for tokens in data {
        match tokens.as_slice() {
            // * OK [UIDVALIDITY 3857529045]
            [Tok::Atom(_), Tok::List(code)] => pairs(code),
            // * STATUS "INBOX" (UIDNEXT 5 UIDVALIDITY 1)
            [first, _, Tok::List(items)] if first.is("STATUS") => pairs(items),
            _ => {}
        }
    }
    Ok(MailboxStatus {
        uid_validity: validity.ok_or_else(|| protocol("no UIDVALIDITY for the mailbox"))?,
        uid_next: next,
    })
}

/// `17-Jul-1996 02:44:25 -0700`.
fn internal_date(text: &str) -> Option<Timestamp> {
    DateTime::parse_from_str(text.trim(), "%d-%b-%Y %H:%M:%S %z")
        .ok()
        .map(|d| Timestamp(d.timestamp_millis()))
}

impl<S: Read + Write + Send> ImapSession for Session<S> {
    fn capabilities(&mut self) -> Result<Vec<String>, ImapError> {
        let data = self.command(&[Arg::Raw("CAPABILITY")])?;
        Ok(data
            .iter()
            .filter(|t| t.first().is_some_and(|f| f.is("CAPABILITY")))
            .flat_map(|t| t[1..].iter().filter_map(Tok::text))
            .map(|c| c.to_ascii_uppercase())
            .collect())
    }

    fn list(&mut self) -> Result<Vec<Mailbox>, ImapError> {
        let data = self.command(&[Arg::Raw("LIST"), Arg::Str(""), Arg::Str("*")])?;
        let mut out = Vec::new();
        for tokens in data {
            if let [first, Tok::List(attributes), delimiter, name] = tokens.as_slice()
                && first.is("LIST")
            {
                let Some(name) = name.text() else { continue };
                out.push(Mailbox {
                    name,
                    delimiter: delimiter.text().and_then(|d| d.chars().next()),
                    attributes: attributes
                        .iter()
                        .filter_map(Tok::text)
                        .map(|a| a.to_ascii_lowercase())
                        .collect(),
                });
            }
        }
        Ok(out)
    }

    fn status(&mut self, mailbox: &str) -> Result<MailboxStatus, ImapError> {
        let data = self.command(&[
            Arg::Raw("STATUS"),
            mailbox_arg(mailbox),
            Arg::Raw("(UIDNEXT UIDVALIDITY)"),
        ])?;
        status_of(&data)
    }

    fn examine(&mut self, mailbox: &str) -> Result<MailboxStatus, ImapError> {
        let data = self.command(&[Arg::Raw("EXAMINE"), mailbox_arg(mailbox)])?;
        status_of(&data)
    }

    fn search(&mut self, from_uid: u32, since: Option<NaiveDate>) -> Result<Vec<u32>, ImapError> {
        let range = format!("{}:*", from_uid.max(1));
        let day = since.map(|d| d.format("%-d-%b-%Y").to_string());
        let mut args = vec![
            Arg::Raw("UID"),
            Arg::Raw("SEARCH"),
            Arg::Raw("UID"),
            Arg::Raw(&range),
        ];
        if let Some(day) = &day {
            args.push(Arg::Raw("SINCE"));
            args.push(Arg::Raw(day));
        }
        let data = self.command(&args)?;
        Ok(data
            .iter()
            .filter(|t| t.first().is_some_and(|f| f.is("SEARCH")))
            .flat_map(|t| t[1..].iter().filter_map(Tok::number))
            .map(|n| n as u32)
            .collect())
    }

    fn fetch(&mut self, uids: &[u32], gmail: bool) -> Result<Vec<FetchedMessage>, ImapError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        // Sizes first, so a huge message is never read.
        let sizes = self.command(&[
            Arg::Raw("UID"),
            Arg::Raw("FETCH"),
            Arg::Raw(&set),
            Arg::Raw("(UID RFC822.SIZE)"),
        ])?;
        let mut small = Vec::new();
        let mut large = Vec::new();
        for item in fetch_items(&sizes) {
            let uid = item_value(&item, "UID").and_then(Tok::number);
            let size = item_value(&item, "RFC822.SIZE").and_then(Tok::number);
            if let Some(uid) = uid {
                match size {
                    Some(size) if size > MAX_MESSAGE => large.push(uid as u32),
                    _ => small.push(uid as u32),
                }
            }
        }
        let mut out: Vec<FetchedMessage> = large
            .into_iter()
            .map(|uid| FetchedMessage {
                uid,
                internal_date: None,
                bytes: None,
                gmail_id: None,
                gmail_labels: Vec::new(),
            })
            .collect();
        if small.is_empty() {
            return Ok(out);
        }
        let set = small
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let items = if gmail {
            "(UID INTERNALDATE X-GM-MSGID X-GM-LABELS BODY.PEEK[])"
        } else {
            "(UID INTERNALDATE BODY.PEEK[])"
        };
        let data = self.command(&[
            Arg::Raw("UID"),
            Arg::Raw("FETCH"),
            Arg::Raw(&set),
            Arg::Raw(items),
        ])?;
        for item in fetch_items(&data) {
            let Some(uid) = item_value(&item, "UID").and_then(Tok::number) else {
                continue;
            };
            let bytes = item_value(&item, "BODY[]").and_then(|t| match t {
                Tok::Bytes(b) => Some(b.clone()),
                _ => None,
            });
            let Some(bytes) = bytes else { continue };
            out.push(FetchedMessage {
                uid: uid as u32,
                internal_date: item_value(&item, "INTERNALDATE")
                    .and_then(Tok::text)
                    .and_then(|t| internal_date(&t)),
                bytes: Some(bytes),
                gmail_id: item_value(&item, "X-GM-MSGID").and_then(Tok::number),
                gmail_labels: match item_value(&item, "X-GM-LABELS") {
                    Some(Tok::List(labels)) => labels.iter().filter_map(Tok::text).collect(),
                    _ => Vec::new(),
                },
            });
        }
        Ok(out)
    }

    fn logout(&mut self) {
        let _ = self.command(&[Arg::Raw("LOGOUT")]);
    }
}

/// The item lists of `* <n> FETCH (...)` responses.
fn fetch_items(data: &[Vec<Tok>]) -> Vec<Vec<Tok>> {
    data.iter()
        .filter_map(|tokens| match tokens.as_slice() {
            [_, word, Tok::List(items)] if word.is("FETCH") => Some(items.clone()),
            _ => None,
        })
        .collect()
}

fn item_value<'t>(items: &'t [Tok], key: &str) -> Option<&'t Tok> {
    items
        .chunks(2)
        .find(|pair| pair[0].is(key))
        .and_then(|pair| pair.get(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that answers with a script, and keeps what was sent to it.
    struct Script {
        input: io::Cursor<Vec<u8>>,
        sent: Vec<u8>,
    }

    impl Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Script {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.sent.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn session(script: &str) -> Session<Script> {
        Session::new(Script {
            input: io::Cursor::new(script.replace('\n', "\r\n").into_bytes()),
            sent: Vec::new(),
        })
    }

    fn sent(session: &mut Session<Script>) -> String {
        String::from_utf8_lossy(&session.stream.get_ref().sent).into_owned()
    }

    #[test]
    fn signs_in_and_reads_capabilities() {
        let mut s = session(
            "* OK [CAPABILITY IMAP4rev1] ready\n\
             k1 OK [CAPABILITY IMAP4rev1 X-GM-EXT-1] signed in\n\
             * CAPABILITY IMAP4rev1 X-GM-EXT-1 CONDSTORE\n\
             k2 OK done\n",
        );
        s.greeting().unwrap();
        s.login("ana@example.com", "pa\"ss").unwrap();
        let caps = s.capabilities().unwrap();
        assert!(caps.contains(&"X-GM-EXT-1".to_string()));
        assert!(sent(&mut s).starts_with("k1 LOGIN \"ana@example.com\" \"pa\\\"ss\"\r\n"));
    }

    #[test]
    fn a_refused_login_is_a_login_error() {
        let mut s = session(
            "* OK ready\n\
             k1 NO [AUTHENTICATIONFAILED] Invalid credentials (Failure)\n",
        );
        s.greeting().unwrap();
        assert!(matches!(s.login("a", "b"), Err(ImapError::Login(_))));
    }

    #[test]
    fn a_password_that_cannot_be_quoted_goes_as_a_literal() {
        let mut s = session("+ go on\nk1 OK signed in\n");
        s.login("ana", "contraseña").unwrap();
        let sent = sent(&mut s);
        assert!(sent.starts_with("k1 LOGIN \"ana\" {11}\r\ncontraseña\r\n"));
    }

    #[test]
    fn lists_mailboxes() {
        let mut s = session(
            "* LIST (\\HasNoChildren) \"/\" \"INBOX\"\n\
             * LIST (\\HasChildren \\Noselect) \"/\" \"[Gmail]\"\n\
             * LIST (\\All \\HasNoChildren) \"/\" \"[Gmail]/All Mail\"\n\
             * LIST (\\HasNoChildren) \".\" {16}\n\
             Work.&AOk-t&AOk-\n\
             * LIST (\\HasNoChildren) NIL Plain\n\
             k1 OK done\n",
        );
        let list = s.list().unwrap();
        assert_eq!(list.len(), 5);
        assert_eq!(list[0].name, "INBOX");
        assert!(!list[1].selectable());
        assert!(list[2].has("\\All"));
        assert_eq!(list[3].display_name(), "Work/été");
        assert_eq!(list[4].delimiter, None);
        assert_eq!(list[4].name, "Plain");
    }

    #[test]
    fn reads_status_and_examine() {
        let mut s = session(
            "* STATUS \"INBOX\" (UIDNEXT 44 UIDVALIDITY 7)\n\
             k1 OK done\n\
             * 172 EXISTS\n\
             * OK [UIDVALIDITY 3857529045] UIDs valid\n\
             * OK [UIDNEXT 4392] Predicted next UID\n\
             k2 OK [READ-ONLY] EXAMINE completed\n",
        );
        let status = s.status("INBOX").unwrap();
        assert_eq!(status.uid_validity, 7);
        assert_eq!(status.uid_next, Some(44));
        let examined = s.examine("INBOX").unwrap();
        assert_eq!(examined.uid_validity, 3857529045);
        assert_eq!(examined.uid_next, Some(4392));
    }

    #[test]
    fn searches_from_a_uid_and_a_day() {
        let mut s = session("* SEARCH 5 9 12\nk1 OK done\n* SEARCH\nk2 OK done\n");
        let since = NaiveDate::from_ymd_opt(2026, 11, 7).unwrap();
        assert_eq!(s.search(5, Some(since)).unwrap(), vec![5, 9, 12]);
        assert!(s.search(13, None).unwrap().is_empty());
        let sent = sent(&mut s);
        assert!(sent.contains("k1 UID SEARCH UID 5:* SINCE 7-Nov-2026\r\n"));
        assert!(sent.contains("k2 UID SEARCH UID 13:*\r\n"));
    }

    #[test]
    fn fetches_messages_with_gmail_labels_and_leaves_out_huge_ones() {
        let body = "Message-ID: <a@x>\r\nSubject: Hi\r\n\r\nHello\r\n";
        let script = format!(
            "* 1 FETCH (UID 5 RFC822.SIZE {len})\r\n\
             * 2 FETCH (UID 9 RFC822.SIZE 99999999999)\r\n\
             k1 OK done\r\n\
             * 1 FETCH (UID 5 INTERNALDATE \"17-Nov-2026 09:30:00 +0100\" X-GM-MSGID \
             1278455344230334865 X-GM-LABELS (\\Inbox \"Work/Clients\") BODY[] {{{len}}}\r\n\
             {body})\r\n\
             k2 OK done\r\n",
            len = body.len(),
        );
        let mut s = Session::new(Script {
            input: io::Cursor::new(script.into_bytes()),
            sent: Vec::new(),
        });
        let fetched = s.fetch(&[5, 9], true).unwrap();
        let huge = fetched.iter().find(|m| m.uid == 9).unwrap();
        assert!(huge.bytes.is_none());
        let mail = fetched.iter().find(|m| m.uid == 5).unwrap();
        assert_eq!(mail.bytes.as_deref(), Some(body.as_bytes()));
        assert_eq!(mail.gmail_id, Some(1278455344230334865));
        assert_eq!(mail.gmail_labels, vec!["\\Inbox", "Work/Clients"]);
        assert_eq!(
            mail.internal_date,
            internal_date("17-Nov-2026 09:30:00 +0100")
        );
        assert!(mail.internal_date.is_some());
        assert!(sent(&mut s).contains("k2 UID FETCH 5 (UID INTERNALDATE X-GM-MSGID"));
    }

    #[test]
    fn a_closed_connection_is_a_network_error() {
        let mut s = session("");
        assert!(matches!(s.capabilities(), Err(ImapError::Network(_))));
    }
}
