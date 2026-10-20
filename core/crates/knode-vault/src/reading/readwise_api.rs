//! Readwise through its API: the highlights export (`/api/v2/export/`), page by page, only what
//! changed since the last sync of the space.

use std::io::Read;
use std::time::Duration;

use knode_format::{ActorKind, Provenance, Space, Timestamp};
use serde::Deserialize;

use super::book::{Book, Highlight};
use super::readwise::{place, write_books};
use super::{ReadingReport, USER_AGENT, settings};
use crate::Result;

/// The Readwise highlights export endpoint.
pub const READWISE_API: &str = "https://readwise.io/api/v2/export/";

/// The most one page of the export may weigh.
const MAX_PAGE: u64 = 64 * 1024 * 1024;
/// How many times a page is asked for again when Readwise says to slow down (`429`).
const RETRIES: u32 = 5;
/// The longest wait for a `Retry-After`, in seconds.
const MAX_WAIT: u64 = 120;

/// Imports the highlights of the Readwise account of `token`, through the Readwise API: one node
/// of kind `source` per book or article, written by `by`, exactly as
/// [`import_readwise_csv`](super::import_readwise_csv) writes them, so the API, the CSV and the
/// Markdown exports (and Kindle) merge into the same nodes and highlights.
///
/// Only the highlights updated after `since` are fetched; without it, those updated since the
/// last sync of the space (the [`settings::READWISE_SYNCED`] setting), or all of them on the
/// first run. When every page was read and `by` is a person, the time the sync started is
/// recorded in that setting. The token is only sent to Readwise: it is never written to the
/// space or to errors.
pub fn import_readwise_api(
    space: &mut Space,
    token: &str,
    since: Option<Timestamp>,
    by: &Provenance,
) -> Result<ReadingReport> {
    import_readwise_api_from(space, READWISE_API, token, since, by)
}

/// [`import_readwise_api`] against another server that speaks the same API, such as a test
/// server. `endpoint` is the full URL of the export endpoint.
pub fn import_readwise_api_from(
    space: &mut Space,
    endpoint: &str,
    token: &str,
    since: Option<Timestamp>,
    by: &Provenance,
) -> Result<ReadingReport> {
    let token = token.trim();
    if token.is_empty() {
        return Err(invalid("no Readwise token".into()));
    }
    let updated_after = match since {
        Some(t) => Some(t.to_string()),
        None => space.setting(settings::READWISE_SYNCED)?,
    };
    let started = Timestamp::now();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(USER_AGENT)
        .http_status_as_error(false)
        .build()
        .into();

    let mut report = ReadingReport::default();
    let mut cursor: Option<String> = None;
    loop {
        let page = fetch_page(
            &agent,
            endpoint,
            token,
            updated_after.as_deref(),
            cursor.as_deref(),
        )?;
        let books: Vec<Book> = page.results.into_iter().filter_map(to_book).collect();
        write_books(space, "Readwise", books, &mut report, by)?;
        cursor = page
            .next_page_cursor
            .map(|c| match c {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            })
            .filter(|c| !c.is_empty());
        if cursor.is_none() {
            break;
        }
    }
    if by.kind == ActorKind::Person {
        space.set_setting(settings::READWISE_SYNCED, Some(&started.to_string()), by)?;
    }
    Ok(report)
}

fn invalid(message: String) -> crate::Error {
    crate::Error::Space(knode_format::Error::Invalid(message))
}

/// One page of the export, asked for again after a `429` as long as Readwise says.
fn fetch_page(
    agent: &ureq::Agent,
    endpoint: &str,
    token: &str,
    updated_after: Option<&str>,
    cursor: Option<&str>,
) -> Result<ExportPage> {
    let mut tries = 0;
    loop {
        let mut request = agent
            .get(endpoint)
            .header("Authorization", &format!("Token {token}"))
            .header("Accept", "application/json");
        if let Some(after) = updated_after {
            request = request.query("updatedAfter", after);
        }
        if let Some(cursor) = cursor {
            request = request.query("pageCursor", cursor);
        }
        // ureq's errors name the URL, never the headers, so the token stays out of them.
        let response = request
            .call()
            .map_err(|e| invalid(format!("Readwise could not be reached: {e}")))?;
        let status = response.status().as_u16();
        match status {
            200..=299 => {
                let mut bytes = Vec::new();
                response
                    .into_body()
                    .into_reader()
                    .take(MAX_PAGE)
                    .read_to_end(&mut bytes)
                    .map_err(|e| invalid(format!("Readwise: {e}")))?;
                return serde_json::from_slice(&bytes)
                    .map_err(|e| invalid(format!("Readwise sent an unexpected answer: {e}")));
            }
            429 if tries < RETRIES => {
                tries += 1;
                let wait = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(5 * u64::from(tries))
                    .min(MAX_WAIT);
                std::thread::sleep(Duration::from_secs(wait));
            }
            429 => {
                return Err(invalid(
                    "Readwise asks to slow down: try again in a few minutes".into(),
                ));
            }
            401 | 403 => {
                return Err(invalid(
                    "Readwise refused the token: get a new one at readwise.io/access_token".into(),
                ));
            }
            other => return Err(invalid(format!("Readwise answered with HTTP {other}"))),
        }
    }
}

/// A page of `/api/v2/export/`.
#[derive(Debug, Deserialize)]
struct ExportPage {
    #[serde(default)]
    results: Vec<ApiBook>,
    #[serde(default, rename = "nextPageCursor")]
    next_page_cursor: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ApiBook {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    readable_title: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    unique_url: Option<String>,
    #[serde(default)]
    book_tags: Vec<ApiTag>,
    #[serde(default)]
    highlights: Vec<ApiHighlight>,
}

#[derive(Debug, Deserialize)]
struct ApiHighlight {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    location: Option<serde_json::Value>,
    #[serde(default)]
    location_type: Option<String>,
    #[serde(default)]
    highlighted_at: Option<String>,
    #[serde(default)]
    tags: Vec<ApiTag>,
    #[serde(default)]
    is_discard: bool,
}

#[derive(Debug, Deserialize)]
struct ApiTag {
    #[serde(default)]
    name: String,
}

fn non_empty(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// A book of the API as the CSV export gives it: the readable title (the CSV's "Book Title" and
/// the Markdown export's heading), the author, and its highlights.
fn to_book(api: ApiBook) -> Option<Book> {
    let title = non_empty(api.readable_title).or(non_empty(api.title))?;
    let highlights: Vec<Highlight> = api
        .highlights
        .into_iter()
        .filter(|h| !h.is_discard)
        .filter_map(|h| {
            let text = non_empty(h.text).unwrap_or_default();
            let note = non_empty(h.note);
            if text.is_empty() && note.is_none() {
                return None;
            }
            let location = h.location.and_then(|l| match l {
                serde_json::Value::Number(n) => Some(n.to_string()),
                serde_json::Value::String(s) => Some(s),
                _ => None,
            });
            let kind = match h.location_type.as_deref().unwrap_or("") {
                "time_offset" => "time",
                other => other,
            };
            Some(Highlight {
                text,
                note,
                location: location.and_then(|l| place(kind, &l)),
                chapter: None,
                date: non_empty(h.highlighted_at).map(|d| date(&d)),
                tags: h.tags.into_iter().map(|t| t.name).collect(),
            })
        })
        .collect();
    if highlights.is_empty() {
        return None;
    }
    Some(Book {
        title,
        author: non_empty(api.author),
        url: non_empty(api.source_url).or(non_empty(api.unique_url)),
        category: non_empty(api.category),
        tags: api
            .book_tags
            .into_iter()
            .map(|t| t.name)
            .filter(|t| !t.trim().is_empty())
            .collect(),
        highlights,
    })
}

/// A date of the API, `2026-01-05T21:15:02.123Z`, written as the CSV export's dates are read:
/// `2026-01-05T21:15:02+00:00`. Other text is kept as it is.
fn date(text: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|d| {
            d.with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%M:%S+00:00")
                .to_string()
        })
        .unwrap_or_else(|_| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_read_like_the_csv_export() {
        assert_eq!(
            date("2026-01-05T21:15:02.808Z"),
            "2026-01-05T21:15:02+00:00"
        );
        assert_eq!(
            date("2026-01-05T22:15:02+01:00"),
            "2026-01-05T21:15:02+00:00"
        );
        assert_eq!(date("yesterday"), "yesterday");
    }
}
