//! The typed frontmatter model and its tolerant round-trip.
//!
//! Every store file's structured data lives in YAML frontmatter. Known fields
//! are modelled as typed struct fields; a flattened catch-all captures every
//! key the model does not recognise. That catch-all is the tolerant-reader
//! guarantee in code:
//!
//!   * a writer preserves keys it does not understand — unknown optional
//!     frontmatter keys are round-tripped verbatim, never dropped and never an
//!     error.
//!
//! Canonical ordering is accepted on write: known fields are emitted first in
//! declaration order, then the carried-through unknown keys. Byte-exact
//! round-trip of the original file is not a goal; surviving unknown keys is.
//!
//! Wire names are kebab-case (`blocked-by`, `close-reason`, `next-number`) so
//! the frontmatter reads naturally and stays greppable by a human.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_yaml::Mapping;

use crate::frontmatter::{self, FrontmatterError};
use crate::{Actor, NodeState};

/// Failure to (de)serialise a store file's frontmatter.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// The frontmatter delimiters could not be located.
    #[error(transparent)]
    Frontmatter(#[from] FrontmatterError),
    /// The frontmatter YAML did not match the expected shape.
    #[error("frontmatter is not valid YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// A structured blocker on a node. Both parts are optional: a `blocked` node
/// may cite other nodes, a free-form reason, or both. `loti` never sets or
/// clears this automatically.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedBy {
    /// References to other nodes that block this one, as `<epic-id>/<n>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
    /// A free-form blocker for non-node causes (e.g. an external dependency).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl BlockedBy {
    /// A blocker carrying neither references nor a reason is vacuous.
    pub fn is_empty(&self) -> bool {
        self.refs.is_empty() && self.reason.is_none()
    }
}

/// One entry in a file's attachments index. The bytes live in the companion
/// directory beside the file; this records the name and an optional caption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// File name within the companion directory; unique per node/epic.
    pub name: String,
    /// Optional human caption for the attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A comment appended to a node or epic. Comments are the sole attribution
/// channel and are never hard-deleted: deletion is a flag, which keeps ids
/// stable and monotonic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    /// Per-file id; allocated as `max(existing id) + 1`.
    pub id: u64,
    /// Who wrote it — the human or a named agent.
    pub author: Actor,
    /// When it was written, ISO-8601 UTC.
    pub created: Timestamp,
    /// The comment text; serialised as a YAML literal block scalar.
    pub text: String,
    /// Soft-delete flag: a deleted comment is hidden but retained so its id is
    /// never reused.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
}

/// The next comment id for a file is one past the highest existing id, whether
/// or not that comment is soft-deleted (deleted comments still hold their id).
pub fn next_comment_id(comments: &[Comment]) -> u64 {
    comments.iter().map(|c| c.id).max().map_or(1, |m| m + 1)
}

/// Frontmatter of a node file (`<epic-id>/<n>.md`).
///
/// The trailing `extra` field is the tolerant-reader catch-all: every key not
/// named above is captured here and re-emitted verbatim on write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeFrontmatter {
    /// Flat, monotonic per-epic number; unique within its epic, never reused.
    pub number: u64,
    /// One-line name used in listings.
    pub name: String,
    /// Summary of scope.
    pub summary: String,
    /// The single node state.
    pub status: NodeState,
    /// Free-form labels; carry no intrinsic semantics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Parent node number within the same epic; absent means top-level. The
    /// tree is encoded solely by this field, never by file location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    /// Structured blocker; present only while relevant.
    #[serde(
        rename = "blocked-by",
        default,
        skip_serializing_if = "BlockedBy::is_empty"
    )]
    pub blocked_by: BlockedBy,
    /// Reason a terminally-closed node was closed; absent otherwise.
    #[serde(
        rename = "close-reason",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub close_reason: Option<String>,
    /// Attachments index; the bytes live in the companion directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Appended comments in id order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<Comment>,
    /// Creation timestamp, ISO-8601 UTC.
    pub created: Timestamp,
    /// Last-update timestamp, ISO-8601 UTC.
    pub updated: Timestamp,
    /// Unknown keys, preserved across read/write. Never dropped, never an error.
    #[serde(flatten)]
    pub extra: Mapping,
}

/// Frontmatter of an epic file (`<epic-id>/epic.md`).
///
/// Like [`NodeFrontmatter`], the trailing `extra` field preserves unknown keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpicFrontmatter {
    /// Human-chosen epic identifier.
    pub id: String,
    /// One-line name used in listings.
    pub name: String,
    /// Summary of scope.
    pub summary: String,
    /// Monotonic counter hinting the next node number. A hint only: correctness
    /// comes from exclusive file creation, and a stale-low value self-heals by
    /// probing forward. Allocation is performed elsewhere.
    #[serde(rename = "next-number")]
    pub next_number: u64,
    /// Explicit stored closed flag; independent of node states and reversible.
    #[serde(default, skip_serializing_if = "is_false")]
    pub closed: bool,
    /// Reason the epic was closed; absent unless closed with a reason.
    #[serde(
        rename = "close-reason",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub close_reason: Option<String>,
    /// Free-form labels; carry no intrinsic semantics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Attachments index; the bytes live in the `epic/` companion directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Appended comments in id order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<Comment>,
    /// Creation timestamp, ISO-8601 UTC.
    pub created: Timestamp,
    /// Last-update timestamp, ISO-8601 UTC.
    pub updated: Timestamp,
    /// Unknown keys, preserved across read/write. Never dropped, never an error.
    #[serde(flatten)]
    pub extra: Mapping,
}

/// A parsed node file: typed frontmatter plus the verbatim body below it.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeFile {
    /// Structured data from the frontmatter.
    pub frontmatter: NodeFrontmatter,
    /// Free-form markdown body, verbatim.
    pub body: String,
}

/// A parsed epic file: typed frontmatter plus the verbatim body below it.
#[derive(Debug, Clone, PartialEq)]
pub struct EpicFile {
    /// Structured data from the frontmatter.
    pub frontmatter: EpicFrontmatter,
    /// Free-form markdown body, verbatim.
    pub body: String,
}

impl NodeFile {
    /// Parse raw file text into typed frontmatter and a verbatim body.
    pub fn parse(text: &str) -> Result<Self, ModelError> {
        let split = frontmatter::split(text)?;
        let frontmatter: NodeFrontmatter = serde_yaml::from_str(&split.frontmatter)?;
        Ok(Self {
            frontmatter,
            body: split.body,
        })
    }

    /// Render typed frontmatter and body back to file text.
    pub fn to_text(&self) -> Result<String, ModelError> {
        let yaml = serde_yaml::to_string(&self.frontmatter)?;
        Ok(frontmatter::join(&yaml, &self.body))
    }
}

impl EpicFile {
    /// Parse raw file text into typed frontmatter and a verbatim body.
    pub fn parse(text: &str) -> Result<Self, ModelError> {
        let split = frontmatter::split(text)?;
        let frontmatter: EpicFrontmatter = serde_yaml::from_str(&split.frontmatter)?;
        Ok(Self {
            frontmatter,
            body: split.body,
        })
    }

    /// Render typed frontmatter and body back to file text.
    pub fn to_text(&self) -> Result<String, ModelError> {
        let yaml = serde_yaml::to_string(&self.frontmatter)?;
        Ok(frontmatter::join(&yaml, &self.body))
    }
}

/// Add or replace an entry in an attachments index by name; returns the prior
/// entry when the name already existed. The bytes are copied in separately.
pub fn upsert_attachment(index: &mut Vec<Attachment>, entry: Attachment) -> Option<Attachment> {
    if let Some(slot) = index.iter_mut().find(|a| a.name == entry.name) {
        Some(std::mem::replace(slot, entry))
    } else {
        index.push(entry);
        None
    }
}

/// Remove an attachment index entry by name; returns it when present.
pub fn remove_attachment(index: &mut Vec<Attachment>, name: &str) -> Option<Attachment> {
    let pos = index.iter().position(|a| a.name == name)?;
    Some(index.remove(pos))
}

/// serde `skip_serializing_if` predicate for `bool` fields that default false.
fn is_false(b: &bool) -> bool {
    !*b
}

/// Serialise the human actor as `human` and a named agent as `agent:<name>`,
/// matching the output vocabulary. This is the wire form in frontmatter.
impl Serialize for Actor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Actor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == "human" {
            Ok(Actor::Human)
        } else if let Some(name) = raw.strip_prefix("agent:") {
            if name.is_empty() {
                return Err(serde::de::Error::custom("agent actor is missing a name"));
            }
            Ok(Actor::Agent(name.to_string()))
        } else {
            Err(serde::de::Error::custom(format!(
                "unrecognised actor '{raw}': expected 'human' or 'agent:<name>'"
            )))
        }
    }
}

/// serde value form of [`NodeState`] on the wire is the kebab-case state name.
impl Serialize for NodeState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_name())
    }
}

impl<'de> Deserialize<'de> for NodeState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        NodeState::from_wire(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unrecognised status '{raw}': expected one of \
                 to-do, in-progress, blocked, done, closed"
            ))
        })
    }
}

impl NodeState {
    /// The kebab-case wire name used in frontmatter and output.
    pub fn wire_name(self) -> &'static str {
        match self {
            NodeState::ToDo => "to-do",
            NodeState::InProgress => "in-progress",
            NodeState::Blocked => "blocked",
            NodeState::Done => "done",
            NodeState::Closed => "closed",
        }
    }

    /// Parse a kebab-case wire name back into a state.
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "to-do" => NodeState::ToDo,
            "in-progress" => NodeState::InProgress,
            "blocked" => NodeState::Blocked,
            "done" => NodeState::Done,
            "closed" => NodeState::Closed,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn sample_node_text() -> &'static str {
        "---\n\
         number: 7\n\
         name: do the thing\n\
         summary: a slice of work\n\
         status: in-progress\n\
         labels:\n\
         - backend\n\
         parent: 3\n\
         created: 2024-01-02T03:04:05Z\n\
         updated: 2024-01-02T03:04:05Z\n\
         ---\n\
         free body text\n"
    }

    #[test]
    fn node_round_trips_known_fields() {
        let node = NodeFile::parse(sample_node_text()).unwrap();
        assert_eq!(node.frontmatter.number, 7);
        assert_eq!(node.frontmatter.status, NodeState::InProgress);
        assert_eq!(node.frontmatter.parent, Some(3));
        assert_eq!(node.frontmatter.labels, vec!["backend".to_string()]);
        assert_eq!(node.body, "free body text\n");
    }

    #[test]
    fn unknown_keys_are_preserved_across_read_and_write() {
        // A key the model has never heard of must survive a parse/emit cycle.
        let text = "---\n\
             number: 1\n\
             name: n\n\
             summary: s\n\
             status: to-do\n\
             created: 2024-01-01T00:00:00Z\n\
             updated: 2024-01-01T00:00:00Z\n\
             future-field: keep-me\n\
             nested-unknown:\n  a: 1\n  b: two\n\
             ---\n\
             body\n";
        let node = NodeFile::parse(text).unwrap();
        assert!(node
            .frontmatter
            .extra
            .contains_key(serde_yaml::Value::from("future-field")));
        let out = node.to_text().unwrap();
        assert!(out.contains("future-field: keep-me"));
        assert!(out.contains("nested-unknown:"));
        // Re-parsing the emitted text keeps the unknown keys stable.
        let again = NodeFile::parse(&out).unwrap();
        assert_eq!(node.frontmatter.extra, again.frontmatter.extra);
    }

    #[test]
    fn blocked_by_round_trips() {
        let text = "---\n\
             number: 2\n\
             name: n\n\
             summary: s\n\
             status: blocked\n\
             blocked-by:\n  refs:\n  - my-epic/1\n  reason: waiting on a key\n\
             created: 2024-01-01T00:00:00Z\n\
             updated: 2024-01-01T00:00:00Z\n\
             ---\n";
        let node = NodeFile::parse(text).unwrap();
        assert_eq!(node.frontmatter.blocked_by.refs, vec!["my-epic/1"]);
        assert_eq!(
            node.frontmatter.blocked_by.reason.as_deref(),
            Some("waiting on a key")
        );
        let out = node.to_text().unwrap();
        assert!(out.contains("blocked-by:"));
        assert!(out.contains("my-epic/1"));
    }

    #[test]
    fn comment_text_serialises_as_a_literal_block() {
        let mut fm = minimal_node_frontmatter();
        fm.comments.push(Comment {
            id: 1,
            author: Actor::Agent("bot".into()),
            created: ts("2024-01-01T00:00:00Z"),
            text: "line one\nline two\n".into(),
            deleted: false,
        });
        let node = NodeFile {
            frontmatter: fm,
            body: String::new(),
        };
        let out = node.to_text().unwrap();
        // A multi-line comment is emitted as a `|` literal block scalar.
        assert!(
            out.contains("text: |"),
            "expected literal block, got:\n{out}"
        );
        assert!(out.contains("agent:bot"));
    }

    #[test]
    fn next_comment_id_is_max_plus_one_including_deleted() {
        let comments = vec![
            Comment {
                id: 1,
                author: Actor::Human,
                created: ts("2024-01-01T00:00:00Z"),
                text: "a".into(),
                deleted: true,
            },
            Comment {
                id: 4,
                author: Actor::Human,
                created: ts("2024-01-01T00:00:00Z"),
                text: "b".into(),
                deleted: false,
            },
        ];
        assert_eq!(next_comment_id(&comments), 5);
        assert_eq!(next_comment_id(&[]), 1);
    }

    #[test]
    fn epic_round_trips_and_preserves_unknown_keys() {
        let text = "---\n\
             id: my-epic\n\
             name: the epic\n\
             summary: big work\n\
             next-number: 12\n\
             created: 2024-01-01T00:00:00Z\n\
             updated: 2024-01-01T00:00:00Z\n\
             experimental: yes\n\
             ---\n\
             epic body\n";
        let epic = EpicFile::parse(text).unwrap();
        assert_eq!(epic.frontmatter.id, "my-epic");
        assert_eq!(epic.frontmatter.next_number, 12);
        assert!(!epic.frontmatter.closed);
        let out = epic.to_text().unwrap();
        assert!(out.contains("experimental: yes"));
        assert!(out.contains("next-number: 12"));
    }

    #[test]
    fn closed_epic_carries_a_reason() {
        let mut fm = minimal_epic_frontmatter();
        fm.closed = true;
        fm.close_reason = Some("superseded".into());
        let out = EpicFile {
            frontmatter: fm,
            body: String::new(),
        }
        .to_text()
        .unwrap();
        assert!(out.contains("closed: true"));
        assert!(out.contains("close-reason: superseded"));
    }

    #[test]
    fn actor_wire_forms() {
        assert_eq!(
            serde_yaml::to_string(&Actor::Human).unwrap().trim(),
            "human"
        );
        assert_eq!(
            serde_yaml::to_string(&Actor::Agent("x".into()))
                .unwrap()
                .trim(),
            "agent:x"
        );
        let a: Actor = serde_yaml::from_str("agent:bot").unwrap();
        assert_eq!(a, Actor::Agent("bot".into()));
        assert!(serde_yaml::from_str::<Actor>("nonsense").is_err());
    }

    #[test]
    fn status_wire_names_are_kebab_case() {
        assert_eq!(NodeState::InProgress.wire_name(), "in-progress");
        assert_eq!(NodeState::from_wire("to-do"), Some(NodeState::ToDo));
        assert_eq!(NodeState::from_wire("bogus"), None);
    }

    #[test]
    fn attachment_index_upsert_and_remove() {
        let mut index = Vec::new();
        assert!(upsert_attachment(
            &mut index,
            Attachment {
                name: "a.png".into(),
                description: Some("first".into())
            }
        )
        .is_none());
        let prior = upsert_attachment(
            &mut index,
            Attachment {
                name: "a.png".into(),
                description: Some("second".into()),
            },
        );
        assert_eq!(prior.unwrap().description.as_deref(), Some("first"));
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].description.as_deref(), Some("second"));
        let removed = remove_attachment(&mut index, "a.png").unwrap();
        assert_eq!(removed.name, "a.png");
        assert!(remove_attachment(&mut index, "a.png").is_none());
    }

    fn minimal_node_frontmatter() -> NodeFrontmatter {
        NodeFrontmatter {
            number: 1,
            name: "n".into(),
            summary: "s".into(),
            status: NodeState::ToDo,
            labels: Vec::new(),
            parent: None,
            blocked_by: BlockedBy::default(),
            close_reason: None,
            attachments: Vec::new(),
            comments: Vec::new(),
            created: ts("2024-01-01T00:00:00Z"),
            updated: ts("2024-01-01T00:00:00Z"),
            extra: Mapping::new(),
        }
    }

    fn minimal_epic_frontmatter() -> EpicFrontmatter {
        EpicFrontmatter {
            id: "e".into(),
            name: "n".into(),
            summary: "s".into(),
            next_number: 1,
            closed: false,
            close_reason: None,
            labels: Vec::new(),
            attachments: Vec::new(),
            comments: Vec::new(),
            created: ts("2024-01-01T00:00:00Z"),
            updated: ts("2024-01-01T00:00:00Z"),
            extra: Mapping::new(),
        }
    }
}
