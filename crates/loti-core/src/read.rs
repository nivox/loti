//! Resolving stored data into the shapes the read forms render from.
//!
//! This is the read-side counterpart to [`crate::ops`]: it reads a node, an
//! epic, or a scoped set of nodes, and turns them into the canonical JSON value
//! and the small row/line types [`crate::render`] consumes. Scope here is only
//! the minimum needed to make `list` runnable end-to-end — a whole epic, the
//! direct children (or subtree) under a node, and the flat epic roster. The
//! richer filter families (label/state/match) are a separate layer that will
//! narrow the set this module resolves before it is rendered.

use serde_json::Value;

use crate::domain::{epic_state, NodeRef, NodeStatus};
use crate::ops::{descendants_of, list_comments, load_epic_nodes, CommentView, OpError, Target};
use crate::render::{BlockedTag, ChildRow, CommentLine, ListEpic, ListNode};
use crate::store::{Store, EPIC_FILE};
use crate::{model::NodeFile, render};

/// The canonical JSON value of a node, ready for `show --json`/`--raw`/field
/// projection. Errors if the node does not exist.
pub fn node_json(store: &Store, node_ref: &NodeRef) -> Result<Value, OpError> {
    let node = read_node(store, node_ref)?;
    Ok(render::node_to_json(&node_ref.epic_id, &node))
}

/// The canonical JSON value of an epic, with its computed `state` filled in from
/// the current node states. Errors if the epic does not exist.
pub fn epic_json(store: &Store, epic_id: &str) -> Result<Value, OpError> {
    let epic = read_epic(store, epic_id)?;
    let statuses = node_statuses(store, epic_id)?;
    Ok(render::epic_to_json(&epic, &statuses))
}

/// The direct children of a node, as markdown-table rows in ascending order.
pub fn node_children(store: &Store, node_ref: &NodeRef) -> Result<Vec<ChildRow>, OpError> {
    let nodes = load_epic_nodes(store, &node_ref.epic_id)?;
    let mut rows: Vec<(u64, ChildRow)> = nodes
        .iter()
        .filter(|n| n.frontmatter.parent == Some(node_ref.number))
        .map(|n| (n.frontmatter.number, child_row(&node_ref.epic_id, n)))
        .collect();
    rows.sort_by_key(|(number, _)| *number);
    Ok(rows.into_iter().map(|(_, r)| r).collect())
}

/// The top-level nodes of an epic (those with no parent), as table rows.
pub fn epic_children(store: &Store, epic_id: &str) -> Result<Vec<ChildRow>, OpError> {
    let nodes = load_epic_nodes(store, epic_id)?;
    let mut rows: Vec<(u64, ChildRow)> = nodes
        .iter()
        .filter(|n| n.frontmatter.parent.is_none())
        .map(|n| (n.frontmatter.number, child_row(epic_id, n)))
        .collect();
    rows.sort_by_key(|(number, _)| *number);
    Ok(rows.into_iter().map(|(_, r)| r).collect())
}

/// The comments of a node or epic as render lines, honouring the
/// hidden-unless-requested rule (tombstones only appear under `include_deleted`).
pub fn comment_lines(
    store: &Store,
    target: &Target,
    include_deleted: bool,
) -> Result<Vec<CommentLine>, OpError> {
    let views = list_comments(store, target, include_deleted)?;
    Ok(views.into_iter().map(comment_line).collect())
}

/// The flat roster of every epic under the data root, in id order, for
/// `epic list`.
pub fn list_epics(store: &Store) -> Result<Vec<ListEpic>, OpError> {
    let mut ids = epic_ids(store)?;
    ids.sort();
    let mut out = Vec::new();
    for id in ids {
        let epic = read_epic(store, &id)?;
        let statuses = node_statuses(store, &id)?;
        out.push(ListEpic {
            id: epic.frontmatter.id.clone(),
            name: epic.frontmatter.name.clone(),
            state: epic_state(epic.frontmatter.closed, &statuses)
                .wire_name()
                .to_string(),
            labels: epic.frontmatter.labels.clone(),
            nodes: statuses.len(),
        });
    }
    Ok(out)
}

/// The scope a node `list` resolves over: a whole epic, or under one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListScope {
    /// Every node in an epic.
    Epic(String),
    /// The children of a node — direct only, or the whole subtree when
    /// `recursive`.
    Under {
        /// The node whose descendants are listed.
        node: NodeRef,
        /// Whether to include the whole subtree rather than direct children.
        recursive: bool,
    },
}

/// Resolve a node `list` scope to the flat set of nodes to render, each located
/// by its reference and parent pointer. The set is flat by construction; the
/// plain-text renderer reconstructs the tree from the parent pointers.
pub fn list_nodes(store: &Store, scope: &ListScope) -> Result<Vec<ListNode>, OpError> {
    let (epic_id, selected): (String, Vec<NodeFile>) = match scope {
        ListScope::Epic(id) => {
            let nodes = load_epic_nodes(store, id)?;
            (id.clone(), nodes)
        }
        ListScope::Under { node, recursive } => {
            // Confirm the anchor node exists so an unknown scope is a clean error.
            read_node(store, node)?;
            let all = load_epic_nodes(store, &node.epic_id)?;
            let selected = if *recursive {
                let sub: std::collections::HashSet<u64> = descendants_of(store, node)?
                    .into_iter()
                    .map(|s| s.number)
                    .collect();
                all.into_iter()
                    .filter(|n| sub.contains(&n.frontmatter.number))
                    .collect()
            } else {
                all.into_iter()
                    .filter(|n| n.frontmatter.parent == Some(node.number))
                    .collect()
            };
            (node.epic_id.clone(), selected)
        }
    };

    let mut rows: Vec<ListNode> = selected.iter().map(|n| list_node(&epic_id, n)).collect();
    rows.sort_by_key(|n| n.number);
    Ok(rows)
}

// ---------------------------------------------------------------------------
// internal conversions
// ---------------------------------------------------------------------------

fn list_node(epic_id: &str, n: &NodeFile) -> ListNode {
    let fm = &n.frontmatter;
    ListNode {
        reference: format!("{}/{}", epic_id, fm.number),
        number: fm.number,
        name: fm.name.clone(),
        status: fm.status.wire_name().to_string(),
        parent: fm.parent.map(|p| format!("{}/{}", epic_id, p)),
        labels: fm.labels.clone(),
        blocked: if fm.status == crate::NodeState::Blocked || !fm.blocked_by.is_empty() {
            // A blocked node carries the tag; a node with a stored blocker but a
            // non-blocked state does not (the state is authoritative), so only
            // tag when actually blocked.
            if fm.status == crate::NodeState::Blocked {
                Some(BlockedTag {
                    refs: fm.blocked_by.refs.clone(),
                    reason: fm.blocked_by.reason.clone(),
                })
            } else {
                None
            }
        } else {
            None
        },
    }
}

fn child_row(epic_id: &str, n: &NodeFile) -> ChildRow {
    ChildRow {
        reference: format!("{}/{}", epic_id, n.frontmatter.number),
        name: n.frontmatter.name.clone(),
        status: n.frontmatter.status.wire_name().to_string(),
    }
}

fn comment_line(view: CommentView) -> CommentLine {
    match view {
        CommentView::Live(c) => CommentLine::Live {
            id: c.id,
            author: c.author.to_string(),
            created: c.created.to_string(),
            text: c.text,
        },
        CommentView::Tombstone {
            id,
            author,
            created,
        } => CommentLine::Tombstone {
            id,
            author: author.to_string(),
            created: created.to_string(),
        },
    }
}

/// The `(number, state)` pairs of an epic's nodes, for computing its state and
/// node count.
fn node_statuses(store: &Store, epic_id: &str) -> Result<Vec<NodeStatus>, OpError> {
    Ok(load_epic_nodes(store, epic_id)?
        .iter()
        .map(|n| NodeStatus::new(n.frontmatter.number, n.frontmatter.status))
        .collect())
}

/// The ids of every epic directory under the data root: a directory that holds
/// an `epic.md`. The epic's own asset directory and any stray files are skipped.
fn epic_ids(store: &Store) -> Result<Vec<String>, OpError> {
    let mut ids = Vec::new();
    let entries = match std::fs::read_dir(store.root()) {
        Ok(e) => e,
        // No root directory yet: no epics.
        Err(_) => return Ok(ids),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // The store's own metadata directory is not an epic.
        if name.starts_with('.') {
            continue;
        }
        // An epic directory is one that holds an epic file.
        if path.join(EPIC_FILE).is_file() {
            ids.push(name.to_string());
        }
    }
    Ok(ids)
}

/// Read an epic, mapping a missing file to a clean not-found error.
fn read_epic(store: &Store, epic_id: &str) -> Result<crate::model::EpicFile, OpError> {
    if !store.epic_path(epic_id).is_file() {
        return Err(OpError::NoSuchEpic(epic_id.to_string()));
    }
    Ok(store.read_epic(epic_id)?)
}

/// Read a node, mapping a missing file to a clean not-found error.
fn read_node(store: &Store, node_ref: &NodeRef) -> Result<NodeFile, OpError> {
    if !store
        .node_path(&node_ref.epic_id, node_ref.number)
        .is_file()
    {
        return Err(OpError::NoSuchNode(node_ref.clone()));
    }
    Ok(store.read_node(&node_ref.epic_id, node_ref.number)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::LockConfig;
    use crate::ops::{
        create_epic, create_node, set_node_status, NewEpic, NewNode, NodeStatusChange,
    };
    use std::time::Duration;

    fn fast_store(root: &std::path::Path) -> Store {
        Store::at(root).with_lock_config(LockConfig {
            stale_threshold: Duration::from_millis(80),
            retry_interval: Duration::from_millis(5),
        })
    }

    fn seeded() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        (dir, store)
    }

    fn new_epic(id: &str) -> NewEpic {
        NewEpic {
            epic_id: id.to_string(),
            name: "the epic".into(),
            summary: "scope".into(),
            labels: vec![],
            body: String::new(),
        }
    }

    fn new_node(epic: &str, parent: Option<NodeRef>, name: &str) -> NewNode {
        NewNode {
            epic_id: epic.to_string(),
            parent,
            name: name.to_string(),
            summary: "slice".into(),
            labels: vec![],
            body: String::new(),
        }
    }

    #[test]
    fn epic_json_carries_computed_state_and_node_count() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        create_node(&s, new_node("e", None, "a")).unwrap();
        let value = epic_json(&s, "e").unwrap();
        assert_eq!(value["state"], "open");
        assert_eq!(value["nodes"], 1);
        assert_eq!(value["id"], "e");
    }

    #[test]
    fn list_scope_epic_returns_all_nodes_flat_with_parent_pointers() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let a = create_node(&s, new_node("e", None, "a")).unwrap();
        let ar = NodeRef::new("e", a.frontmatter.number);
        create_node(&s, new_node("e", Some(ar.clone()), "b")).unwrap();
        let rows = list_nodes(&s, &ListScope::Epic("e".into())).unwrap();
        assert_eq!(rows.len(), 2);
        // The child carries a parent pointer; the parent has none.
        let child = rows.iter().find(|r| r.name == "b").unwrap();
        assert_eq!(child.parent.as_deref(), Some("e/1"));
        let parent = rows.iter().find(|r| r.name == "a").unwrap();
        assert_eq!(parent.parent, None);
    }

    #[test]
    fn list_scope_under_node_direct_vs_recursive() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let a = create_node(&s, new_node("e", None, "a")).unwrap();
        let ar = NodeRef::new("e", a.frontmatter.number);
        let b = create_node(&s, new_node("e", Some(ar.clone()), "b")).unwrap();
        let br = NodeRef::new("e", b.frontmatter.number);
        create_node(&s, new_node("e", Some(br), "c")).unwrap();

        let direct = list_nodes(
            &s,
            &ListScope::Under {
                node: ar.clone(),
                recursive: false,
            },
        )
        .unwrap();
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].name, "b");

        let subtree = list_nodes(
            &s,
            &ListScope::Under {
                node: ar,
                recursive: true,
            },
        )
        .unwrap();
        assert_eq!(subtree.len(), 2);
    }

    #[test]
    fn blocked_node_carries_a_tag() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let a = create_node(&s, new_node("e", None, "a")).unwrap();
        let ar = NodeRef::new("e", a.frontmatter.number);
        set_node_status(
            &s,
            &ar,
            NodeStatusChange::Blocked {
                refs: vec![NodeRef::new("e", 99)],
                reason: Some("waiting".into()),
            },
        )
        .unwrap();
        let rows = list_nodes(&s, &ListScope::Epic("e".into())).unwrap();
        let tag = rows[0].blocked.as_ref().expect("blocked tag");
        assert_eq!(tag.refs, vec!["e/99"]);
        assert_eq!(tag.reason.as_deref(), Some("waiting"));
    }

    #[test]
    fn list_epics_roster_is_sorted_with_counts() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("z-epic")).unwrap();
        create_epic(&s, new_epic("a-epic")).unwrap();
        create_node(&s, new_node("a-epic", None, "n")).unwrap();
        let roster = list_epics(&s).unwrap();
        assert_eq!(roster.len(), 2);
        // Sorted by id.
        assert_eq!(roster[0].id, "a-epic");
        assert_eq!(roster[0].nodes, 1);
        assert_eq!(roster[1].id, "z-epic");
        assert_eq!(roster[1].nodes, 0);
    }

    #[test]
    fn node_and_epic_children_tables() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let a = create_node(&s, new_node("e", None, "a")).unwrap();
        let ar = NodeRef::new("e", a.frontmatter.number);
        create_node(&s, new_node("e", Some(ar.clone()), "b")).unwrap();

        let epic_kids = epic_children(&s, "e").unwrap();
        assert_eq!(epic_kids.len(), 1);
        assert_eq!(epic_kids[0].name, "a");

        let node_kids = node_children(&s, &ar).unwrap();
        assert_eq!(node_kids.len(), 1);
        assert_eq!(node_kids[0].name, "b");
    }

    #[test]
    fn missing_targets_are_clean_errors() {
        let (_d, s) = seeded();
        assert!(matches!(
            epic_json(&s, "ghost"),
            Err(OpError::NoSuchEpic(_))
        ));
        assert!(matches!(
            node_json(&s, &NodeRef::new("ghost", 1)),
            Err(OpError::NoSuchNode(_))
        ));
    }
}
