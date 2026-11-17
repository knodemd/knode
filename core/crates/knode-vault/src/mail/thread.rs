//! Grouping messages into threads, by `Message-ID`, `In-Reply-To` and `References`, and by the
//! subject only for a reply that has neither of the last two: then only with a thread that
//! shares one of its people and whose last message is at most [`WINDOW_DAYS`] before it.

use std::collections::{HashMap, HashSet};

use super::message::Mail;

/// One thread: its messages in the order they were sent.
#[derive(Debug)]
pub(crate) struct Thread {
    pub messages: Vec<Mail>,
    /// What finds it again: the root its first message names.
    pub key: String,
    /// Other keys an earlier import may have found it by: the roots its other messages name.
    pub other_keys: Vec<String>,
    /// The subject, without `Re:` and `Fwd:`, in lower case.
    pub subject: String,
    /// Whether it is only replies without headers that were not matched in this import: such a
    /// thread is looked for by its subject among the threads already in the space.
    pub by_subject: bool,
}

/// How long after the last message of a thread a reply without headers can still join it by
/// its subject.
pub(crate) const WINDOW_DAYS: i64 = 30;

/// Whether a reply without headers sent at `sent` (seconds) by or to `people` can join a thread
/// with these people, first and last message (seconds).
pub(crate) fn fits_by_subject(
    thread_people: &HashSet<String>,
    first: i64,
    last: i64,
    people: &HashSet<String>,
    sent: i64,
) -> bool {
    first <= sent
        && sent <= last + WINDOW_DAYS * 86_400
        && thread_people.iter().any(|p| people.contains(p))
}

/// The addresses of a message.
pub(crate) fn addresses(mail: &Mail) -> HashSet<String> {
    mail.people().map(|p| p.address.clone()).collect()
}

/// The people and dates of a group of messages.
#[derive(Default)]
struct Info {
    people: HashSet<String>,
    first: Option<i64>,
    last: Option<i64>,
}

impl Info {
    fn add(&mut self, mail: &Mail) {
        self.people.extend(addresses(mail));
        if let Some(t) = mail.utc().map(|d| d.timestamp()) {
            self.first = Some(self.first.map_or(t, |f| f.min(t)));
            self.last = Some(self.last.map_or(t, |l| l.max(t)));
        }
    }

    fn merge(mut self, other: Info) -> Info {
        self.people.extend(other.people);
        self.first = match (self.first, other.first) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.last = match (self.last, other.last) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        self
    }
}

/// A subject as threads are matched by it.
pub(crate) fn subject_key(topic: &str) -> String {
    topic
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

struct Sets {
    parent: Vec<usize>,
}

impl Sets {
    fn find(&mut self, i: usize) -> usize {
        let mut root = i;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut i = i;
        while self.parent[i] != root {
            let next = self.parent[i];
            self.parent[i] = root;
            i = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            // The smaller index stays the root, so the result does not depend on the order.
            let (low, high) = if a < b { (a, b) } else { (b, a) };
            self.parent[high] = low;
        }
    }
}

/// Groups messages into threads, each in the order its messages were sent (undated last, in
/// the order they were read), threads in the order of their first message.
pub(crate) fn group(mails: Vec<Mail>) -> Vec<Thread> {
    // One set per message, and one per id named but not read.
    let mut ids: HashMap<String, usize> = HashMap::new();
    for (i, mail) in mails.iter().enumerate() {
        ids.insert(mail.id.clone(), i);
    }
    let mut next = mails.len();
    let mut sets = Sets {
        parent: (0..mails.len()).collect(),
    };
    for (i, mail) in mails.iter().enumerate() {
        for parent in &mail.parents {
            let at = *ids.entry(parent.clone()).or_insert_with(|| {
                next += 1;
                next - 1
            });
            while sets.parent.len() <= at {
                let n = sets.parent.len();
                sets.parent.push(n);
            }
            sets.union(i, at);
        }
    }

    // A reply without headers joins the first thread with its subject that shares one of its
    // people and is recent enough.
    let mut info: HashMap<usize, Info> = HashMap::new();
    for (i, mail) in mails.iter().enumerate() {
        info.entry(sets.find(i)).or_default().add(mail);
    }
    let mut seen: HashMap<String, Vec<usize>> = HashMap::new();
    let mut order: Vec<usize> = (0..mails.len()).collect();
    order.sort_by_key(|&i| sort_key(&mails[i]));
    for &i in &order {
        let mail = &mails[i];
        let key = subject_key(&mail.topic);
        if key.is_empty() {
            continue;
        }
        if let (false, true, Some(sent)) = (
            mail.has_parents(),
            mail.is_reply(),
            mail.utc().map(|d| d.timestamp()),
        ) {
            let mine = sets.find(i);
            let people = addresses(mail);
            let candidates: Vec<usize> = seen
                .get(&key)
                .map(|seen| seen.iter().map(|&j| sets.find(j)).collect())
                .unwrap_or_default();
            let target = candidates.into_iter().find(|&root| {
                root != mine
                    && info.get(&root).is_some_and(|g| match (g.first, g.last) {
                        (Some(first), Some(last)) => {
                            fits_by_subject(&g.people, first, last, &people, sent)
                        }
                        _ => false,
                    })
            });
            if let Some(root) = target {
                let a = info.remove(&mine).unwrap_or_default();
                let b = info.remove(&root).unwrap_or_default();
                sets.union(mine, root);
                info.insert(sets.find(mine), a.merge(b));
            }
        }
        seen.entry(key).or_default().push(i);
    }

    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..mails.len() {
        groups.entry(sets.find(i)).or_default().push(i);
    }
    let mut slots: Vec<Option<Mail>> = mails.into_iter().map(Some).collect();
    let mut threads: Vec<Thread> = groups
        .into_values()
        .map(|members| {
            let mut messages: Vec<Mail> = members
                .into_iter()
                .filter_map(|i| slots[i].take())
                .collect();
            messages.sort_by_key(sort_key);
            let first = &messages[0];
            let key = first.root().to_string();
            let mut other_keys = Vec::new();
            let mut seen: HashSet<&str> = HashSet::from([key.as_str()]);
            for mail in &messages {
                let named = mail.parents.iter().map(String::as_str);
                for candidate in [mail.root(), mail.id.as_str()].into_iter().chain(named) {
                    if seen.insert(candidate) {
                        other_keys.push(candidate.to_string());
                    }
                }
            }
            let subject = subject_key(&first.topic);
            let by_subject =
                !subject.is_empty() && messages.iter().all(|m| !m.has_parents() && m.is_reply());
            Thread {
                messages,
                key,
                other_keys,
                subject,
                by_subject,
            }
        })
        .collect();
    threads.sort_by_key(|t| sort_key(&t.messages[0]));
    threads
}

/// Sent first, then read first; undated messages after dated ones.
fn sort_key(mail: &Mail) -> (bool, i64, usize) {
    match mail.utc() {
        Some(date) => (false, date.timestamp(), mail.order),
        None => (true, 0, mail.order),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::DateTime;

    use super::*;
    use crate::mail::message::{Origin, Person};

    fn mail(id: &str, parents: &[&str], subject: &str, minute: u32) -> Mail {
        sent(
            id,
            parents,
            subject,
            &format!("2026-03-01T10:{minute:02}:00Z"),
            "ada@example.com",
        )
    }

    fn sent(id: &str, parents: &[&str], subject: &str, date: &str, from: &str) -> Mail {
        let topic = subject
            .trim_start_matches("Re: ")
            .trim_start_matches("Fwd: ")
            .to_string();
        Mail {
            id: id.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            subject: subject.into(),
            topic,
            from: vec![Person {
                name: None,
                address: from.into(),
            }],
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            date: DateTime::parse_from_rfc3339(date).ok(),
            body: Some(String::new()),
            attachments: Vec::new(),
            folders: BTreeSet::new(),
            list: false,
            origin: Origin::Elsewhere,
            order: date.len() + id.len(),
        }
    }

    #[test]
    fn threads_by_headers_then_subject() {
        let threads = group(vec![
            mail("c", &["a", "b"], "Re: Plans", 3),
            mail("a", &[], "Plans", 1),
            mail("x", &[], "Other", 2),
            // Its parent was not read: it still joins through the root it names.
            mail("d", &["a", "missing"], "Re: Plans", 4),
            // No headers: joins by subject.
            mail("e", &[], "Re: Plans", 5),
            // Not a reply: a thread of its own, though the subject is the same.
            mail("f", &[], "Plans", 6),
        ]);
        let ids: Vec<Vec<&str>> = threads
            .iter()
            .map(|t| t.messages.iter().map(|m| m.id.as_str()).collect())
            .collect();
        assert_eq!(ids, [vec!["a", "c", "d", "e"], vec!["x"], vec!["f"]]);
        assert_eq!(threads[0].key, "a");
        assert!(!threads[0].by_subject);
    }

    #[test]
    fn a_lone_reply_without_headers_is_looked_for_by_subject() {
        let threads = group(vec![mail("e", &[], "Re: Lunch", 1)]);
        assert!(threads[0].by_subject);
        assert_eq!(threads[0].subject, "lunch");
    }

    #[test]
    fn the_subject_joins_only_people_who_share_a_recent_thread() {
        let threads = group(vec![
            sent(
                "q",
                &[],
                "Question",
                "2020-05-01T10:00:00Z",
                "ada@example.com",
            ),
            // A stranger, years later, with the same subject.
            sent(
                "r",
                &[],
                "Re: question",
                "2026-05-01T10:00:00Z",
                "zoe@example.net",
            ),
            // Someone of the thread, but long after it.
            sent(
                "s",
                &[],
                "Re: Question",
                "2020-08-01T10:00:00Z",
                "ada@example.com",
            ),
            // Someone of the thread, within the window.
            sent(
                "t",
                &[],
                "Re: Question",
                "2020-05-20T10:00:00Z",
                "ada@example.com",
            ),
        ]);
        let ids: Vec<Vec<&str>> = threads
            .iter()
            .map(|t| t.messages.iter().map(|m| m.id.as_str()).collect())
            .collect();
        assert_eq!(ids, [vec!["q", "t"], vec!["s"], vec!["r"]]);
    }
}
