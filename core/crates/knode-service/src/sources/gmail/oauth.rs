//! Sign in with Google: OAuth 2.0 for installed apps (RFC 8252), with PKCE (RFC 7636) and a
//! loopback redirect to `http://127.0.0.1:<port>` on a port the system picks. The browser goes
//! to Google, the person allows Knode to read their mail (`gmail.readonly`), and Google sends
//! the browser back to Knode on this machine with a code; Knode trades it, with the PKCE
//! verifier only it knows, for a refresh token, kept in the keychain.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};

use super::http::{encode, query_pairs};

/// The only scope asked for: reading mail, nothing else.
pub const SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";

/// The build variable, and the variable at run time, with the OAuth client id of Knode's
/// Google Cloud project.
pub const CLIENT_ID_VAR: &str = "KNODE_GOOGLE_CLIENT_ID";
/// The same for the client secret Google gives "Desktop app" clients. For installed apps it is
/// not a secret (RFC 8252, section 8.5): PKCE protects the exchange.
pub const CLIENT_SECRET_VAR: &str = "KNODE_GOOGLE_CLIENT_SECRET";

/// How long the browser has to come back.
pub const SIGN_IN_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Knode's OAuth client at Google.
#[derive(Clone, PartialEq, Eq)]
pub struct GoogleClient {
    /// The client id.
    pub id: String,
    /// The client secret of a "Desktop app" client, when it has one.
    pub secret: Option<String>,
}

impl std::fmt::Debug for GoogleClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleClient")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl GoogleClient {
    /// The client of this build: `KNODE_GOOGLE_CLIENT_ID` (and `KNODE_GOOGLE_CLIENT_SECRET`)
    /// at run time, else as they were when Knode was built; `None` when there is none.
    pub fn from_env() -> Option<GoogleClient> {
        let runtime = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let id = runtime(CLIENT_ID_VAR)
            .or_else(|| option_env!("KNODE_GOOGLE_CLIENT_ID").map(str::to_string))
            .filter(|v| !v.trim().is_empty())?;
        let secret = runtime(CLIENT_SECRET_VAR)
            .or_else(|| option_env!("KNODE_GOOGLE_CLIENT_SECRET").map(str::to_string))
            .filter(|v| !v.trim().is_empty());
        Some(GoogleClient {
            id: id.trim().to_string(),
            secret: secret.map(|s| s.trim().to_string()),
        })
    }
}

/// `n` random bytes, as base64url without padding.
fn random(n: usize) -> io::Result<String> {
    let mut bytes = vec![0u8; n];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("the system gave no random bytes"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The PKCE challenge of a verifier: base64url of its SHA-256 (`S256`).
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A sign-in under way: the browser was sent to Google, and Knode listens on the loopback for
/// it to come back.
pub(crate) struct Pending {
    /// What goes with the code to Google.
    pub verifier: String,
    pub redirect_uri: String,
    /// The page the browser opens.
    pub url: String,
    /// The code, or why there is none, once the browser came back.
    pub codes: Receiver<Result<String, String>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Starts signing in: listens on a loopback port and returns the page of Google to open.
pub(crate) fn begin(client: &GoogleClient, auth_endpoint: &str) -> io::Result<Pending> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let verifier = random(32)?;
    let state = random(16)?;
    let url = format!(
        "{auth_endpoint}?client_id={}&redirect_uri={}&response_type=code&scope={}&\
         code_challenge={}&code_challenge_method=S256&state={}&access_type=offline&\
         prompt=consent",
        encode(&client.id),
        encode(&redirect_uri),
        encode(SCOPE),
        challenge(&verifier),
        encode(&state),
    );
    let (send, codes) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = cancel.clone();
        std::thread::Builder::new()
            .name("knode-google-sign-in".into())
            .spawn(move || wait_for_browser(listener, state, cancel, send))?;
    }
    Ok(Pending {
        verifier,
        redirect_uri,
        url,
        codes,
        cancel,
    })
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Knode</title>\
     <body style=\"font-family:system-ui;margin:4em\"><h1>Signed in</h1>\
     <p>Knode can read your Gmail now. Close this window and go back to Knode.</p>";
const REFUSED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Knode</title>\
     <body style=\"font-family:system-ui;margin:4em\"><h1>Not signed in</h1>\
     <p>Google did not let Knode read your mail. Go back to Knode to try again.</p>";

/// Waits for the browser on the loopback until it brings a code or an error, the sign-in
/// window ends, or the sign-in is dropped.
fn wait_for_browser(
    listener: TcpListener,
    state: String,
    cancel: Arc<AtomicBool>,
    send: Sender<Result<String, String>>,
) {
    let deadline = Instant::now() + SIGN_IN_WINDOW;
    if listener.set_nonblocking(true).is_err() {
        let _ = send.send(Err("Knode could not wait for the browser".into()));
        return;
    }
    loop {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        if Instant::now() > deadline {
            let _ = send.send(Err(
                "the browser did not come back in time: sign in again".into()
            ));
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(result) = answer_browser(stream, &state) {
                    let _ = send.send(result);
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = send.send(Err(format!("Knode could not wait for the browser: {e}")));
                return;
            }
        }
    }
}

/// Reads one request of the browser; returns the code or Google's error when it is the
/// redirect, `None` for anything else (such as `/favicon.ico`).
fn answer_browser(mut stream: TcpStream, state: &str) -> Option<Result<String, String>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 * 1024 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let head = String::from_utf8_lossy(&head);
    let target = head
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("GET "))
        .and_then(|l| l.split(' ').next())
        .unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let respond = |stream: &mut TcpStream, status: &str, page: &str| {
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: \
             {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = stream.flush();
    };
    if path != "/" {
        respond(&mut stream, "404 Not Found", "");
        return None;
    }
    let params = query_pairs(query);
    let get = |key: &str| {
        params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    if get("state").as_deref() != Some(state) {
        // Not the browser Knode sent: ignored.
        respond(&mut stream, "400 Bad Request", "");
        return None;
    }
    if let Some(error) = get("error") {
        respond(&mut stream, "200 OK", REFUSED_PAGE);
        return Some(Err(match error.as_str() {
            "access_denied" => "Google sign-in was cancelled: sign in again and allow Knode \
                                to read your mail"
                .to_string(),
            other => format!("Google refused to sign in: {other}"),
        }));
    }
    match get("code") {
        Some(code) => {
            respond(&mut stream, "200 OK", DONE_PAGE);
            Some(Ok(code))
        }
        None => {
            respond(&mut stream, "400 Bad Request", "");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_the_s256_of_the_verifier() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let verifier = random(32).unwrap();
        assert_eq!(verifier.len(), 43);
        assert_ne!(verifier, random(32).unwrap());
    }
}
