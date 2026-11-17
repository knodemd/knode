//! HTTPS for the Gmail connector, behind a trait so tests answer for Google: [`Http`]. The
//! real one is [`UreqHttp`]: TLS with rustls, a timeout on every request.

use std::io;
use std::time::Duration;

/// How long one request may take, from connecting to the last byte.
const TIMEOUT: Duration = Duration::from_secs(30);
/// The largest answer read: a message of the Gmail API in `raw` format, base64.
const MAX_BODY: u64 = 64 * 1024 * 1024;

/// An answer.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// The body.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Bodies hold mail and tokens: never logged.
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("body", &self.body.len())
            .finish()
    }
}

impl HttpResponse {
    /// An answer with this status and a JSON body.
    pub fn json(status: u16, body: &serde_json::Value) -> Self {
        HttpResponse {
            status,
            body: body.to_string().into_bytes(),
        }
    }

    /// Whether the status is 2xx.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Makes HTTPS requests.
pub trait Http: Send + Sync {
    /// `GET url` with `Authorization: Bearer <bearer>`.
    fn get(&self, url: &str, bearer: &str) -> io::Result<HttpResponse>;
    /// `POST url` with the form `form`, `application/x-www-form-urlencoded`.
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> io::Result<HttpResponse>;
}

/// HTTPS with `ureq` and rustls.
pub struct UreqHttp {
    agent: ureq::Agent,
}

impl Default for UreqHttp {
    fn default() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .https_only(true)
            .build();
        UreqHttp {
            agent: config.into(),
        }
    }
}

fn read(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> io::Result<HttpResponse> {
    let mut response = response.map_err(|e| match e {
        ureq::Error::Io(e) => e,
        ureq::Error::Timeout(_) => io::Error::new(io::ErrorKind::TimedOut, "Google took too long"),
        other => io::Error::other(other.to_string()),
    })?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_vec()
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(HttpResponse { status, body })
}

impl Http for UreqHttp {
    fn get(&self, url: &str, bearer: &str) -> io::Result<HttpResponse> {
        read(
            self.agent
                .get(url)
                .header("Authorization", &format!("Bearer {bearer}"))
                .call(),
        )
    }

    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> io::Result<HttpResponse> {
        read(self.agent.post(url).send_form(form.iter().copied()))
    }
}

/// `text` for a URL's query.
pub(crate) fn encode(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The parameters of a query (`a=1&b=two%20words`), decoded.
pub(crate) fn query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |s: &str| {
                percent_encoding::percent_decode_str(&s.replace('+', " "))
                    .decode_utf8_lossy()
                    .into_owned()
            };
            (decode(key), decode(value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_queries() {
        assert_eq!(encode("after:12 b/c"), "after%3A12%20b%2Fc");
        let pairs = query_pairs("code=4%2F0Ab&state=x+y&empty");
        assert_eq!(pairs[0], ("code".into(), "4/0Ab".into()));
        assert_eq!(pairs[1], ("state".into(), "x y".into()));
        assert_eq!(pairs[2], ("empty".into(), String::new()));
    }
}
