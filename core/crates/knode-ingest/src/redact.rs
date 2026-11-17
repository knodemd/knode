//! Replacing secrets that look obvious (API keys, tokens, private keys and passwords in common
//! forms) in conversation text, so a key pasted into an agent session is not stored with it.
//!
//! The patterns are deliberately narrow: they catch well-known key formats and `name = value`
//! assignments whose name says it holds a secret. Text that matches none of them is kept as
//! it is, so this lowers the risk of storing a credential but cannot rule it out.

use crate::model::{Conversation, Part};
use regex::{Captures, Regex};
use std::sync::LazyLock;

/// What a secret is replaced with.
pub const REDACTED: &str = "[redacted]";

/// Patterns whose whole match is a secret.
static WHOLE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // PEM private keys, from the BEGIN line to the END line.
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        // AWS access key ids.
        r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        // GitHub tokens.
        r"\bgh[pousr]_[A-Za-z0-9]{36,}\b",
        r"\bgithub_pat_[A-Za-z0-9_]{22,}\b",
        // Anthropic, OpenAI and similar `sk-` keys.
        r"\bsk-[A-Za-z0-9_\-]{20,}",
        // Stripe keys.
        r"\b(?:sk|rk|pk)_(?:live|test)_[A-Za-z0-9]{16,}\b",
        // Slack tokens: bot, user, app, refresh and the rest (`xoxb-`, `xoxp-`, `xoxa-`,
        // `xoxr-`, `xoxe-`, `xoxe.xoxp-`...), and app-level tokens.
        r"\bxox[abeposr](?:\.xox[a-z])?-[A-Za-z0-9\-]{10,}",
        r"\bxapp-[0-9]-[A-Za-z0-9\-]{10,}",
        // Google API keys.
        r"\bAIza[0-9A-Za-z_\-]{35}",
        // JSON Web Tokens.
        r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid secret pattern"))
    .collect()
});

/// Patterns whose `secret` group is a secret and the rest is kept, such as the name in
/// `API_KEY=...`.
static VALUES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // `Authorization: Bearer ...`.
        r"(?i)\bbearer\s+(?P<secret>[A-Za-z0-9._~+/=\-]{16,})",
        // `password = ...`, `API_KEY: "..."` and the like.
        r#"(?i)\b[A-Za-z0-9_.\-]*(?:password|passwd|pwd|secret|token|api[_\-]?key|access[_\-]?key|private[_\-]?key)[A-Za-z0-9_.\-]*["']?\s*[:=]\s*["']?(?P<secret>[^\s"',;]{8,})"#,
        // `--password ...`, `--api-key=...` and the like on a command line.
        r"(?i)--[a-z0-9\-]*(?:password|token|secret|api-key)[= ](?P<secret>[^\s\-][^\s]{7,})",
        // The password in `scheme://user:password@host`.
        r"[A-Za-z][A-Za-z0-9+.\-]*://[^\s/:@]+:(?P<secret>[^\s/@]+)@",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid secret pattern"))
    .collect()
});

/// The text with each obvious secret replaced by [`REDACTED`], and how many were replaced.
pub fn redact_secrets(text: &str) -> (String, usize) {
    redact(text, false)
}

/// Like [`redact_secrets`], for text people wrote to each other, such as chat messages, where
/// "Secret: everything went fine" is a sentence: only what is shaped like a credential is
/// replaced. Known key formats always are; after a name such as `password:` or `token=`, or
/// `Bearer`, only a value of at least 16 characters with both letters and digits; and the
/// password in a URL.
pub(crate) fn redact_tokens(text: &str) -> (String, usize) {
    redact(text, true)
}

/// The value rules, in [`VALUES`], that take a value whatever its shape in [`redact_tokens`]:
/// the password in a URL, the last one.
fn any_value(rule: usize) -> bool {
    rule == VALUES.len() - 1
}

/// Whether a value after a name looks generated rather than written: long, with letters and
/// digits.
fn token_shaped(value: &str) -> bool {
    value.chars().count() >= 16
        && value.chars().any(|c| c.is_ascii_alphabetic())
        && value.chars().any(|c| c.is_ascii_digit())
}

fn redact(text: &str, tokens_only: bool) -> (String, usize) {
    let mut count = 0;
    let mut out = text.to_string();
    for pattern in WHOLE.iter() {
        if pattern.is_match(&out) {
            out = pattern
                .replace_all(&out, |_: &Captures| {
                    count += 1;
                    REDACTED
                })
                .into_owned();
        }
    }
    for (rule, pattern) in VALUES.iter().enumerate() {
        if pattern.is_match(&out) {
            out = pattern
                .replace_all(&out, |caps: &Captures| {
                    let whole = caps.get(0).expect("match");
                    let secret = caps.name("secret").expect("secret group");
                    if secret.as_str().contains(REDACTED)
                        || (tokens_only && !any_value(rule) && !token_shaped(secret.as_str()))
                    {
                        return whole.as_str().to_string();
                    }
                    count += 1;
                    let start = secret.start() - whole.start();
                    let end = secret.end() - whole.start();
                    format!(
                        "{}{REDACTED}{}",
                        &whole.as_str()[..start],
                        &whole.as_str()[end..]
                    )
                })
                .into_owned();
        }
    }
    (out, count)
}

/// How many obvious secrets [`redact_conversation`] would replace in a conversation.
pub fn count_secrets(conversation: &Conversation) -> usize {
    redact_conversation(&mut conversation.clone())
}

/// Replaces the obvious secrets in a conversation's title, text and tool calls, and returns
/// how many were replaced.
pub fn redact_conversation(conversation: &mut Conversation) -> usize {
    let mut count = 0;
    let mut redact = |text: &mut String| {
        let (redacted, n) = redact_secrets(text);
        if n > 0 {
            *text = redacted;
            count += n;
        }
    };
    if let Some(title) = &mut conversation.recorded_title {
        redact(&mut title.text);
    }
    for message in &mut conversation.messages {
        for part in &mut message.parts {
            match part {
                Part::Text { text } => redact(text),
                Part::Tool(call) => redact(&mut call.summary),
                Part::Image => {}
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every value below is made up for the test; none is a real credential.
    #[test]
    fn known_key_formats_are_replaced() {
        let samples = [
            "AKIAABCDEFGHIJKLMNOP",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
            "sk_live_abcdefghijklmnop1234",
            "xoxb-1234567890-abcdefghij",
            "AIzaSyA-abcdefghijklmnopqrstuvwxyz01234",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.abcdefghijklmnop",
        ];
        for sample in samples {
            let (out, n) = redact_secrets(&format!("use {sample} here"));
            assert_eq!(n, 1, "{sample}");
            assert_eq!(out, format!("use {REDACTED} here"), "{sample}");
        }
    }

    #[test]
    fn words_after_a_name_are_kept_in_what_people_wrote() {
        for kept in [
            "Secret: everything went fine",
            "The token: a coffee voucher",
            "password = the usual one",
            "Bearer of good news, everyone",
            "--password reset tomorrow please",
        ] {
            assert_eq!(redact_tokens(kept), (kept.to_string(), 0), "{kept}");
        }
        // The same rule still catches them in agent sessions.
        assert_eq!(redact_secrets("Secret: everything went fine").1, 1);
        // Shaped like credentials, they go in chats too.
        let key = ["sk-", "ant-api03-", "abcdefghijklmnopqrstuvwxyz"].concat();
        for (text, expected) in [
            (format!("use {key} now"), format!("use {REDACTED} now")),
            (
                "api_key=a1b2c3d4e5f6g7h8i9j0".to_string(),
                format!("api_key={REDACTED}"),
            ),
            (
                "Authorization: Bearer abcdEFGH1234ijklMNOP".to_string(),
                format!("Authorization: Bearer {REDACTED}"),
            ),
            (
                "https://ada:hunter2@example.com/x".to_string(),
                format!("https://ada:{REDACTED}@example.com/x"),
            ),
        ] {
            assert_eq!(redact_tokens(&text).0, expected, "{text}");
        }
        let slack = format!("xox{}-1-1234567890-abcdefghij", "b");
        assert_eq!(redact_tokens(&slack), (REDACTED.to_string(), 1));
    }

    #[test]
    fn every_kind_of_slack_token_is_replaced() {
        // Put together here, so the source holds no whole token.
        for kind in ["b", "p", "a", "r", "e", "e.xoxp"] {
            let sample = format!("xox{kind}-1-1234567890-abcdefghij");
            let (out, n) = redact_secrets(&format!("use {sample}. here"));
            assert_eq!(n, 1, "{sample}");
            assert_eq!(out, format!("use {REDACTED}. here"), "{sample}");
        }
        let app = format!("x{}-1-A0123456789-abcdefghij", "app");
        assert_eq!(redact_secrets(&app), (REDACTED.to_string(), 1));
    }

    #[test]
    fn private_keys_are_replaced_whole() {
        let text = "key:\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAexample\nBBBBexample\n-----END OPENSSH PRIVATE KEY-----\ndone";
        let (out, n) = redact_secrets(text);
        assert_eq!(n, 1);
        assert_eq!(out, format!("key:\n{REDACTED}\ndone"));
    }

    #[test]
    fn assigned_passwords_and_tokens_keep_their_name() {
        let (out, n) = redact_secrets(
            "export DB_PASSWORD=hunter2hunter2\napi_key: \"abcd1234efgh\"\ncurl -H 'Authorization: Bearer abcdefghijklmnop1234' postgres://me:topsecret99@db.example\ntool --api-key abcdefgh1234",
        );
        assert_eq!(n, 5, "{out}");
        assert!(out.contains(&format!("DB_PASSWORD={REDACTED}")), "{out}");
        assert!(out.contains(&format!("api_key: \"{REDACTED}\"")), "{out}");
        assert!(out.contains(&format!("Bearer {REDACTED}")), "{out}");
        assert!(
            out.contains(&format!("postgres://me:{REDACTED}@db.example")),
            "{out}"
        );
        assert!(out.contains(&format!("--api-key {REDACTED}")), "{out}");
    }

    #[test]
    fn ordinary_text_is_kept() {
        let text = "The token count was high. Reset your password reminders in Settings. See https://example.com/docs.";
        assert_eq!(redact_secrets(text), (text.to_string(), 0));
    }

    #[test]
    fn a_secret_is_counted_once() {
        let (out, n) = redact_secrets("ANTHROPIC_API_KEY=sk-ant-abcdefghijklmnopqrstuvwxyz");
        assert_eq!(n, 1);
        assert_eq!(out, format!("ANTHROPIC_API_KEY={REDACTED}"));
    }
}
