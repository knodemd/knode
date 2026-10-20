//! Link kinds and backlinks (schema version 4).

use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};

use crate::space::{add_change_with_detail, check_name, provenance, provenance_id};
use crate::{Error, Link, NodeId, Provenance, Result, Space, Timestamp, actions, link_kinds};

/// How a link reads from its two nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkDirection {
    /// The link reads one way from its source (its label) and another from its target (its
    /// inverse label), such as *part of* and *has part*.
    #[default]
    Directed,
    /// The link reads the same from both nodes, such as *related to*.
    Symmetric,
}

impl LinkDirection {
    /// The value stored in `link_kinds.direction`.
    pub fn as_str(&self) -> &'static str {
        match self {
            LinkDirection::Directed => "directed",
            LinkDirection::Symmetric => "symmetric",
        }
    }

    pub(crate) fn parse(text: &str) -> Self {
        match text {
            "symmetric" => LinkDirection::Symmetric,
            _ => LinkDirection::Directed,
        }
    }
}

/// A kind of link: built in, or defined by the space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkKind {
    /// The name, as stored on each link, such as `part-of` or `depends-on`.
    pub kind: String,
    /// How the link reads from its source, such as `depends on`.
    pub label: String,
    /// How it reads from its target, such as `needed by`. Empty for none: the target then shows
    /// the kind's name. Ignored for a symmetric kind.
    pub inverse_label: String,
    /// Whether it reads the same from both nodes.
    pub direction: LinkDirection,
    /// What the kind means, for people and agents choosing one. Empty for none.
    pub description: String,
    /// Whether the format defines it. A built-in kind cannot be changed or removed.
    pub built_in: bool,
    /// When the space defined it; `None` for a built-in kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Timestamp>,
    /// Who defined it; `None` for a built-in kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Provenance>,
    /// When it last changed; `None` for a built-in kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<Timestamp>,
    /// Who changed it last; `None` for a built-in kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Provenance>,
}

impl LinkKind {
    /// How a link of this kind reads from one of its nodes: the label from the source, the
    /// inverse label from the target (the kind's name when it has none), and the label from
    /// both for a symmetric kind.
    pub fn label_from(&self, outgoing: bool) -> &str {
        if outgoing || self.direction == LinkDirection::Symmetric {
            &self.label
        } else if self.inverse_label.is_empty() {
            &self.kind
        } else {
            &self.inverse_label
        }
    }

    /// A kind no one defined: shown by its name, read one way.
    fn unknown(kind: &str) -> Self {
        LinkKind {
            kind: kind.to_string(),
            label: kind.to_string(),
            inverse_label: String::new(),
            direction: LinkDirection::Directed,
            description: String::new(),
            built_in: false,
            created_at: None,
            created_by: None,
            updated_at: None,
            updated_by: None,
        }
    }
}

/// A link kind to define or change with [`Space::set_link_kind`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkKindSpec {
    /// The name: lower case letters, digits and `-`, starting with a letter, at most 32
    /// characters.
    pub kind: String,
    /// How the link reads from its source: 1 to 64 characters, one line.
    pub label: String,
    /// How it reads from its target: at most 64 characters, one line. Empty for none.
    #[serde(default)]
    pub inverse_label: String,
    /// Whether it reads the same from both nodes. Cannot change while links have the kind.
    #[serde(default)]
    pub direction: LinkDirection,
    /// What the kind means. Empty for none.
    #[serde(default)]
    pub description: String,
}

impl LinkKindSpec {
    /// A directed kind with a label and an inverse label.
    pub fn directed(
        kind: impl Into<String>,
        label: impl Into<String>,
        inverse_label: impl Into<String>,
    ) -> Self {
        LinkKindSpec {
            kind: kind.into(),
            label: label.into(),
            inverse_label: inverse_label.into(),
            direction: LinkDirection::Directed,
            description: String::new(),
        }
    }

    /// A symmetric kind, which reads the same from both nodes.
    pub fn symmetric(kind: impl Into<String>, label: impl Into<String>) -> Self {
        LinkKindSpec {
            kind: kind.into(),
            label: label.into(),
            inverse_label: String::new(),
            direction: LinkDirection::Symmetric,
            description: String::new(),
        }
    }

    /// Sets what the kind means.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

/// A link as one of its nodes shows it: going out or coming in, with the label it reads with
/// from that node. See [`Space::node_links`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLink {
    /// The link as stored.
    pub link: Link,
    /// `true` when the node is the link's source (a link going out, or a link to itself),
    /// `false` for a backlink.
    pub outgoing: bool,
    /// The node at the other end.
    pub node: NodeId,
    /// The title of the node at the other end.
    pub title: String,
    /// How the link reads from the node, such as `part of` going out or `has part` coming in.
    pub label: String,
    /// The direction of the link's kind.
    pub direction: LinkDirection,
}

/// The built-in kinds: name, direction, label, inverse label and description.
const BUILT_IN: &[(&str, LinkDirection, &str, &str, &str)] = &[
    (
        link_kinds::MENTIONS,
        LinkDirection::Directed,
        "mentions",
        "mentioned in",
        "The source's body has a [[link]] to the target. Written only from the body.",
    ),
    (
        link_kinds::RELATED,
        LinkDirection::Symmetric,
        "related to",
        "related to",
        "The nodes are related. The default for links made by hand.",
    ),
    (
        link_kinds::PART_OF,
        LinkDirection::Directed,
        "part of",
        "has part",
        "The source is part of the target, such as a note of a project.",
    ),
    (
        link_kinds::DERIVED_FROM,
        LinkDirection::Directed,
        "derived from",
        "source of",
        "The source was made from the target, such as a decision from a conversation.",
    ),
    (
        link_kinds::ANSWERS,
        LinkDirection::Directed,
        "answers",
        "answered by",
        "The source answers a question the target asks.",
    ),
    (
        link_kinds::CONTRADICTS,
        LinkDirection::Directed,
        "contradicts",
        "contradicted by",
        "The source says the opposite of the target.",
    ),
];

fn built_in(kind: &str) -> Option<LinkKind> {
    BUILT_IN.iter().find(|b| b.0 == kind).map(
        |&(kind, direction, label, inverse_label, description)| LinkKind {
            kind: kind.to_string(),
            label: label.to_string(),
            inverse_label: inverse_label.to_string(),
            direction,
            description: description.to_string(),
            built_in: true,
            created_at: None,
            created_by: None,
            updated_at: None,
            updated_by: None,
        },
    )
}

const KIND_COLUMNS: &str = "k.kind, k.label, k.inverse_label, k.direction, k.description, \
     k.created_at, c.actor_kind, c.actor, c.session, c.source, \
     k.updated_at, u.actor_kind, u.actor, u.session, u.source";

fn kind_row(row: &Row<'_>) -> rusqlite::Result<LinkKind> {
    Ok(LinkKind {
        kind: row.get(0)?,
        label: row.get(1)?,
        inverse_label: row.get(2)?,
        direction: LinkDirection::parse(&row.get::<_, String>(3)?),
        description: row.get(4)?,
        built_in: false,
        created_at: Some(Timestamp(row.get(5)?)),
        created_by: Some(provenance(row, 6)?),
        updated_at: Some(Timestamp(row.get(10)?)),
        updated_by: Some(provenance(row, 11)?),
    })
}

fn query_kinds<P: rusqlite::Params>(
    conn: &Connection,
    filter: &str,
    params: P,
) -> Result<Vec<LinkKind>> {
    let sql = format!(
        "SELECT {KIND_COLUMNS} FROM link_kinds k
         JOIN provenance c ON c.id = k.created_by
         JOIN provenance u ON u.id = k.updated_by
         {filter}"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params, kind_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A kind as a reader shows it: built in (with the labels of a row the space kept for it),
/// defined by the space, or `None` when it is neither.
pub(crate) fn find_kind(conn: &Connection, kind: &str) -> Result<Option<LinkKind>> {
    let row = query_kinds(conn, "WHERE k.kind = ?1", [kind])?.pop();
    Ok(match (built_in(kind), row) {
        (Some(mut built), Some(row)) => {
            // A kind made built in later keeps the labels the space gave it.
            built.label = row.label;
            built.inverse_label = row.inverse_label;
            built.description = row.description;
            Some(built)
        }
        (Some(built), None) => Some(built),
        (None, row) => row,
    })
}

/// Checks a label: trimmed, one line, at most `max` characters, and not empty when `required`.
fn check_label(what: &str, text: &str, required: bool, max: usize) -> Result<String> {
    let text = text.trim();
    if required && text.is_empty() {
        return Err(Error::Invalid(format!("a link kind needs a {what}")));
    }
    if text.chars().count() > max {
        return Err(Error::Invalid(format!(
            "the {what} of a link kind is at most {max} characters"
        )));
    }
    if text.chars().any(char::is_control) {
        return Err(Error::Invalid(format!(
            "the {what} of a link kind is one line of text"
        )));
    }
    Ok(text.to_string())
}

fn links_with_kind(conn: &Connection, kind: &str) -> Result<u64> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM links WHERE kind = ?1",
        [kind],
        |row| row.get(0),
    )?;
    Ok(count as u64)
}

impl Space {
    /// Every kind of link a link can be made with: the built-in kinds first, in the order the
    /// specification lists them, then the kinds the space defines, by name.
    pub fn link_kinds(&self) -> Result<Vec<LinkKind>> {
        let mut kinds = Vec::new();
        for (kind, ..) in BUILT_IN {
            kinds.extend(find_kind(&self.conn, kind)?);
        }
        let defined = query_kinds(&self.conn, "ORDER BY k.kind", [])?;
        kinds.extend(
            defined
                .into_iter()
                .filter(|k| !link_kinds::BUILT_IN.contains(&k.kind.as_str())),
        );
        Ok(kinds)
    }

    /// A kind of link, built in or defined by the space, or `None` if it is neither.
    pub fn link_kind(&self, kind: &str) -> Result<Option<LinkKind>> {
        find_kind(&self.conn, kind.trim())
    }

    /// Defines a kind of link, or changes its labels and description, records the change and
    /// returns the kind. Changing nothing writes nothing.
    ///
    /// Built-in kinds cannot be defined or changed. The direction of a kind cannot change while
    /// links have it ([`Error::LinkKindInUse`]). Recorded as a `link_kind.set` change, which is
    /// not undone through [`undo`](Self::undo): change the kind again instead.
    pub fn set_link_kind(&mut self, spec: &LinkKindSpec, by: &Provenance) -> Result<LinkKind> {
        let kind = check_name("link kind", spec.kind.trim())?;
        if built_in(&kind).is_some() {
            return Err(Error::Invalid(format!(
                "{kind} is a built-in link kind and cannot be changed"
            )));
        }
        let label = check_label("label", &spec.label, true, 64)?;
        let inverse_label = match spec.direction {
            LinkDirection::Symmetric => String::new(),
            LinkDirection::Directed => {
                check_label("inverse label", &spec.inverse_label, false, 64)?
            }
        };
        let description = spec.description.trim().to_string();
        let current = query_kinds(&self.conn, "WHERE k.kind = ?1", [&kind])?.pop();
        if let Some(current) = &current {
            if current.label == label
                && current.inverse_label == inverse_label
                && current.direction == spec.direction
                && current.description == description
            {
                return Ok(current.clone());
            }
            if current.direction != spec.direction {
                let links = links_with_kind(&self.conn, &kind)?;
                if links > 0 {
                    return Err(Error::LinkKindInUse { kind, links });
                }
            }
        }
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        tx.execute(
            "INSERT INTO link_kinds (kind, label, inverse_label, direction, description,
                                     created_at, created_by, updated_at, updated_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?6, ?7)
             ON CONFLICT (kind) DO UPDATE SET
                 label = excluded.label, inverse_label = excluded.inverse_label,
                 direction = excluded.direction, description = excluded.description,
                 updated_at = excluded.updated_at, updated_by = excluded.updated_by",
            params![
                kind,
                label,
                inverse_label,
                spec.direction.as_str(),
                description,
                now.0,
                prov
            ],
        )?;
        let detail = serde_json::json!({
            "label": label,
            "inverse_label": inverse_label,
            "direction": spec.direction.as_str(),
            "description": description,
        })
        .to_string();
        add_change_with_detail(&tx, now, prov, actions::LINK_KIND_SET, &kind, None, &detail)?;
        tx.commit()?;
        find_kind(&self.conn, &kind)?
            .ok_or_else(|| Error::Invalid("the link kind could not be read back".into()))
    }

    /// Removes a kind the space defined, records the change and returns whether there was one.
    ///
    /// Fails with [`Error::LinkKindInUse`] while links have the kind: remove them first. Built-in
    /// kinds cannot be removed. Links removed earlier keep the kind's name in their history.
    pub fn remove_link_kind(&mut self, kind: &str, by: &Provenance) -> Result<bool> {
        let kind = kind.trim();
        if built_in(kind).is_some() {
            return Err(Error::Invalid(format!(
                "{kind} is a built-in link kind and cannot be removed"
            )));
        }
        let Some(current) = query_kinds(&self.conn, "WHERE k.kind = ?1", [kind])?.pop() else {
            return Ok(false);
        };
        let links = links_with_kind(&self.conn, kind)?;
        if links > 0 {
            return Err(Error::LinkKindInUse {
                kind: kind.to_string(),
                links,
            });
        }
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        tx.execute("DELETE FROM link_kinds WHERE kind = ?1", [kind])?;
        let detail = serde_json::json!({
            "label": current.label,
            "inverse_label": current.inverse_label,
            "direction": current.direction.as_str(),
            "description": current.description,
        })
        .to_string();
        add_change_with_detail(
            &tx,
            now,
            prov,
            actions::LINK_KIND_REMOVE,
            kind,
            None,
            &detail,
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// The links of a node as a reader shows them, each with the label it reads with from the
    /// node: first the links going out, by kind and title, then the backlinks, by kind and
    /// title.
    ///
    /// Links going out read with their kind's label and backlinks with its inverse label; a
    /// symmetric kind reads with its label both ways, and two nodes linked both ways with it are
    /// shown once. A link from the node to itself is shown once, going out. Links of a kind the
    /// space does not know read with the kind's name.
    pub fn node_links(&self, id: &NodeId) -> Result<Vec<NodeLink>> {
        let mut kinds: Vec<LinkKind> = Vec::new();
        let mut kind_of = |conn: &Connection, name: &str| -> Result<LinkKind> {
            if let Some(found) = kinds.iter().find(|k| k.kind == name) {
                return Ok(found.clone());
            }
            let found = find_kind(conn, name)?.unwrap_or_else(|| LinkKind::unknown(name));
            kinds.push(found.clone());
            Ok(found)
        };
        let mut out = Vec::new();
        let mut back = Vec::new();
        for link in self.query_links("l.source_id = ?1", [id.to_string()])? {
            let kind = kind_of(&self.conn, &link.kind)?;
            out.push(NodeLink {
                node: link.to,
                title: link.to_title.clone(),
                label: kind.label_from(true).to_string(),
                direction: kind.direction,
                outgoing: true,
                link,
            });
        }
        for link in self.query_links("l.target_id = ?1 AND l.source_id <> ?1", [id.to_string()])? {
            let kind = kind_of(&self.conn, &link.kind)?;
            let shown = kind.direction == LinkDirection::Symmetric
                && out
                    .iter()
                    .any(|o| o.link.kind == link.kind && o.node == link.from);
            if shown {
                continue;
            }
            back.push(NodeLink {
                node: link.from,
                title: link.from_title.clone(),
                label: kind.label_from(false).to_string(),
                direction: kind.direction,
                outgoing: false,
                link,
            });
        }
        let order = |a: &NodeLink, b: &NodeLink| {
            (&a.link.kind, knode_md::title_key(&a.title), a.node).cmp(&(
                &b.link.kind,
                knode_md::title_key(&b.title),
                b.node,
            ))
        };
        out.sort_by(order);
        back.sort_by(order);
        out.extend(back);
        Ok(out)
    }

    /// The backlinks of a node, of every kind including `mentions`, each with the label it reads
    /// with from the node (the kind's inverse label), by kind and title. A link from the node to
    /// itself is not a backlink here: [`node_links`](Self::node_links) shows it going out.
    pub fn labeled_backlinks(&self, id: &NodeId) -> Result<Vec<NodeLink>> {
        let mut links = self.node_links(id)?;
        links.retain(|l| !l.outgoing);
        Ok(links)
    }
}

/// The direction of a kind a link may be made with by hand, or `None` when the space does not
/// know the kind.
pub(crate) fn direction_of(conn: &Connection, kind: &str) -> Result<Option<LinkDirection>> {
    Ok(find_kind(conn, kind)?.map(|k| k.direction))
}
