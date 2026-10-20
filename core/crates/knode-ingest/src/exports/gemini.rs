//! Gemini Apps activity from Google Takeout.
//!
//! Requested at takeout.google.com by selecting "My Activity" (and in its options, "Gemini
//! Apps"); it arrives as a `.zip` with `Takeout/My Activity/Gemini Apps/MyActivity.html`, or
//! `MyActivity.json` when JSON was chosen as the format. Folder names follow the language of
//! the account, so the file is found by name in any folder whose name has "Gemini" in it.
//!
//! Google documents My Activity in Takeout but not its file structure. The JSON form is the one
//! all My Activity products share, an array of activities:
//!
//! ```json
//! {"header": "Gemini Apps", "title": "Prompted How do tides work?",
//!  "time": "2025-01-02T10:00:00.123Z", "products": ["Gemini Apps"],
//!  "safeHtmlItem": [{"html": "<p>The answer…</p>"}], "attachedFiles": ["photo.png"]}
//! ```
//!
//! The HTML form has one `outer-cell` block per activity, whose first `content-cell` holds the
//! prompt, a line break, the local date and time, a line break and the answer. Neither form has
//! been verified against a real export of every language: the prompt prefix ("Prompted ") is
//! known in English only, and the HTML date is read in the English format, with its time zone
//! taken as UTC unless it is written as an offset (`GMT+02:00`). Prefer the JSON form, which
//! has exact times.
//!
//! Takeout keeps no conversation ids: every activity is one prompt and its answer. Activities
//! less than [`GAP_MINUTES`] apart are read as one conversation, whose session id is
//! `takeout-` and the time of its first prompt in milliseconds, so a later export of the same
//! activity finds the same nodes.

use chrono::{DateTime, Duration, FixedOffset, NaiveDateTime, TimeZone, Utc};
use serde_json::Value;

use super::html;
use super::{Skipped, file_part};
use crate::model::{Agent, Conversation, Part, Role};
use crate::text::{str_field, timestamp};

/// The longest pause, in minutes, between two prompts of one conversation.
pub(crate) const GAP_MINUTES: i64 = 30;

/// The prefix of the title of a prompt activity, in English.
const PROMPT_PREFIX: &str = "Prompted";

/// One prompt and its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Activity {
    pub(crate) at: DateTime<Utc>,
    pub(crate) prompt: String,
    pub(crate) answer: String,
    pub(crate) files: Vec<String>,
}

/// One activity of `MyActivity.json`. `None` for an activity that is not a prompt to Gemini,
/// such as feedback or a setting changed.
pub(crate) fn json_activity(value: &Value) -> Result<Option<Activity>, String> {
    let is_gemini = |text: &str| text.contains("Gemini") || text.contains("Bard");
    let header = str_field(value, "header").unwrap_or("");
    let products = value.get("products").and_then(Value::as_array);
    let gemini = is_gemini(header)
        || products
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(is_gemini);
    if !gemini {
        return Ok(None);
    }
    let title = str_field(value, "title").unwrap_or("");
    let answer = value
        .get("safeHtmlItem")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| str_field(item, "html"))
        .map(html::to_markdown)
        .collect::<Vec<_>>()
        .join("\n\n");
    let Some(prompt) = prompt_of(title, !answer.is_empty()) else {
        return Ok(None);
    };
    let at = timestamp(value.get("time")).ok_or_else(|| format!("no time for {title:?}"))?;
    let files = value
        .get("attachedFiles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    Ok(Some(Activity {
        at,
        prompt,
        answer,
        files,
    }))
}

/// The prompt of an activity title: the title without its prefix, or the whole title of an
/// activity with an answer whose prefix is in another language.
fn prompt_of(title: &str, has_answer: bool) -> Option<String> {
    let title = title.trim();
    match title.strip_prefix(PROMPT_PREFIX) {
        Some(rest) if rest.starts_with(char::is_whitespace) || rest.is_empty() => {
            Some(rest.trim().to_string())
        }
        _ if has_answer => Some(title.to_string()),
        _ => None,
    }
}

/// The activities of `MyActivity.html`, and the ones that could not be read.
pub(crate) fn html_activities(page: &str) -> (Vec<Activity>, Vec<Skipped>) {
    let mut activities = Vec::new();
    let mut skipped = Vec::new();
    for cell in page.split("class=\"outer-cell").skip(1) {
        let header = cell
            .split_once("mdl-typography--title\">")
            .map(|(_, rest)| html::to_text(rest.split("</p>").next().unwrap_or("")))
            .unwrap_or_default();
        if !(header.contains("Gemini") || header.contains("Bard")) {
            continue;
        }
        let Some((_, content)) = cell.split_once("mdl-typography--body-1\">") else {
            continue;
        };
        let content = content
            .split("<div class=\"content-cell")
            .next()
            .unwrap_or("");
        let content = content.trim_end();
        let content = content.strip_suffix("</div>").unwrap_or(content);
        let mut pieces = content.splitn(3, "<br>");
        let prompt_html = pieces.next().unwrap_or("");
        let date = html::to_text(pieces.next().unwrap_or(""));
        let answer = html::to_markdown(pieces.next().unwrap_or(""));
        let Some(prompt) = prompt_of(&html::to_text(prompt_html), !answer.is_empty()) else {
            continue;
        };
        match parse_html_date(&date) {
            Some(at) => activities.push(Activity {
                at,
                prompt,
                answer,
                files: Vec::new(),
            }),
            None => skipped.push(Skipped {
                item: shorten(&prompt),
                reason: format!("the date {date:?} could not be read"),
            }),
        }
    }
    (activities, skipped)
}

fn shorten(text: &str) -> String {
    crate::text::one_line(text, 60)
}

/// A date as Takeout writes it in English, such as `Jan 2, 2025, 10:00:00 AM CET`. A zone
/// written as an offset (`GMT+02:00`, `UTC-5`) is applied; a named zone is taken as UTC.
fn parse_html_date(text: &str) -> Option<DateTime<Utc>> {
    let text: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let mut words: Vec<&str> = text.split_whitespace().collect();
    let mut offset = FixedOffset::east_opt(0)?;
    if let Some(last) = words.last().copied() {
        if last.chars().any(|c| c.is_ascii_alphabetic()) && !matches!(last, "AM" | "PM") {
            words.pop();
            if let Some(parsed) = parse_offset(last) {
                offset = parsed;
            }
        }
    }
    let local = words.join(" ");
    let formats = [
        "%b %d, %Y, %I:%M:%S %p",
        "%b %d, %Y, %H:%M:%S",
        "%d %b %Y, %H:%M:%S",
        "%B %d, %Y, %I:%M:%S %p",
    ];
    formats.iter().find_map(|format| {
        let naive = NaiveDateTime::parse_from_str(&local, format).ok()?;
        offset
            .from_local_datetime(&naive)
            .single()
            .map(|at| at.with_timezone(&Utc))
    })
}

/// `GMT+02:00`, `UTC-5` or `GMT` as an offset.
fn parse_offset(zone: &str) -> Option<FixedOffset> {
    let rest = zone
        .strip_prefix("GMT")
        .or_else(|| zone.strip_prefix("UTC"))?;
    if rest.is_empty() {
        return FixedOffset::east_opt(0);
    }
    let sign = match rest.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let (hours, minutes) = rest[1..].split_once(':').unwrap_or((&rest[1..], "0"));
    let seconds = hours.parse::<i32>().ok()? * 3600 + minutes.parse::<i32>().ok()? * 60;
    FixedOffset::east_opt(sign * seconds)
}

/// The activities as conversations: sorted by time, a new conversation after every pause of
/// more than [`GAP_MINUTES`].
pub(crate) fn conversations(mut activities: Vec<Activity>) -> Vec<Conversation> {
    activities.sort_by_key(|activity| activity.at);
    activities.dedup();
    let mut conversations: Vec<Conversation> = Vec::new();
    let mut last: Option<DateTime<Utc>> = None;
    for activity in activities {
        let starts = last.is_none_or(|last| activity.at - last > Duration::minutes(GAP_MINUTES));
        if starts {
            conversations.push(Conversation::new(
                Agent::GeminiWeb,
                format!("takeout-{}", activity.at.timestamp_millis()),
            ));
        }
        last = Some(activity.at);
        let Some(conversation) = conversations.last_mut() else {
            continue;
        };
        conversation.saw_time(activity.at);
        let at = Some(activity.at);
        if !activity.prompt.is_empty() {
            conversation.push_part(
                Role::User,
                at,
                Part::Text {
                    text: activity.prompt,
                },
            );
        }
        for file in &activity.files {
            conversation.push_part(Role::User, at, file_part(file));
        }
        if !activity.answer.is_empty() {
            conversation.push_part(
                Role::Assistant,
                at,
                Part::Text {
                    text: activity.answer,
                },
            );
        }
    }
    conversations
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_json_prompts_and_skips_other_activity() {
        let prompt = json!({"header": "Gemini Apps", "title": "Prompted What is <b>?",
            "time": "2025-01-02T10:00:00.000Z", "products": ["Gemini Apps"],
            "safeHtmlItem": [{"html": "<p>A <b>tag</b>.</p>"}], "attachedFiles": ["a.png"]});
        let activity = json_activity(&prompt).unwrap().unwrap();
        assert_eq!(activity.prompt, "What is <b>?");
        assert_eq!(activity.answer, "A **tag**.");
        assert_eq!(activity.files, ["a.png"]);
        let feedback = json!({"header": "Gemini Apps", "title": "Gave feedback: good",
            "time": "2025-01-02T10:00:00Z"});
        assert!(json_activity(&feedback).unwrap().is_none());
        let other =
            json!({"header": "Search", "title": "Searched for x", "time": "2025-01-02T10:00:00Z"});
        assert!(json_activity(&other).unwrap().is_none());
        let no_time = json!({"header": "Gemini Apps", "title": "Prompted hi"});
        assert!(json_activity(&no_time).is_err());
    }

    #[test]
    fn groups_activities_by_pauses() {
        let at = |minutes: i64| {
            DateTime::parse_from_rfc3339("2025-01-02T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
                + Duration::minutes(minutes)
        };
        let activity = |minutes, prompt: &str| Activity {
            at: at(minutes),
            prompt: prompt.into(),
            answer: format!("re {prompt}"),
            files: Vec::new(),
        };
        let conversations = conversations(vec![
            activity(100, "third"),
            activity(0, "first"),
            activity(20, "second"),
        ]);
        assert_eq!(conversations.len(), 2);
        assert_eq!(
            conversations[0].session_id,
            format!("takeout-{}", at(0).timestamp_millis())
        );
        assert_eq!(conversations[0].messages.len(), 4);
        assert_eq!(conversations[0].ended_at, Some(at(20)));
        assert_eq!(conversations[1].messages[0].text(), "third");
    }

    #[test]
    fn reads_html_dates() {
        let utc = |text: &str| parse_html_date(text).map(|at| at.to_rfc3339());
        assert_eq!(
            utc("Jan 2, 2025, 10:00:00\u{202f}AM CET").as_deref(),
            Some("2025-01-02T10:00:00+00:00")
        );
        assert_eq!(
            utc("Jan 2, 2025, 10:00:00 PM GMT+02:00").as_deref(),
            Some("2025-01-02T20:00:00+00:00")
        );
        assert_eq!(utc("not a date"), None);
    }
}
