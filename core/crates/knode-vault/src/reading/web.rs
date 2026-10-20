//! Web pages, fetched or saved: the readable article as Markdown, with its canonical URL.

use std::fs;
use std::path::Path;

use dom_query::Document as Html;
use knode_format::{Properties, Provenance, Space};

use super::document::{self, Document};
use super::{ReadingReport, keys, markdown, sources};
use crate::{Result, Skipped, io_error};

/// The `User-Agent` Knode sends when it fetches a page.
#[cfg(feature = "fetch")]
pub const USER_AGENT: &str = concat!(
    "Knode/",
    env!("CARGO_PKG_VERSION"),
    " (+https://knode.md; fetches one page when its user asks)"
);

/// The most of a page Knode reads: 20 MB.
#[cfg(feature = "fetch")]
const MAX_PAGE: u64 = 20 * 1024 * 1024;

/// A page fetched with [`fetch`].
#[cfg(feature = "fetch")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedPage {
    /// The URL the page was read from, after redirects.
    pub url: String,
    /// The page.
    pub html: String,
}

/// Fetches one web page, only when the person asks for it: a `GET` with [`USER_AGENT`], at most
/// 20 MB, following redirects. Only `http` and `https` URLs, and only HTML, are read.
#[cfg(feature = "fetch")]
pub fn fetch(url: &str) -> Result<FetchedPage> {
    use std::io::Read;
    use std::time::Duration;
    use ureq::ResponseExt;

    let invalid = |m: String| crate::Error::Space(knode_format::Error::Invalid(m));
    let lower = url.trim().to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(invalid(format!(
            "only http and https pages can be fetched: {url}"
        )));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent(USER_AGENT)
        .build()
        .into();
    let response = agent
        .get(url.trim())
        .header("Accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.1")
        .call()
        .map_err(|e| invalid(format!("{url}: {e}")))?;
    let final_url = response.get_uri().to_string();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html")
        .to_ascii_lowercase();
    if !(content_type.contains("html") || content_type.starts_with("text/plain")) {
        return Err(invalid(format!(
            "{url} is not a web page ({content_type}); download it and import the file"
        )));
    }
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_PAGE)
        .read_to_end(&mut bytes)
        .map_err(|e| invalid(format!("{url}: {e}")))?;
    Ok(FetchedPage {
        url: final_url,
        html: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

/// Fetches a page with [`fetch`] and imports it with [`import_page`]. Call it only when the
/// person asked for this page.
#[cfg(feature = "fetch")]
pub fn import_url(space: &mut Space, url: &str, by: &Provenance) -> Result<ReadingReport> {
    let page = fetch(url)?;
    let mut extra = Properties::new();
    extra.insert(
        keys::FETCHED.into(),
        knode_format::Timestamp::now().to_string(),
    );
    import(space, &page.html, Some(&page.url), None, extra, by)
}

/// Imports a web page saved as an `.html` file. `url` is where it was read from, when known;
/// the page's own canonical URL is used first. The file is attached to the node.
pub fn import_html(
    space: &mut Space,
    path: impl AsRef<Path>,
    url: Option<&str>,
    by: &Provenance,
) -> Result<ReadingReport> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(io_error(path))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "page.html".into());
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let mut extra = Properties::new();
    extra.insert(keys::FILE.into(), name.clone());
    import(space, &html, url, Some((name, bytes)), extra, by)
}

/// Imports a web page from its HTML, as a node of kind `source`, written by `by`.
///
/// - The readable article is found as Firefox's Reader View finds it (a port of Readability)
///   and written as Markdown; without one, the whole body is. Scripts, styles, forms and
///   embedded objects are dropped, never run; only `http`, `https` and `mailto` links are kept.
/// - The URL kept in [`reading.url`](keys::URL) is the page's canonical URL
///   (`<link rel="canonical">`, else `og:url`), else `url`. The author, published date and site
///   name go to [`reading.author`](keys::AUTHOR), [`reading.published`](keys::PUBLISHED) and
///   [`reading.site`](keys::SITE).
/// - The page is found again by its canonical URL, or by its content when it has no URL.
pub fn import_page(
    space: &mut Space,
    html: &str,
    url: Option<&str>,
    by: &Provenance,
) -> Result<ReadingReport> {
    import(space, html, url, None, Properties::new(), by)
}

fn import(
    space: &mut Space,
    html: &str,
    url: Option<&str>,
    file: Option<(String, Vec<u8>)>,
    mut props: Properties,
    by: &Provenance,
) -> Result<ReadingReport> {
    let mut report = ReadingReport::default();
    let page = read(html, url);
    let key = match &page.url {
        Some(url) => format!("url:{url}"),
        None => format!("html:{}", super::sha256(html.as_bytes())),
    };
    if let Some(url) = &page.url {
        props.insert(keys::URL.into(), url.clone());
    }
    for (key, value) in [
        (keys::AUTHOR, &page.author),
        (keys::PUBLISHED, &page.published),
        (keys::SITE, &page.site),
    ] {
        if let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            props.insert(key.into(), value.into());
        }
    }
    let mut body = Vec::new();
    if let Some((name, _)) = &file {
        body.push(format!("Original file: [[{name}]]"));
    }
    if page.body.trim().is_empty() {
        body.push("No readable text was found on this page.".to_string());
    } else {
        body.push(page.body);
    }
    let doc = Document {
        key,
        source: sources::WEB,
        title: page.title,
        body: body.join("\n\n"),
        props,
        file,
        note: None,
    };
    match document::write(space, doc, by) {
        Ok(imported) => report.imported.push(imported),
        Err(e) => report.skipped.push(Skipped {
            path: url.unwrap_or("page").to_string(),
            reason: e.to_string(),
        }),
    }
    Ok(report)
}

/// A page read for its node.
struct Page {
    title: String,
    url: Option<String>,
    author: Option<String>,
    published: Option<String>,
    site: Option<String>,
    body: String,
}

/// Reads the article of a page.
fn read(html: &str, url: Option<&str>) -> Page {
    let base = url.and_then(absolute_http);
    let doc = Html::from(html);
    let canonical = meta_url(&doc, base.as_deref());
    let page_url = canonical.or(base.clone());

    let fallback_title = doc
        .select("title")
        .nodes()
        .first()
        .map(|t| t.text().trim().to_string())
        .filter(|t| !t.is_empty());

    let config = dom_smoothie::Config {
        max_elements_to_parse: 200_000,
        ..Default::default()
    };
    let article = dom_smoothie::Readability::new(html, base.as_deref(), Some(config))
        .ok()
        .and_then(|mut r| r.parse().ok());

    match article {
        Some(article) => {
            let mut body = markdown::from_html(&article.content);
            let title = Some(article.title.trim().to_string())
                .filter(|t| !t.is_empty())
                .or(fallback_title)
                .or(page_url.clone())
                .unwrap_or_else(|| "Web page".into());
            body = drop_leading_title(&body, &title);
            Page {
                title,
                url: page_url,
                author: article.byline,
                published: article.published_time,
                site: article.site_name,
                body,
            }
        }
        None => Page {
            title: fallback_title
                .or(page_url.clone())
                .unwrap_or_else(|| "Web page".into()),
            url: page_url,
            author: None,
            published: None,
            site: None,
            body: markdown::from_html(html),
        },
    }
}

/// The body without a first heading that only repeats the title.
fn drop_leading_title(body: &str, title: &str) -> String {
    let first = body.lines().next().unwrap_or("");
    let heading = first.trim_start_matches('#');
    if first.starts_with('#') && heading.trim() == markdown::escape_inline(title.trim()) {
        body[first.len()..].trim_start().to_string()
    } else {
        body.to_string()
    }
}

/// The canonical URL of a page: `<link rel="canonical">`, else `og:url`, made absolute.
fn meta_url(doc: &Html, base: Option<&str>) -> Option<String> {
    let canonical = doc
        .select("link[rel~=canonical]")
        .nodes()
        .iter()
        .find_map(|n| n.attr("href").map(|h| h.to_string()));
    let og = || {
        doc.select(r#"meta[property="og:url"]"#)
            .nodes()
            .iter()
            .find_map(|n| n.attr("content").map(|c| c.to_string()))
    };
    canonical
        .and_then(|href| resolve(&href, base))
        .or_else(|| og().and_then(|href| resolve(&href, base)))
}

/// `url` when it is an absolute `http` or `https` URL, without its fragment.
fn absolute_http(url: &str) -> Option<String> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    let url = url.split('#').next().unwrap_or(url);
    (url.len() > lower.find("://").unwrap_or(0) + 3).then(|| url.to_string())
}

/// Resolves `href` against `base`: absolute URLs, `//host/...` and `/path` are understood.
fn resolve(href: &str, base: Option<&str>) -> Option<String> {
    let href = href.trim();
    if let Some(url) = absolute_http(href) {
        return Some(url);
    }
    let base = base?;
    let scheme_end = base.find("://")? + 3;
    if let Some(rest) = href.strip_prefix("//") {
        return absolute_http(&format!("{}{rest}", &base[..scheme_end]));
    }
    if href.starts_with('/') {
        let host_end = base[scheme_end..]
            .find('/')
            .map(|i| scheme_end + i)
            .unwrap_or(base.len());
        return absolute_http(&format!("{}{href}", &base[..host_end]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_canonical_urls() {
        let base = Some("https://example.com/a/b?x=1");
        assert_eq!(
            resolve("/c", base).as_deref(),
            Some("https://example.com/c")
        );
        assert_eq!(
            resolve("//cdn.example.com/c", base).as_deref(),
            Some("https://cdn.example.com/c")
        );
        assert_eq!(
            resolve("https://example.org/p#top", base).as_deref(),
            Some("https://example.org/p")
        );
        assert_eq!(resolve("c", base), None);
        assert_eq!(resolve("javascript:alert(1)", base), None);
        assert_eq!(resolve("/c", None), None);
    }
}
