//! What a graph view and a canvas show: the nodes a filter keeps, the links between them and the
//! positions saved in the view (schema version 4).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    Error, LinkKind, NodeId, Position, Provenance, Result, Space, Timestamp, View, ViewGroup,
    view_kinds,
};

/// Which nodes and links a graph shows. A node is shown only when it passes every field that is
/// set; `None` leaves a field out. Stored in a graph view as `settings.filter`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphFilter {
    /// Kinds of node, one of which the node must be.
    pub node_kinds: Option<Vec<String>>,
    /// Tags, one of which the node must have.
    pub tags: Option<Vec<String>>,
    /// People or agents, one of whom created or last changed the node.
    pub actors: Option<Vec<String>>,
    /// The node changed at or after this time.
    pub since: Option<Timestamp>,
    /// The node changed before this time.
    pub until: Option<Timestamp>,
    /// Kinds of link to show; the others are hidden.
    pub link_kinds: Option<Vec<String>>,
}

impl GraphFilter {
    /// Reads a filter from its JSON object, as a graph view stores it. `since` and `until` are
    /// RFC 3339 text or Unix milliseconds. Keys this library does not know are ignored.
    pub fn from_json(value: &Value) -> Result<Self> {
        let Some(object) = value.as_object() else {
            return Err(Error::Invalid("a graph filter is a JSON object".into()));
        };
        let list = |key: &str| -> Result<Option<Vec<String>>> {
            match object.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|v| {
                        v.as_str().map(str::to_string).ok_or_else(|| {
                            Error::Invalid(format!("{key} in a graph filter is a list of text"))
                        })
                    })
                    .collect::<Result<Vec<_>>>()
                    .map(Some),
                Some(_) => Err(Error::Invalid(format!(
                    "{key} in a graph filter is a list of text"
                ))),
            }
        };
        let time = |key: &str| -> Result<Option<Timestamp>> {
            match object.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Number(n)) => n
                    .as_i64()
                    .map(|ms| Some(Timestamp(ms)))
                    .ok_or_else(|| Error::Invalid(format!("{key} in a graph filter is a time"))),
                Some(Value::String(text)) => parse_rfc3339(text)
                    .map(|ms| Some(Timestamp(ms)))
                    .ok_or_else(|| {
                        Error::Invalid(format!(
                            "{key} in a graph filter is RFC 3339, such as 2026-10-20T09:30:00Z"
                        ))
                    }),
                Some(_) => Err(Error::Invalid(format!("{key} in a graph filter is a time"))),
            }
        };
        Ok(GraphFilter {
            node_kinds: list("node_kinds")?,
            tags: list("tags")?,
            actors: list("actors")?,
            since: time("since")?,
            until: time("until")?,
            link_kinds: list("link_kinds")?,
        })
    }

    /// The filter as the JSON object a graph view stores, with times as RFC 3339. Fields left
    /// out are not written.
    pub fn to_json(&self) -> Value {
        let mut object = serde_json::Map::new();
        let lists = [
            ("node_kinds", &self.node_kinds),
            ("tags", &self.tags),
            ("actors", &self.actors),
            ("link_kinds", &self.link_kinds),
        ];
        for (key, value) in lists {
            if let Some(value) = value {
                object.insert(key.into(), Value::from(value.clone()));
            }
        }
        for (key, value) in [("since", self.since), ("until", self.until)] {
            if let Some(time) = value {
                object.insert(key.into(), Value::from(time.to_string()));
            }
        }
        Value::Object(object)
    }

    /// Whether the filter keeps every node and link.
    pub fn is_empty(&self) -> bool {
        *self == GraphFilter::default()
    }

    /// Whether the filter keeps a node.
    pub fn keeps(&self, node: &GraphNode) -> bool {
        let any = |list: &Option<Vec<String>>, f: &dyn Fn(&String) -> bool| {
            list.as_ref().is_none_or(|list| list.iter().any(f))
        };
        any(&self.node_kinds, &|k| *k == node.kind)
            && any(&self.tags, &|t| node.tags.contains(t))
            && any(&self.actors, &|a| {
                *a == node.created_by.actor || *a == node.updated_by.actor
            })
            && self.since.is_none_or(|since| node.updated_at >= since)
            && self.until.is_none_or(|until| node.updated_at < until)
    }

    /// Whether the filter keeps links of a kind.
    pub fn keeps_link(&self, kind: &str) -> bool {
        self.link_kinds
            .as_ref()
            .is_none_or(|kinds| kinds.iter().any(|k| k == kind))
    }
}

/// A node in a graph: what a graph shows of it, without its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    /// The node id.
    pub id: NodeId,
    /// What the node is, such as `note` or `conversation`.
    pub kind: String,
    /// The title.
    pub title: String,
    /// Tags, lower case, sorted.
    pub tags: Vec<String>,
    /// When it was created.
    pub created_at: Timestamp,
    /// When it last changed.
    pub updated_at: Timestamp,
    /// Who created it.
    pub created_by: Provenance,
    /// Who changed it last.
    pub updated_by: Provenance,
}

/// A link between two nodes of a graph or a canvas.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GraphEdge {
    /// The source.
    pub from: NodeId,
    /// The target.
    pub to: NodeId,
    /// The kind, such as `part-of`.
    pub kind: String,
}

/// The nodes and links a graph shows, with the positions saved in its view. See
/// [`Space::graph`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    /// The nodes the filter keeps, newest change first.
    pub nodes: Vec<GraphNode>,
    /// The links between them, of the kinds the filter keeps.
    pub edges: Vec<GraphEdge>,
    /// Where the nodes shown were put in the view; empty without a view.
    pub positions: Vec<Position>,
    /// Every kind of link the space knows, to label the edges.
    pub link_kinds: Vec<LinkKind>,
}

/// A node on a canvas, with what the board shows of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasItem {
    /// Where it is.
    pub position: Position,
    /// What the node is.
    pub kind: String,
    /// The title.
    pub title: String,
    /// The first line of plain text of its body, at most 140 characters.
    pub excerpt: String,
}

/// A canvas as its board shows it: its node, the nodes on it, its groups and the links between
/// its nodes. See [`Space::canvas_board`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasBoard {
    /// The canvas view.
    pub view: View,
    /// The title of the canvas, its node's title.
    pub title: String,
    /// What the canvas is for, its node's body.
    pub body: String,
    /// The nodes on it, back to front.
    pub items: Vec<CanvasItem>,
    /// Its groups.
    pub groups: Vec<ViewGroup>,
    /// The links between its nodes.
    pub edges: Vec<GraphEdge>,
    /// Every kind of link the space knows, to label the edges.
    pub link_kinds: Vec<LinkKind>,
}

impl Space {
    /// The nodes a filter keeps and the links between them, with the positions saved in the
    /// view `view` (if given). Without a filter, the view's own filter applies; with neither,
    /// the whole space. Fails when `view` names no view.
    pub fn graph(&self, view: Option<&str>, filter: Option<&GraphFilter>) -> Result<Graph> {
        let saved = match view {
            Some(id) => Some(
                self.view(id)?
                    .ok_or_else(|| Error::Invalid(format!("no view has the id {id:?}")))?,
            ),
            None => None,
        };
        let filter = match (filter, saved.as_ref().and_then(View::filter)) {
            (Some(filter), _) => filter.clone(),
            (None, Some(stored)) => GraphFilter::from_json(stored)?,
            (None, None) => GraphFilter::default(),
        };
        let mut nodes = Vec::new();
        for summary in self.recent(usize::MAX)? {
            let Some(node) = self.get_node(&summary.id)? else {
                continue;
            };
            let node = GraphNode {
                id: node.id,
                kind: node.kind,
                title: node.title,
                tags: node.tags,
                created_at: node.created_at,
                updated_at: node.updated_at,
                created_by: node.created_by,
                updated_by: node.updated_by,
            };
            if filter.keeps(&node) {
                nodes.push(node);
            }
        }
        let shown: HashSet<NodeId> = nodes.iter().map(|n| n.id).collect();
        let mut edges = Vec::new();
        for node in &nodes {
            for link in self.links_from(&node.id)? {
                if shown.contains(&link.to) && filter.keeps_link(&link.kind) {
                    edges.push(GraphEdge {
                        from: node.id,
                        to: link.to,
                        kind: link.kind,
                    });
                }
            }
        }
        let positions = match view {
            Some(id) => self
                .positions(id)?
                .into_iter()
                .filter(|p| shown.contains(&p.node_id))
                .collect(),
            None => Vec::new(),
        };
        Ok(Graph {
            nodes,
            edges,
            positions,
            link_kinds: self.link_kinds()?,
        })
    }

    /// The canvas whose node is `node`, as its board shows it, or `None` when the node is not a
    /// canvas or is deleted.
    pub fn canvas_board(&self, node: &NodeId) -> Result<Option<CanvasBoard>> {
        let Some(found) = self.canvas(node)? else {
            return Ok(None);
        };
        let Some(canvas) = self.get_node(node)? else {
            return Ok(None);
        };
        debug_assert_eq!(found.kind, view_kinds::CANVAS);
        let mut items = Vec::new();
        for position in self.positions(&found.id)? {
            let Some(node) = self.get_node(&position.node_id)? else {
                continue;
            };
            items.push(CanvasItem {
                position,
                kind: node.kind,
                title: node.title,
                excerpt: excerpt(&node.body),
            });
        }
        let on: HashSet<NodeId> = items.iter().map(|i| i.position.node_id).collect();
        let mut edges = Vec::new();
        for item in &items {
            for link in self.links_from(&item.position.node_id)? {
                if on.contains(&link.to) {
                    edges.push(GraphEdge {
                        from: item.position.node_id,
                        to: link.to,
                        kind: link.kind,
                    });
                }
            }
        }
        let groups = self.groups(&found.id)?;
        Ok(Some(CanvasBoard {
            view: found,
            title: canvas.title,
            body: canvas.body,
            items,
            groups,
            edges,
            link_kinds: self.link_kinds()?,
        }))
    }
}

/// The first line of plain text of a body, without Markdown marks, at most 140 characters.
pub(crate) fn excerpt(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("```"))
        // Table delimiter rows, such as `|---|:--|`.
        .filter(|line| !line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')))
        .map(|line| {
            let line = line.trim_start_matches(['#', '>', '-', '*', '+', ' ']);
            let line = ["[ ] ", "[x] ", "[X] "]
                .iter()
                .find_map(|task| line.strip_prefix(task))
                .unwrap_or(line);
            let line = line.replace("**", "").replace("__", "").replace('`', "");
            let cells: Vec<&str> = line
                .split('|')
                .map(str::trim)
                .filter(|cell| !cell.is_empty())
                .collect();
            cells.join("  ")
        })
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(140).collect())
        .unwrap_or_default()
}

/// Unix milliseconds from RFC 3339 text, such as `2026-10-20T09:30:00.000Z` or
/// `2026-10-20T11:30:00+02:00`, or `None`.
fn parse_rfc3339(text: &str) -> Option<i64> {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' {
        return None;
    }
    if !matches!(bytes[10], b'T' | b't' | b' ') || bytes[16] != b':' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = text.get(range)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    if second > 60 {
        return None;
    }
    let mut rest = &text[19..];
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let padded: String = fraction[..digits]
            .chars()
            .chain("000".chars())
            .take(3)
            .collect();
        millis = padded.parse::<i64>().ok()?;
        rest = &fraction[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ if rest.len() == 6 && rest.as_bytes()[3] == b':' => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hours: i64 = rest.get(1..3)?.parse().ok()?;
            let minutes: i64 = rest.get(4..6)?.parse().ok()?;
            sign * (hours * 60 + minutes) * 60
        }
        _ => return None,
    };
    let days = days_from_civil(year, month as u32, day as u32);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset;
    Some(seconds * 1000 + millis)
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar. Howard Hinnant's
/// algorithm.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips() {
        for ms in [0, 1_792_489_800_123, 951_782_400_000, -86_400_000] {
            let text = Timestamp(ms).to_string();
            assert_eq!(parse_rfc3339(&text), Some(ms), "{text}");
        }
        assert_eq!(
            parse_rfc3339("2026-10-20T11:30:00+02:00"),
            parse_rfc3339("2026-10-20T09:30:00Z")
        );
        assert_eq!(parse_rfc3339("yesterday"), None);
        assert_eq!(parse_rfc3339("2026-13-20T09:30:00Z"), None);
    }

    #[test]
    fn filters_read_text_and_milliseconds() {
        let filter = GraphFilter::from_json(&serde_json::json!({
            "node_kinds": ["decision"],
            "since": "1970-01-01T00:00:01.000Z",
            "until": 5000,
            "other": true
        }))
        .unwrap();
        assert_eq!(filter.node_kinds, Some(vec!["decision".to_string()]));
        assert_eq!(filter.since, Some(Timestamp(1000)));
        assert_eq!(filter.until, Some(Timestamp(5000)));
        assert!(GraphFilter::from_json(&serde_json::json!({ "tags": "x" })).is_err());
        assert!(GraphFilter::from_json(&serde_json::json!([])).is_err());
        assert!(
            GraphFilter::from_json(&serde_json::json!({}))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn excerpts_are_plain_text() {
        assert_eq!(excerpt("```\ncode\n```\n# **Big** `idea`\n"), "code");
        assert_eq!(excerpt("\n\n- [ ] **Ship** it\n"), "Ship it");
        assert_eq!(excerpt("|---|\n| a | b |"), "a  b");
        assert_eq!(excerpt(""), "");
    }
}
