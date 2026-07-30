//! The write-side business operations.
//!
//! This module is the UI-agnostic home of every mutating operation: creating
//! and editing epics and nodes, driving the state machine, and managing the
//! label/comment/asset collections. Each function takes plain typed inputs,
//! enforces the domain rules ([`crate::domain`]) and the concurrency/version
//! discipline (via [`crate::store::Store`]), and returns a rich result or a
//! typed [`OpError`]. No CLI types leak in here, so the same operations back
//! the CLI today and any future surface unchanged.
//!
//! The invariant every mutation upholds: it goes through the store, which
//! brackets it with the temp-file lock and the version gate — never a raw
//! filesystem write. An operation that depends on what is stored goes further:
//! it hands the change to the store's read-modify-write, so the lock is held
//! from before the entity is read until the publish. Nothing can land in
//! between, and a merge (a label added, a comment appended, a blocker removed)
//! therefore merges into the entity as stored rather than into a stale copy.

use jiff::Timestamp;

use crate::domain::{
    self, authorize_comment_mutation, plan_close, validate_blocked, validate_done, Cascade,
    NodeRef, NodeStatus, RefParseError,
};
use crate::lock::{self, Force};
use crate::model::{
    insert_asset, next_comment_id, remove_asset, Asset, Claim, Comment, EpicFile, EpicFrontmatter,
    NodeFile, NodeFrontmatter,
};
use crate::store::{Store, StoreError};
use crate::{Actor, NodeState};
use serde_yaml::Mapping;

/// Failure of a write-side operation. Wraps the lower layers' errors and adds
/// the operation-level rule violations the store and domain layers cannot see
/// on their own (e.g. "that epic already exists", "the new parent would form a
/// cycle").
#[derive(Debug, thiserror::Error)]
pub enum OpError {
    /// A storage-layer failure (I/O, lock, version gate, parse).
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A domain state-machine rule was violated.
    #[error(transparent)]
    Transition(#[from] domain::TransitionError),
    /// A comment edit/delete was not authorised (wrong author or already gone).
    #[error(transparent)]
    CommentAuth(#[from] domain::CommentAuthError),
    /// A `<epic-id>/<n>` reference could not be parsed.
    #[error(transparent)]
    Ref(#[from] RefParseError),
    /// Creating an epic whose id is already taken.
    #[error("epic {0} already exists")]
    EpicExists(String),
    /// Addressing an epic that does not exist.
    #[error("epic {0} does not exist")]
    NoSuchEpic(String),
    /// Addressing a node that does not exist within its epic.
    #[error("node {0} does not exist")]
    NoSuchNode(NodeRef),
    /// A `--parent`/reparent reference points into a different epic; a node's
    /// parent is always within the same epic (the tree never spans epics).
    #[error("parent {parent} is not in epic {epic_id}; a node's parent must be in the same epic")]
    ParentInDifferentEpic {
        /// The offending parent reference.
        parent: NodeRef,
        /// The epic the node being created/edited belongs to.
        epic_id: String,
    },
    /// A node cannot list itself as a `blocked-by` dependency.
    #[error("a node cannot be blocked by itself ({0})")]
    BlockedBySelf(NodeRef),
    /// Taking a claim needs a non-empty identifier to record who holds it.
    #[error("a claim needs a non-empty identifier: pass --as <identifier>")]
    EmptyClaimIdentifier,
    /// Reparenting would make a node its own ancestor (a cycle).
    #[error("reparenting {node} under {parent} would form a cycle")]
    ReparentCycle {
        /// The node being reparented.
        node: NodeRef,
        /// The proposed new parent.
        parent: NodeRef,
    },
    /// Addressing a comment id that does not exist on the target.
    #[error("comment #{0} does not exist here")]
    NoSuchComment(u64),
    /// Deleting/reading/updating an asset that is not indexed on the target.
    #[error("asset '{0}' does not exist here")]
    NoSuchAsset(String),
    /// Adding an asset whose name is already taken. An asset name is a
    /// caller-chosen key; `add` never overwrites — use `update` to replace one.
    #[error("asset '{0}' already exists here; use `asset update` to replace it")]
    AssetExists(String),
    /// An asset add was given no name and no --file to derive one from.
    #[error("an asset needs a name: pass --name, or --file so the basename can be used")]
    AssetNeedsName,
    /// A cascade close committed some descendants but then failed; the store is
    /// left with partial progress and the operation is safe to re-run.
    #[error("cascade close stopped partway at {failed}: {reason}; re-run to finish")]
    CascadePartial {
        /// The node the cascade failed on.
        failed: NodeRef,
        /// Why it failed.
        reason: String,
    },
}

/// Which kind of target a collection operation addresses: an epic (by id) or a
/// node (by reference). Labels, comments and assets are identical under both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// An epic addressed by its id.
    Epic(String),
    /// A node addressed by `<epic-id>/<n>`.
    Node(NodeRef),
}

/// The current wall-clock instant as an ISO-8601 UTC timestamp, for
/// `created`/`updated` stamping.
fn now() -> Timestamp {
    Timestamp::now()
}

/// Deduplicate a label list, preserving first-seen order, so a label set never
/// carries repeats regardless of how it was supplied.
fn dedup_preserving_order(labels: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for l in labels {
        if seen.insert(l.clone()) {
            out.push(l);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// create
// ---------------------------------------------------------------------------

/// Inputs for creating an epic. Body is the already-resolved payload (empty
/// when no source was given).
#[derive(Debug, Clone)]
pub struct NewEpic {
    /// The human-chosen epic id.
    pub epic_id: String,
    /// One-line name.
    pub name: String,
    /// One-line summary.
    pub summary: String,
    /// Labels (deduplicated on create).
    pub labels: Vec<String>,
    /// Free-form body (verbatim; empty allowed).
    pub body: String,
}

/// Create an epic. Refuses if the id is already taken. The new epic starts its
/// number pool at 1 and is open (not closed).
pub fn create_epic(store: &Store, new: NewEpic) -> Result<EpicFile, OpError> {
    if store.epic_path(&new.epic_id).is_file() {
        return Err(OpError::EpicExists(new.epic_id));
    }
    let ts = now();
    let epic = EpicFile {
        frontmatter: EpicFrontmatter {
            id: new.epic_id.clone(),
            name: new.name,
            summary: new.summary,
            next_number: 1,
            closed: false,
            close_reason: None,
            labels: dedup_preserving_order(new.labels),
            assets: Vec::new(),
            comments: Vec::new(),
            created: ts,
            updated: ts,
            extra: Mapping::new(),
        },
        body: new.body,
    };
    store.write_epic(&new.epic_id, &epic)?;
    Ok(epic)
}

/// Inputs for creating a node. `parent` is an already-parsed reference when a
/// subticket was requested.
#[derive(Debug, Clone)]
pub struct NewNode {
    /// The owning epic id.
    pub epic_id: String,
    /// Optional parent reference for a subticket; absent = top-level ticket.
    pub parent: Option<NodeRef>,
    /// One-line name.
    pub name: String,
    /// One-line summary.
    pub summary: String,
    /// Labels (deduplicated on create).
    pub labels: Vec<String>,
    /// Free-form body (verbatim; empty allowed).
    pub body: String,
}

/// Create a node (ticket or subticket) in an epic, allocating its number from
/// the epic's flat pool. When a parent is given it is validated: it must be in
/// the same epic and must exist. The node starts `to-do`.
pub fn create_node(store: &Store, new: NewNode) -> Result<NodeFile, OpError> {
    if !store.epic_path(&new.epic_id).is_file() {
        return Err(OpError::NoSuchEpic(new.epic_id));
    }
    let parent_number = match &new.parent {
        Some(parent) => {
            if parent.epic_id != new.epic_id {
                return Err(OpError::ParentInDifferentEpic {
                    parent: parent.clone(),
                    epic_id: new.epic_id.clone(),
                });
            }
            if !store.node_path(&parent.epic_id, parent.number).is_file() {
                return Err(OpError::NoSuchNode(parent.clone()));
            }
            Some(parent.number)
        }
        None => None,
    };

    let ts = now();
    let fields = NodeFile {
        frontmatter: NodeFrontmatter {
            // Overwritten by allocation; a placeholder until then.
            number: 0,
            name: new.name,
            summary: new.summary,
            status: NodeState::ToDo,
            labels: dedup_preserving_order(new.labels),
            parent: parent_number,
            blocked_by: Vec::new(),
            block_reason: None,
            close_reason: None,
            claim: None,
            assets: Vec::new(),
            comments: Vec::new(),
            created: ts,
            updated: ts,
            extra: Mapping::new(),
        },
        body: new.body,
    };
    Ok(store.create_node(&new.epic_id, fields)?)
}

// ---------------------------------------------------------------------------
// edit (plain scalar fields)
// ---------------------------------------------------------------------------

/// The scalar edits for an epic. Each `Some` replaces that field; `None` leaves
/// it. `body` follows the content-input rule: a resolved payload replaces the
/// body, and its absence means "leave the body unchanged".
#[derive(Debug, Clone, Default)]
pub struct EpicEdits {
    /// Replacement name.
    pub name: Option<String>,
    /// Replacement summary.
    pub summary: Option<String>,
    /// Replacement body (already resolved from stdin/--file); `None` = leave.
    pub body: Option<String>,
    /// Apply only while the epic's stored `updated` stamp still equals this one
    /// — the opt-in guard against silently discarding a change made while the
    /// replacement text was being composed. `None` applies unconditionally
    /// (last write wins). It is a precondition, not a change, so a stamp alone
    /// never makes an otherwise-empty edit write.
    pub expect_updated: Option<Timestamp>,
}

impl EpicEdits {
    /// Whether this edit set would change anything at all.
    fn is_empty(&self) -> bool {
        self.name.is_none() && self.summary.is_none() && self.body.is_none()
    }
}

/// Edit an epic's plain scalar fields. Bumps `updated` when anything changed.
pub fn edit_epic(store: &Store, epic_id: &str, edits: EpicEdits) -> Result<EpicFile, OpError> {
    if edits.is_empty() {
        // A stamp is a precondition, never a change: an edit with nothing to
        // apply writes nothing and reports the epic as it stands.
        return read_epic(store, epic_id);
    }
    let (epic, ()) = modify_epic(store, epic_id, edits.expect_updated, |epic| {
        if let Some(name) = edits.name {
            epic.frontmatter.name = name;
        }
        if let Some(summary) = edits.summary {
            epic.frontmatter.summary = summary;
        }
        if let Some(body) = edits.body {
            epic.body = body;
        }
        epic.frontmatter.updated = now();
        Ok(())
    })?;
    Ok(epic)
}

/// The scalar edits for a node, including an optional reparent.
#[derive(Debug, Clone, Default)]
pub struct NodeEdits {
    /// Replacement name.
    pub name: Option<String>,
    /// Replacement summary.
    pub summary: Option<String>,
    /// Reparent under this reference; a reparent is a one-field edit.
    pub parent: Option<NodeRef>,
    /// Replacement body (already resolved from stdin/--file); `None` = leave.
    pub body: Option<String>,
    /// Apply only while the node's stored `updated` stamp still equals this one;
    /// `None` applies unconditionally (last write wins). As on an epic, it is a
    /// precondition and never a change in its own right.
    pub expect_updated: Option<Timestamp>,
}

impl NodeEdits {
    fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.summary.is_none()
            && self.parent.is_none()
            && self.body.is_none()
    }
}

/// Edit a node's plain scalar fields. A reparent validates that the new parent
/// is in the same epic, exists, and would not make the node its own ancestor.
/// Bumps `updated` when anything changed.
pub fn edit_node(store: &Store, node_ref: &NodeRef, edits: NodeEdits) -> Result<NodeFile, OpError> {
    // "It is not there" is answered first, and about the node being edited
    // rather than about a parent it names.
    require_node(store, node_ref)?;

    // A reparent is validated against the rest of the epic — other files — so it
    // happens before this node's lock is taken.
    if let Some(parent) = &edits.parent {
        validate_reparent(store, node_ref, parent)?;
    }

    if edits.is_empty() {
        return read_node(store, node_ref);
    }
    let (node, ()) = modify_node(store, node_ref, edits.expect_updated, |node| {
        if let Some(name) = edits.name {
            node.frontmatter.name = name;
        }
        if let Some(summary) = edits.summary {
            node.frontmatter.summary = summary;
        }
        if let Some(parent) = edits.parent {
            node.frontmatter.parent = Some(parent.number);
        }
        if let Some(body) = edits.body {
            node.body = body;
        }
        node.frontmatter.updated = now();
        Ok(())
    })?;
    Ok(node)
}

/// Validate a proposed reparent: same epic, the parent exists, and following
/// the parent chain up from the new parent never reaches the node itself (which
/// would be a cycle — a node cannot be its own ancestor).
fn validate_reparent(store: &Store, node_ref: &NodeRef, parent: &NodeRef) -> Result<(), OpError> {
    if parent.epic_id != node_ref.epic_id {
        return Err(OpError::ParentInDifferentEpic {
            parent: parent.clone(),
            epic_id: node_ref.epic_id.clone(),
        });
    }
    if parent.number == node_ref.number {
        return Err(OpError::ReparentCycle {
            node: node_ref.clone(),
            parent: parent.clone(),
        });
    }
    if !store.node_path(&parent.epic_id, parent.number).is_file() {
        return Err(OpError::NoSuchNode(parent.clone()));
    }

    // Walk up from the proposed parent; if we reach the node being reparented,
    // the parent is a descendant of it and the move would form a cycle.
    let nodes = load_epic_nodes(store, &node_ref.epic_id)?;
    let by_number: std::collections::HashMap<u64, Option<u64>> = nodes
        .iter()
        .map(|n| (n.frontmatter.number, n.frontmatter.parent))
        .collect();

    let mut cursor = Some(parent.number);
    let mut guard = 0usize;
    while let Some(current) = cursor {
        if current == node_ref.number {
            return Err(OpError::ReparentCycle {
                node: node_ref.clone(),
                parent: parent.clone(),
            });
        }
        // A malformed chain (missing link) simply ends the walk; the store
        // layer keeps the tree consistent, so this is defensive only.
        cursor = by_number.get(&current).copied().flatten();
        guard += 1;
        if guard > by_number.len() + 1 {
            // Pre-existing cycle in stored data: stop rather than loop forever.
            break;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// status (the state machine, set-only)
// ---------------------------------------------------------------------------

/// A requested node state transition, mirroring the CLI's set-only status verb
/// but with the payload already parsed into typed values.
#[derive(Debug, Clone)]
pub enum NodeStatusChange {
    /// Move to `to-do`.
    ToDo,
    /// Move to `in-progress`.
    InProgress,
    /// Move to `blocked`, carrying the required reason. The `blocked-by`
    /// dependency list is managed separately and untouched by this change.
    Blocked {
        /// Free-form reason the node is blocked (required, non-blank).
        reason: Option<String>,
    },
    /// Move to `done` (allowed only when all descendants are terminal).
    Done,
    /// Move to `closed`, with a required reason and optional cascade.
    Closed {
        /// Why it is being resolved without completing.
        reason: Option<String>,
        /// Whether to cascade the close to non-terminal descendants.
        cascade: bool,
    },
}

/// The result of a node status change: the updated node, plus the descendants a
/// cascade close resolved along with it. `cascaded_closed` lists their node
/// numbers in ascending order and is empty unless a cascade actually closed
/// something, so a caller can report the wider effect a cascade had.
#[derive(Debug, Clone)]
pub struct StatusOutcome {
    /// The node whose status was set.
    pub node: NodeFile,
    /// Descendants closed by a cascade, ascending; empty for a non-cascade or
    /// non-closing change.
    pub cascaded_closed: Vec<u64>,
}

/// Apply a node state transition, enforcing the state machine. Returns the
/// updated node and any descendants a cascade close resolved with it. A default
/// close resolves only the node itself and leaves its descendants untouched, so
/// it can be reopened without having rewritten the subtree; a cascade close
/// also closes the descendants (ascending order, per-file, idempotent), and a
/// partial failure is reported.
pub fn set_node_status(
    store: &Store,
    node_ref: &NodeRef,
    change: NodeStatusChange,
) -> Result<StatusOutcome, OpError> {
    require_node(store, node_ref)?;
    let mut cascaded_closed: Vec<u64> = Vec::new();

    // The transition is decided once, here: what the state machine permits, and
    // the three fields the new state leaves behind. Everything in this match that
    // touches the store touches *other* nodes — a subtree's states, and the
    // cascade that closes them — so it runs before this node's lock is taken:
    // each cascade step locks its own target, and nesting them inside this node's
    // lock would order the same locks two ways.
    let (status, block_reason, close_reason) = match change {
        // Leaving `blocked`/`closed` clears their reasons; the blocked-by
        // dependency list is independent of status and left untouched.
        NodeStatusChange::ToDo => (NodeState::ToDo, None, None),
        NodeStatusChange::InProgress => (NodeState::InProgress, None, None),
        NodeStatusChange::Blocked { reason } => {
            // `blocked` is set only explicitly (this arm) and must carry a
            // non-blank reason. blocked-by is managed separately, never here.
            let has_reason = reason.as_deref().map(|r| !r.trim().is_empty()) == Some(true);
            validate_blocked(true, has_reason)?;
            (NodeState::Blocked, reason, None)
        }
        NodeStatusChange::Done => {
            let descendants = descendants_of(store, node_ref)?;
            validate_done(&descendants)?;
            // `done` is terminal but not `closed`; it never carries a reason.
            (NodeState::Done, None, None)
        }
        NodeStatusChange::Closed { reason, cascade } => {
            let descendants = descendants_of(store, node_ref)?;
            let cascade_sel = if cascade { Cascade::Yes } else { Cascade::No };
            let plan = plan_close(&descendants, reason.as_deref(), cascade_sel)?;
            // Close the descendants first (ascending order, deadlock-free), so
            // a re-run after a partial failure still converges. Each is an
            // independent bracketed read-modify-write.
            if !plan.cascade_targets.is_empty() {
                close_descendants(
                    store,
                    &node_ref.epic_id,
                    &plan.cascade_targets,
                    reason.clone(),
                )?;
                cascaded_closed = plan.cascade_targets.clone();
            }
            (NodeState::Closed, None, reason)
        }
    };

    let (node, ()) = modify_node(store, node_ref, None, |node| {
        node.frontmatter.status = status;
        node.frontmatter.block_reason = block_reason;
        node.frontmatter.close_reason = close_reason;
        node.frontmatter.updated = now();
        Ok(())
    })?;
    Ok(StatusOutcome {
        node,
        cascaded_closed,
    })
}

/// Close each listed descendant, in the given (ascending) order, as an
/// independent bracketed read-modify-write. Already-terminal descendants are
/// left exactly as they are, and because the terminal test is made against the
/// descendant read under its own lock, a descendant that another actor resolved
/// meanwhile is not overwritten. Stops at the first failure and reports partial
/// progress.
fn close_descendants(
    store: &Store,
    epic_id: &str,
    targets: &[u64],
    reason: Option<String>,
) -> Result<(), OpError> {
    let paths: Vec<std::path::PathBuf> = targets
        .iter()
        .map(|n| store.node_path(epic_id, *n))
        .collect();
    let report = lock::cascade(paths, |path| {
        // Recover the node number from the path to address it via the store.
        let number = targets
            .iter()
            .copied()
            .find(|n| store.node_path(epic_id, *n) == path)
            .expect("cascade visits only the listed targets");
        store
            .modify_node(epic_id, number, None, |child| {
                // Idempotent: a descendant already terminal needs no rewrite, so
                // the change leaves it as found and nothing is published.
                if child.frontmatter.status.is_terminal() {
                    return Ok::<(), StoreError>(());
                }
                child.frontmatter.status = NodeState::Closed;
                child.frontmatter.close_reason = reason.clone();
                child.frontmatter.block_reason = None;
                child.frontmatter.updated = now();
                Ok(())
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    });

    if let Some((failed_path, reason)) = report.failed {
        // Map the failed path back to a number for a clean reference.
        let number = targets
            .iter()
            .copied()
            .find(|n| store.node_path(epic_id, *n) == failed_path)
            .unwrap_or(0);
        return Err(OpError::CascadePartial {
            failed: NodeRef::new(epic_id, number),
            reason,
        });
    }
    Ok(())
}

/// Toggle an epic's stored closed flag. Closing may carry a reason; reopening
/// clears both the flag and any stored reason. Bumps `updated`.
pub fn set_epic_closed(
    store: &Store,
    epic_id: &str,
    closed: bool,
    reason: Option<String>,
) -> Result<EpicFile, OpError> {
    let (epic, ()) = modify_epic(store, epic_id, None, |epic| {
        epic.frontmatter.closed = closed;
        epic.frontmatter.close_reason = if closed { reason } else { None };
        epic.frontmatter.updated = now();
        Ok(())
    })?;
    Ok(epic)
}

// ---------------------------------------------------------------------------
// labels
// ---------------------------------------------------------------------------

/// Add labels to a target's set (deduplicated). Returns the resulting label
/// list. Bumps `updated`.
pub fn add_labels(
    store: &Store,
    target: &Target,
    labels: &[String],
) -> Result<Vec<String>, OpError> {
    with_labels(store, target, |existing| {
        let mut merged = existing.clone();
        for l in labels {
            if !merged.contains(l) {
                merged.push(l.clone());
            }
        }
        merged
    })
}

/// Remove labels from a target's set. Removing an absent label is a no-op.
/// Returns the resulting label list. Bumps `updated`.
pub fn remove_labels(
    store: &Store,
    target: &Target,
    labels: &[String],
) -> Result<Vec<String>, OpError> {
    with_labels(store, target, |existing| {
        existing
            .iter()
            .filter(|l| !labels.contains(l))
            .cloned()
            .collect()
    })
}

/// List a target's labels.
pub fn list_labels(store: &Store, target: &Target) -> Result<Vec<String>, OpError> {
    Ok(match target {
        Target::Epic(id) => read_epic(store, id)?.frontmatter.labels,
        Target::Node(r) => read_node(store, r)?.frontmatter.labels,
    })
}

/// Read a target's labels under its lock, transform them, publish and return the
/// new set. Shared by add/remove so both take the bracketed path, which is what
/// makes a merge a merge: the set transformed is the set as stored, so a label
/// another actor added moments earlier survives.
fn with_labels(
    store: &Store,
    target: &Target,
    transform: impl FnOnce(&Vec<String>) -> Vec<String>,
) -> Result<Vec<String>, OpError> {
    match target {
        Target::Epic(id) => {
            let (epic, ()) = modify_epic(store, id, None, |epic| {
                epic.frontmatter.labels = transform(&epic.frontmatter.labels);
                epic.frontmatter.updated = now();
                Ok(())
            })?;
            Ok(epic.frontmatter.labels)
        }
        Target::Node(r) => {
            let (node, ()) = modify_node(store, r, None, |node| {
                node.frontmatter.labels = transform(&node.frontmatter.labels);
                node.frontmatter.updated = now();
                Ok(())
            })?;
            Ok(node.frontmatter.labels)
        }
    }
}

// ---------------------------------------------------------------------------
// blocked-by (node-only dependency list)
// ---------------------------------------------------------------------------

/// Validate one proposed `blocked-by` blocker and return its canonical
/// `<epic-id>/<n>` string. A blocker must address an existing node and may not
/// be the node itself; its *state* is irrelevant (a terminal node may block).
/// Cross-epic blockers are allowed. The list is a dependency annotation, so no
/// cycle check is performed.
fn validate_blocker(
    store: &Store,
    node_ref: &NodeRef,
    blocker: &NodeRef,
) -> Result<String, OpError> {
    if blocker == node_ref {
        return Err(OpError::BlockedBySelf(blocker.clone()));
    }
    if !store.node_path(&blocker.epic_id, blocker.number).is_file() {
        return Err(OpError::NoSuchNode(blocker.clone()));
    }
    Ok(blocker.to_string())
}

/// Add blockers to a node's `blocked-by` list (deduplicated, first-seen order).
/// Each blocker must exist and not be the node itself. Bumps `updated`.
pub fn add_blocked_by(
    store: &Store,
    node_ref: &NodeRef,
    blockers: &[NodeRef],
) -> Result<Vec<String>, OpError> {
    let mut canonical = Vec::with_capacity(blockers.len());
    for b in blockers {
        canonical.push(validate_blocker(store, node_ref, b)?);
    }
    with_blocked_by(store, node_ref, |existing| {
        let mut merged = existing.clone();
        for c in canonical {
            if !merged.contains(&c) {
                merged.push(c);
            }
        }
        merged
    })
}

/// Remove blockers from a node's `blocked-by` list. Removing an absent blocker
/// is a no-op. Blockers are matched by canonical form, so `<n>` and
/// `<epic-id>/<n>` are resolved by the caller before reaching here. Bumps
/// `updated`.
pub fn remove_blocked_by(
    store: &Store,
    node_ref: &NodeRef,
    blockers: &[NodeRef],
) -> Result<Vec<String>, OpError> {
    let drop: Vec<String> = blockers.iter().map(NodeRef::to_string).collect();
    with_blocked_by(store, node_ref, |existing| {
        existing
            .iter()
            .filter(|c| !drop.contains(c))
            .cloned()
            .collect()
    })
}

/// Replace a node's `blocked-by` list wholesale. Every blocker must exist and
/// not be the node itself. Bumps `updated`.
pub fn set_blocked_by(
    store: &Store,
    node_ref: &NodeRef,
    blockers: &[NodeRef],
) -> Result<Vec<String>, OpError> {
    let mut canonical = Vec::with_capacity(blockers.len());
    for b in blockers {
        let c = validate_blocker(store, node_ref, b)?;
        if !canonical.contains(&c) {
            canonical.push(c);
        }
    }
    with_blocked_by(store, node_ref, |_existing| canonical.clone())
}

/// Empty a node's `blocked-by` list. Bumps `updated`.
pub fn clear_blocked_by(store: &Store, node_ref: &NodeRef) -> Result<Vec<String>, OpError> {
    with_blocked_by(store, node_ref, |_existing| Vec::new())
}

/// List a node's `blocked-by` dependencies (canonical refs).
pub fn list_blocked_by(store: &Store, node_ref: &NodeRef) -> Result<Vec<String>, OpError> {
    Ok(read_node(store, node_ref)?.frontmatter.blocked_by)
}

/// Read a node's `blocked-by` list under its lock, transform it, publish and
/// return the new list. Shared by add/remove/set/clear so each takes the
/// bracketed path and transforms the list as stored.
fn with_blocked_by(
    store: &Store,
    node_ref: &NodeRef,
    transform: impl FnOnce(&Vec<String>) -> Vec<String>,
) -> Result<Vec<String>, OpError> {
    let (node, ()) = modify_node(store, node_ref, None, |node| {
        node.frontmatter.blocked_by = transform(&node.frontmatter.blocked_by);
        node.frontmatter.updated = now();
        Ok(())
    })?;
    Ok(node.frontmatter.blocked_by)
}

// ---------------------------------------------------------------------------
// claim (node-only single-holder)
// ---------------------------------------------------------------------------

/// Take (or reassign) a node's single-holder claim.
///
/// The identifier is freeform and must be non-blank (a blank one is refused);
/// `at` is stamped now. A node has at most one claim, so this overwrites any
/// existing holder — reassigning is just re-taking. Returns the updated node
/// and the prior holder (if the node was already claimed) so the caller can
/// report a reassignment. Bumps `updated`.
pub fn take_claim(
    store: &Store,
    node_ref: &NodeRef,
    identifier: &str,
) -> Result<(NodeFile, Option<String>), OpError> {
    let identifier = identifier.trim();
    if identifier.is_empty() {
        return Err(OpError::EmptyClaimIdentifier);
    }
    // The holder reported as prior is the one stored when the claim was taken,
    // read under the same lock that publishes the new one — so a reassignment
    // never names a holder that had already been replaced.
    modify_node(store, node_ref, None, |node| {
        let prior = node.frontmatter.claim.as_ref().map(|c| c.by.clone());
        // The claim timestamp and the node's `updated` stamp share one instant,
        // so "when it was claimed" never drifts from "when the file last
        // changed".
        let ts = now();
        // by and at are always set together, so a claim never carries a holder
        // without a timestamp.
        node.frontmatter.claim = Some(Claim {
            by: identifier.to_string(),
            at: ts,
        });
        node.frontmatter.updated = ts;
        Ok(prior)
    })
}

/// Release a node's claim, dropping the identifier and timestamp together.
///
/// Releasing an unclaimed node is a no-op on the claim (there is nothing to
/// drop). Returns the updated node and the prior holder (if any). Bumps
/// `updated`.
pub fn release_claim(
    store: &Store,
    node_ref: &NodeRef,
) -> Result<(NodeFile, Option<String>), OpError> {
    modify_node(store, node_ref, None, |node| {
        let prior = node.frontmatter.claim.take().map(|c| c.by);
        node.frontmatter.updated = now();
        Ok(prior)
    })
}

// ---------------------------------------------------------------------------
// comments
// ---------------------------------------------------------------------------

/// Add a comment to a target, authored by `actor`. The id is `max(existing)+1`
/// and never reused. Returns the new comment.
pub fn add_comment(
    store: &Store,
    target: &Target,
    actor: Actor,
    text: String,
) -> Result<Comment, OpError> {
    // An append names no expected stamp: it adds a slot of its own, so a
    // concurrent change cannot be discarded by it and both survive.
    with_comments(store, target, None, |comments| {
        let comment = Comment {
            id: next_comment_id(comments),
            author: actor,
            created: now(),
            text,
            deleted: false,
        };
        comments.push(comment.clone());
        Ok(comment)
    })
}

/// Edit a comment's text — own author only, and not an already-deleted comment.
/// Returns the edited comment.
///
/// The text is a whole-field replacement, so it takes the opt-in precondition:
/// with `expect_updated` set the edit applies only while the target's stored
/// `updated` stamp still equals it, and otherwise refuses with
/// [`StoreError::Conflict`] having written nothing. `None` applies
/// unconditionally (last write wins). The stamp covers the whole epic or node,
/// not the one comment, so any concurrent change to the target refuses the edit.
pub fn edit_comment(
    store: &Store,
    target: &Target,
    comment_id: u64,
    actor: Actor,
    text: String,
    expect_updated: Option<Timestamp>,
) -> Result<Comment, OpError> {
    with_comments(store, target, expect_updated, |comments| {
        let slot = comments
            .iter_mut()
            .find(|c| c.id == comment_id)
            .ok_or(OpError::NoSuchComment(comment_id))?;
        authorize_comment_mutation(&slot.author, slot.deleted, &actor)?;
        slot.text = text;
        Ok(slot.clone())
    })
}

/// Soft-delete a comment — own author only, and not one already deleted. The
/// comment is retained (flag set) so its id is never reused. Returns the
/// tombstoned comment.
pub fn delete_comment(
    store: &Store,
    target: &Target,
    comment_id: u64,
    actor: Actor,
) -> Result<Comment, OpError> {
    // A tombstone replaces no text of anyone's: it flags one slot, so it names
    // no expected stamp either.
    with_comments(store, target, None, |comments| {
        let slot = comments
            .iter_mut()
            .find(|c| c.id == comment_id)
            .ok_or(OpError::NoSuchComment(comment_id))?;
        authorize_comment_mutation(&slot.author, slot.deleted, &actor)?;
        slot.deleted = true;
        Ok(slot.clone())
    })
}

/// One entry as a comment listing sees it: either the full comment, or a
/// tombstone (author + timestamp, text withheld) for a soft-deleted one shown
/// under `--include-deleted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentView {
    /// A live comment, shown in full.
    Live(Comment),
    /// A soft-deleted comment shown as a tombstone: its text is withheld.
    Tombstone {
        /// The comment id.
        id: u64,
        /// Who authored it.
        author: Actor,
        /// When it was written.
        created: Timestamp,
    },
}

/// List a target's comments. Soft-deleted comments are hidden by default and,
/// when `include_deleted` is set, shown as tombstones (text withheld).
pub fn list_comments(
    store: &Store,
    target: &Target,
    include_deleted: bool,
) -> Result<Vec<CommentView>, OpError> {
    let comments = match target {
        Target::Epic(id) => read_epic(store, id)?.frontmatter.comments,
        Target::Node(r) => read_node(store, r)?.frontmatter.comments,
    };
    let mut out = Vec::new();
    for c in comments {
        if !domain::comment_is_visible(c.deleted, include_deleted) {
            continue;
        }
        out.push(if c.deleted {
            CommentView::Tombstone {
                id: c.id,
                author: c.author,
                created: c.created,
            }
        } else {
            CommentView::Live(c)
        });
    }
    Ok(out)
}

/// Read a target's comment list under its lock, mutate it, publish, and return
/// whatever the transform produced. Shared by add/edit/delete, so each takes the
/// bracketed path; an append therefore lands beside a comment added moments
/// earlier instead of replacing the list it was appended to. `expect_updated`
/// carries the caller's precondition (`None` = unconditional).
fn with_comments<T>(
    store: &Store,
    target: &Target,
    expect_updated: Option<Timestamp>,
    transform: impl FnOnce(&mut Vec<Comment>) -> Result<T, OpError>,
) -> Result<T, OpError> {
    match target {
        Target::Epic(id) => {
            let (_, out) = modify_epic(store, id, expect_updated, |epic| {
                let out = transform(&mut epic.frontmatter.comments)?;
                epic.frontmatter.updated = now();
                Ok(out)
            })?;
            Ok(out)
        }
        Target::Node(r) => {
            let (_, out) = modify_node(store, r, expect_updated, |node| {
                let out = transform(&mut node.frontmatter.comments)?;
                node.frontmatter.updated = now();
                Ok(out)
            })?;
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------------------
// assets
// ---------------------------------------------------------------------------

/// Add an asset to a target: copy the bytes verbatim into the companion
/// directory and insert the frontmatter index entry. `name` is required (the
/// CLI derives a default from the --file basename before calling). `add` is
/// create-only — a name already indexed here is refused with `AssetExists`;
/// replacing an existing asset is `update_asset`'s job. Returns the new entry.
pub fn add_asset(
    store: &Store,
    target: &Target,
    name: &str,
    description: Option<String>,
    bytes: &[u8],
) -> Result<Asset, OpError> {
    let entry = Asset {
        name: name.to_string(),
        description,
    };
    match target {
        Target::Epic(id) => {
            modify_epic(store, id, None, |epic| {
                // Reject a duplicate before landing bytes, so a refused add never
                // touches the companion dir. `add` creates; `update` replaces.
                if !insert_asset(&mut epic.frontmatter.assets, entry.clone()) {
                    return Err(OpError::AssetExists(name.to_string()));
                }
                // The bytes land before the index entry is published, so a crash
                // leaves an orphan file rather than an index entry pointing at
                // nothing. They land while the epic's lock is held: an entity's
                // lock is only ever taken before its asset's, never the other
                // way round, so the nesting cannot deadlock.
                store.copy_epic_asset(id, name, bytes)?;
                epic.frontmatter.updated = now();
                Ok(())
            })?;
        }
        Target::Node(r) => {
            modify_node(store, r, None, |node| {
                if !insert_asset(&mut node.frontmatter.assets, entry.clone()) {
                    return Err(OpError::AssetExists(name.to_string()));
                }
                store.copy_node_asset(&r.epic_id, r.number, name, bytes)?;
                node.frontmatter.updated = now();
                Ok(())
            })?;
        }
    }
    Ok(entry)
}

/// Hard-delete an asset: remove both the index entry and the bytes. Refuses if
/// the name is not indexed. Returns the removed entry.
pub fn delete_asset(store: &Store, target: &Target, name: &str) -> Result<Asset, OpError> {
    match target {
        Target::Epic(id) => {
            let (_, removed) = modify_epic(store, id, None, |epic| {
                let removed = remove_asset(&mut epic.frontmatter.assets, name)
                    .ok_or_else(|| OpError::NoSuchAsset(name.to_string()))?;
                epic.frontmatter.updated = now();
                Ok(removed)
            })?;
            // Bytes are removed after the index so a crash leaves an orphan
            // file, never a dangling index entry. A missing file is tolerated.
            let _ = store.remove_epic_asset(id, name);
            Ok(removed)
        }
        Target::Node(r) => {
            let (_, removed) = modify_node(store, r, None, |node| {
                let removed = remove_asset(&mut node.frontmatter.assets, name)
                    .ok_or_else(|| OpError::NoSuchAsset(name.to_string()))?;
                node.frontmatter.updated = now();
                Ok(removed)
            })?;
            let _ = store.remove_node_asset(&r.epic_id, r.number, name);
            Ok(removed)
        }
    }
}

/// Read an asset's bytes. Refuses if the name is not indexed, so a caller never
/// races the index against stray files on disk — the index is authoritative.
pub fn read_asset(store: &Store, target: &Target, name: &str) -> Result<Vec<u8>, OpError> {
    match target {
        Target::Epic(id) => {
            let epic = read_epic(store, id)?;
            if !epic.frontmatter.assets.iter().any(|a| a.name == name) {
                return Err(OpError::NoSuchAsset(name.to_string()));
            }
            Ok(store.read_epic_asset(id, name)?)
        }
        Target::Node(r) => {
            let node = read_node(store, r)?;
            if !node.frontmatter.assets.iter().any(|a| a.name == name) {
                return Err(OpError::NoSuchAsset(name.to_string()));
            }
            Ok(store.read_node_asset(&r.epic_id, r.number, name)?)
        }
    }
}

/// Update an existing asset in place: replace its bytes and/or its description.
/// Refuses if the name is not indexed (use `add_asset` to create). `description`
/// is `Some` to set/clear it, `None` to leave it; `bytes` is `Some` to replace
/// the payload, `None` to leave it. The caller guarantees at least one is set —
/// a no-op update has nothing to persist.
pub fn update_asset(
    store: &Store,
    target: &Target,
    name: &str,
    description: Option<Option<String>>,
    bytes: Option<&[u8]>,
) -> Result<Asset, OpError> {
    match target {
        Target::Epic(id) => {
            let (_, updated) = modify_epic(store, id, None, |epic| {
                let entry = epic
                    .frontmatter
                    .assets
                    .iter_mut()
                    .find(|a| a.name == name)
                    .ok_or_else(|| OpError::NoSuchAsset(name.to_string()))?;
                if let Some(desc) = description {
                    entry.description = desc;
                }
                let updated = entry.clone();
                // Land replacement bytes first (an overwrite), then publish the
                // index entry; the name is unchanged so no stale file is
                // orphaned.
                if let Some(bytes) = bytes {
                    store.copy_epic_asset(id, name, bytes)?;
                }
                epic.frontmatter.updated = now();
                Ok(updated)
            })?;
            Ok(updated)
        }
        Target::Node(r) => {
            let (_, updated) = modify_node(store, r, None, |node| {
                let entry = node
                    .frontmatter
                    .assets
                    .iter_mut()
                    .find(|a| a.name == name)
                    .ok_or_else(|| OpError::NoSuchAsset(name.to_string()))?;
                if let Some(desc) = description {
                    entry.description = desc;
                }
                let updated = entry.clone();
                if let Some(bytes) = bytes {
                    store.copy_node_asset(&r.epic_id, r.number, name, bytes)?;
                }
                node.frontmatter.updated = now();
                Ok(updated)
            })?;
            Ok(updated)
        }
    }
}

/// List a target's assets (the index entries).
pub fn list_assets(store: &Store, target: &Target) -> Result<Vec<Asset>, OpError> {
    Ok(match target {
        Target::Epic(id) => read_epic(store, id)?.frontmatter.assets,
        Target::Node(r) => read_node(store, r)?.frontmatter.assets,
    })
}

// ---------------------------------------------------------------------------
// tree / descendant resolution
// ---------------------------------------------------------------------------

/// Load every node file of an epic by scanning its directory for `<n>.md`
/// files. This is the authoritative source for reconstructing the tree from the
/// `parent` fields; the store keeps no separate index.
pub fn load_epic_nodes(store: &Store, epic_id: &str) -> Result<Vec<NodeFile>, OpError> {
    let dir = store.epic_dir(epic_id);
    if !store.epic_path(epic_id).is_file() {
        return Err(OpError::NoSuchEpic(epic_id.to_string()));
    }
    let mut nodes = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        // An epic with no node files yet: no nodes.
        Err(_) => return Ok(nodes),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Node files are `<n>.md` where `<n>` is all digits; skip epic.md,
        // companion dirs and anything else.
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let is_md = path.extension().and_then(|e| e.to_str()) == Some("md");
        if !is_md || !stem.chars().all(|c| c.is_ascii_digit()) || stem.is_empty() {
            continue;
        }
        let Ok(number) = stem.parse::<u64>() else {
            continue;
        };
        nodes.push(store.read_node(epic_id, number)?);
    }
    Ok(nodes)
}

/// The recursive descendants of a node, as `(number, state)` pairs, for the
/// state-machine checks. Resolves the tree from every node's `parent` field.
pub fn descendants_of(store: &Store, node_ref: &NodeRef) -> Result<Vec<NodeStatus>, OpError> {
    let nodes = load_epic_nodes(store, &node_ref.epic_id)?;

    // children[parent] = the direct children of that parent number.
    let mut children: std::collections::HashMap<u64, Vec<u64>> = std::collections::HashMap::new();
    let mut state_of: std::collections::HashMap<u64, NodeState> = std::collections::HashMap::new();
    for n in &nodes {
        state_of.insert(n.frontmatter.number, n.frontmatter.status);
        if let Some(parent) = n.frontmatter.parent {
            children
                .entry(parent)
                .or_default()
                .push(n.frontmatter.number);
        }
    }

    // Breadth-first walk from the node's direct children downward. A `visited`
    // set makes a malformed pre-existing cycle terminate rather than loop.
    let mut out = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
    if let Some(direct) = children.get(&node_ref.number) {
        queue.extend(direct.iter().copied());
    }
    while let Some(number) = queue.pop_front() {
        if !visited.insert(number) {
            continue;
        }
        if let Some(state) = state_of.get(&number) {
            out.push(NodeStatus::new(number, *state));
        }
        if let Some(grandchildren) = children.get(&number) {
            queue.extend(grandchildren.iter().copied());
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// existence-checked reads (mapping "not found" to a clean op error)
// ---------------------------------------------------------------------------

// These are the readers a caller composing a write goes through: an entity must
// be read before it can be edited, and "it is gone" is an operation-level answer
// rather than a path in an I/O message. The rule lives here once so no surface
// restates it.

/// Read an epic, mapping a missing file to [`OpError::NoSuchEpic`] rather than a
/// raw I/O error.
pub fn read_epic(store: &Store, epic_id: &str) -> Result<EpicFile, OpError> {
    require_epic(store, epic_id)?;
    Ok(store.read_epic(epic_id)?)
}

/// Read a node, mapping a missing file to [`OpError::NoSuchNode`].
pub fn read_node(store: &Store, node_ref: &NodeRef) -> Result<NodeFile, OpError> {
    require_node(store, node_ref)?;
    Ok(store.read_node(&node_ref.epic_id, node_ref.number)?)
}

/// The epic-existence rule, stated once: addressing an epic that is not there is
/// an operation-level answer, not an I/O message.
fn require_epic(store: &Store, epic_id: &str) -> Result<(), OpError> {
    if !store.epic_path(epic_id).is_file() {
        return Err(OpError::NoSuchEpic(epic_id.to_string()));
    }
    Ok(())
}

/// The node twin of [`require_epic`].
fn require_node(store: &Store, node_ref: &NodeRef) -> Result<(), OpError> {
    if !store
        .node_path(&node_ref.epic_id, node_ref.number)
        .is_file()
    {
        return Err(OpError::NoSuchNode(node_ref.clone()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// bracketed read-modify-write
// ---------------------------------------------------------------------------

// Every operation that depends on what is stored goes through one of these two,
// so the lock is held from before the entity is read until the change is
// published. What the change is handed is the entity as stored at that moment,
// which is what makes an operation's merge safe and its precondition meaningful.
//
// Existence is checked before the lock so a caller addressing something that is
// not there gets the operation-level answer rather than an I/O error against a
// path it never named. Under the lock the store answers again: an entity that
// vanished in between refuses the write rather than re-creating it from a read
// that no longer describes the store.

/// Read-modify-write an epic with the whole sequence under its lock.
fn modify_epic<T>(
    store: &Store,
    epic_id: &str,
    expect_updated: Option<Timestamp>,
    change: impl FnOnce(&mut EpicFile) -> Result<T, OpError>,
) -> Result<(EpicFile, T), OpError> {
    require_epic(store, epic_id)?;
    store.modify_epic(epic_id, expect_updated, change)
}

/// Read-modify-write a node with the whole sequence under its lock.
fn modify_node<T>(
    store: &Store,
    node_ref: &NodeRef,
    expect_updated: Option<Timestamp>,
    change: impl FnOnce(&mut NodeFile) -> Result<T, OpError>,
) -> Result<(NodeFile, T), OpError> {
    require_node(store, node_ref)?;
    store.modify_node(&node_ref.epic_id, node_ref.number, expect_updated, change)
}

/// The default force policy for CLI-driven writes: a stale lock fails fast so an
/// operator can decide. A `--force` path plugs in here later.
pub const DEFAULT_FORCE: Force = Force::Deny;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::LockConfig;
    use std::time::Duration;

    fn fast_store(root: &std::path::Path) -> Store {
        Store::at(root).with_lock_config(LockConfig {
            stale_threshold: Duration::from_millis(80),
            retry_interval: Duration::from_millis(5),
        })
    }

    /// Lock tunables for a test that holds a lock on purpose: the waiter must be
    /// able to sit the hold out rather than declare it abandoned, so the
    /// threshold is far longer than the hold and the retry interval stays short.
    fn patient_config() -> LockConfig {
        LockConfig {
            stale_threshold: Duration::from_secs(5),
            retry_interval: Duration::from_millis(5),
        }
    }

    fn patient_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path()).with_lock_config(patient_config());
        (dir, store)
    }

    /// How long a deliberately-held lock is held before the competing change is
    /// published through it: long enough that the waiting operation is certainly
    /// past the point where an unbracketed read would already have happened.
    const HOLD: Duration = Duration::from_millis(50);

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

    fn new_node(epic: &str, parent: Option<NodeRef>) -> NewNode {
        NewNode {
            epic_id: epic.to_string(),
            parent,
            name: "a node".into(),
            summary: "slice".into(),
            labels: vec![],
            body: String::new(),
        }
    }

    // -- create ------------------------------------------------------------

    #[test]
    fn create_epic_then_refuse_duplicate() {
        let (_d, s) = seeded();
        let e = create_epic(&s, new_epic("e")).unwrap();
        assert_eq!(e.frontmatter.id, "e");
        assert_eq!(e.frontmatter.next_number, 1);
        assert!(!e.frontmatter.closed);
        assert!(matches!(
            create_epic(&s, new_epic("e")),
            Err(OpError::EpicExists(_))
        ));
    }

    #[test]
    fn create_epic_dedups_labels() {
        let (_d, s) = seeded();
        let mut ne = new_epic("e");
        ne.labels = vec!["a".into(), "b".into(), "a".into()];
        let e = create_epic(&s, ne).unwrap();
        assert_eq!(e.frontmatter.labels, vec!["a", "b"]);
    }

    #[test]
    fn create_node_allocates_and_defaults_to_to_do() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        assert_eq!(n.frontmatter.number, 1);
        assert_eq!(n.frontmatter.status, NodeState::ToDo);
        assert_eq!(n.frontmatter.parent, None);
    }

    #[test]
    fn create_node_requires_the_epic() {
        let (_d, s) = seeded();
        assert!(matches!(
            create_node(&s, new_node("ghost", None)),
            Err(OpError::NoSuchEpic(_))
        ));
    }

    #[test]
    fn create_subticket_sets_parent_and_validates_it() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let parent = create_node(&s, new_node("e", None)).unwrap();
        let child = create_node(
            &s,
            new_node("e", Some(NodeRef::new("e", parent.frontmatter.number))),
        )
        .unwrap();
        assert_eq!(child.frontmatter.parent, Some(parent.frontmatter.number));
    }

    #[test]
    fn create_subticket_refuses_missing_parent() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        assert!(matches!(
            create_node(&s, new_node("e", Some(NodeRef::new("e", 99)))),
            Err(OpError::NoSuchNode(_))
        ));
    }

    #[test]
    fn create_subticket_refuses_cross_epic_parent() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        create_epic(&s, new_epic("other")).unwrap();
        create_node(&s, new_node("other", None)).unwrap();
        assert!(matches!(
            create_node(&s, new_node("e", Some(NodeRef::new("other", 1)))),
            Err(OpError::ParentInDifferentEpic { .. })
        ));
    }

    // -- edit --------------------------------------------------------------

    #[test]
    fn edit_epic_scalars_and_bumps_updated() {
        let (_d, s) = seeded();
        let created = create_epic(&s, new_epic("e")).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let edited = edit_epic(
            &s,
            "e",
            EpicEdits {
                name: Some("new name".into()),
                body: Some("new body\n".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(edited.frontmatter.name, "new name");
        assert_eq!(edited.body, "new body\n");
        assert!(edited.frontmatter.updated >= created.frontmatter.updated);
    }

    // -- the opt-in `updated` precondition ---------------------------------

    #[test]
    fn an_epic_edit_naming_the_read_stamp_applies_and_a_stale_stamp_refuses() {
        let (_d, s) = seeded();
        let created = create_epic(&s, new_epic("e")).unwrap();

        // The stamp read with the epic still stands, so the replacement applies.
        let edited = edit_epic(
            &s,
            "e",
            EpicEdits {
                body: Some("mine\n".into()),
                expect_updated: Some(created.frontmatter.updated),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(edited.body, "mine\n");

        // An unrelated write to the same epic bumps `updated`, so the stamp taken
        // before it is stale: the next replacement is refused, having written
        // nothing. Granularity is the entity, not the field.
        let stale = edited.frontmatter.updated;
        add_labels(&s, &Target::Epic("e".into()), &["x".into()]).unwrap();
        assert!(matches!(
            edit_epic(
                &s,
                "e",
                EpicEdits {
                    body: Some("theirs\n".into()),
                    expect_updated: Some(stale),
                    ..Default::default()
                },
            ),
            Err(OpError::Store(StoreError::Conflict { .. }))
        ));
        assert_eq!(s.read_epic("e").unwrap().body, "mine\n");

        // Naming no stamp keeps last-write-wins.
        edit_epic(
            &s,
            "e",
            EpicEdits {
                body: Some("theirs\n".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.read_epic("e").unwrap().body, "theirs\n");
    }

    #[test]
    fn a_node_edit_naming_the_read_stamp_applies_and_a_stale_stamp_refuses() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let created = create_node(&s, new_node("e", None)).unwrap();
        let r = NodeRef::new("e", created.frontmatter.number);

        let edited = edit_node(
            &s,
            &r,
            NodeEdits {
                body: Some("mine\n".into()),
                expect_updated: Some(created.frontmatter.updated),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(edited.body, "mine\n");

        // A claim landing mid-edit is an unrelated change to the same node, and
        // refuses the stamp taken before it.
        let stale = edited.frontmatter.updated;
        take_claim(&s, &r, "alice").unwrap();
        assert!(matches!(
            edit_node(
                &s,
                &r,
                NodeEdits {
                    body: Some("theirs\n".into()),
                    expect_updated: Some(stale),
                    ..Default::default()
                },
            ),
            Err(OpError::Store(StoreError::Conflict { .. }))
        ));
        assert_eq!(s.read_node("e", r.number).unwrap().body, "mine\n");

        edit_node(
            &s,
            &r,
            NodeEdits {
                body: Some("theirs\n".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.read_node("e", r.number).unwrap().body, "theirs\n");
    }

    #[test]
    fn a_comment_edit_naming_the_read_stamp_applies_and_a_stale_stamp_refuses() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let target = Target::Epic("e".into());
        let c = add_comment(&s, &target, Actor::Human, "first".into()).unwrap();

        let stamp = s.read_epic("e").unwrap().frontmatter.updated;
        let edited = edit_comment(
            &s,
            &target,
            c.id,
            Actor::Human,
            "second".into(),
            Some(stamp),
        )
        .unwrap();
        assert_eq!(edited.text, "second");

        // Another comment arriving is exactly the accepted cost: it bumps the
        // epic's stamp, so the older one no longer applies.
        add_comment(&s, &target, Actor::Human, "meanwhile".into()).unwrap();
        assert!(matches!(
            edit_comment(&s, &target, c.id, Actor::Human, "third".into(), Some(stamp)),
            Err(OpError::Store(StoreError::Conflict { .. }))
        ));

        // Naming no stamp applies regardless.
        let forced = edit_comment(&s, &target, c.id, Actor::Human, "third".into(), None).unwrap();
        assert_eq!(forced.text, "third");
    }

    #[test]
    fn edit_node_reparent_validates_same_epic_and_existence() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let a = create_node(&s, new_node("e", None)).unwrap();
        let b = create_node(&s, new_node("e", None)).unwrap();
        let br = NodeRef::new("e", b.frontmatter.number);
        let reparented = edit_node(
            &s,
            &br,
            NodeEdits {
                parent: Some(NodeRef::new("e", a.frontmatter.number)),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reparented.frontmatter.parent, Some(a.frontmatter.number));
    }

    #[test]
    fn edit_node_reparent_refuses_self() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let a = create_node(&s, new_node("e", None)).unwrap();
        let ar = NodeRef::new("e", a.frontmatter.number);
        assert!(matches!(
            edit_node(
                &s,
                &ar,
                NodeEdits {
                    parent: Some(ar.clone()),
                    ..Default::default()
                }
            ),
            Err(OpError::ReparentCycle { .. })
        ));
    }

    #[test]
    fn edit_node_reparent_refuses_descendant_cycle() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        // a <- b <- c ; reparenting a under c would be a cycle.
        let a = create_node(&s, new_node("e", None)).unwrap();
        let ar = NodeRef::new("e", a.frontmatter.number);
        let b = create_node(&s, new_node("e", Some(ar.clone()))).unwrap();
        let br = NodeRef::new("e", b.frontmatter.number);
        let c = create_node(&s, new_node("e", Some(br))).unwrap();
        let cr = NodeRef::new("e", c.frontmatter.number);
        assert!(matches!(
            edit_node(
                &s,
                &ar,
                NodeEdits {
                    parent: Some(cr),
                    ..Default::default()
                }
            ),
            Err(OpError::ReparentCycle { .. })
        ));
    }

    // -- status ------------------------------------------------------------

    #[test]
    fn status_simple_transitions() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let r = NodeRef::new("e", n.frontmatter.number);
        let ip = set_node_status(&s, &r, NodeStatusChange::InProgress).unwrap();
        assert_eq!(ip.node.frontmatter.status, NodeState::InProgress);
        let td = set_node_status(&s, &r, NodeStatusChange::ToDo).unwrap();
        assert_eq!(td.node.frontmatter.status, NodeState::ToDo);
    }

    #[test]
    fn status_blocked_sets_block_reason_and_leaves_blocked_by() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let r = NodeRef::new("e", n.frontmatter.number);
        let blocked = set_node_status(
            &s,
            &r,
            NodeStatusChange::Blocked {
                reason: Some("waiting on a key".into()),
            },
        )
        .unwrap();
        assert_eq!(blocked.node.frontmatter.status, NodeState::Blocked);
        assert_eq!(
            blocked.node.frontmatter.block_reason.as_deref(),
            Some("waiting on a key")
        );
        // blocked-by is a separate dependency list, untouched by the state.
        assert!(blocked.node.frontmatter.blocked_by.is_empty());
    }

    // -- blocked-by (dependency list, status-independent) ------------------

    #[test]
    fn blocked_by_add_remove_set_clear_and_existence_checks() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let a = create_node(&s, new_node("e", None)).unwrap();
        let b = create_node(&s, new_node("e", None)).unwrap();
        let c = create_node(&s, new_node("e", None)).unwrap();
        let ar = NodeRef::new("e", a.frontmatter.number);
        let br = NodeRef::new("e", b.frontmatter.number);
        let cr = NodeRef::new("e", c.frontmatter.number);

        // Add two existing blockers, deduped.
        let after = add_blocked_by(&s, &ar, &[br.clone(), cr.clone(), br.clone()]).unwrap();
        assert_eq!(after, vec![br.to_string(), cr.to_string()]);

        // A missing blocker is refused; a self-reference is refused.
        assert!(matches!(
            add_blocked_by(&s, &ar, &[NodeRef::new("e", 999)]),
            Err(OpError::NoSuchNode(_))
        ));
        assert!(matches!(
            add_blocked_by(&s, &ar, std::slice::from_ref(&ar)),
            Err(OpError::BlockedBySelf(_))
        ));

        // Remove one; the other survives.
        let after_rm = remove_blocked_by(&s, &ar, std::slice::from_ref(&br)).unwrap();
        assert_eq!(after_rm, vec![cr.to_string()]);

        // Set replaces wholesale; clear empties.
        let after_set = set_blocked_by(&s, &ar, std::slice::from_ref(&br)).unwrap();
        assert_eq!(after_set, vec![br.to_string()]);
        assert!(clear_blocked_by(&s, &ar).unwrap().is_empty());
        assert!(list_blocked_by(&s, &ar).unwrap().is_empty());
    }

    // -- claim (single-holder, node-only) ----------------------------------

    #[test]
    fn claim_take_reassign_and_release() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let r = NodeRef::new("e", n.frontmatter.number);

        // First take: no prior holder; by/at both set.
        let (node, prior) = take_claim(&s, &r, "alice@example.com").unwrap();
        assert_eq!(prior, None);
        let claim = node.frontmatter.claim.clone().unwrap();
        assert_eq!(claim.by, "alice@example.com");
        let first_at = claim.at;

        // Reassign: single-holder, so it overwrites and reports the prior.
        let (node, prior) = take_claim(&s, &r, "bob@example.com").unwrap();
        assert_eq!(prior.as_deref(), Some("alice@example.com"));
        let claim = node.frontmatter.claim.clone().unwrap();
        assert_eq!(claim.by, "bob@example.com");
        // The timestamp is refreshed on reassignment (never behind the first).
        assert!(claim.at >= first_at);

        // Release drops holder and timestamp together and reports the prior.
        let (node, prior) = release_claim(&s, &r).unwrap();
        assert_eq!(prior.as_deref(), Some("bob@example.com"));
        assert!(node.frontmatter.claim.is_none());

        // Releasing an already-unclaimed node is a no-op with no prior holder.
        let (node, prior) = release_claim(&s, &r).unwrap();
        assert_eq!(prior, None);
        assert!(node.frontmatter.claim.is_none());
    }

    #[test]
    fn claim_take_refuses_a_blank_identifier() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let r = NodeRef::new("e", n.frontmatter.number);
        assert!(matches!(
            take_claim(&s, &r, "   "),
            Err(OpError::EmptyClaimIdentifier)
        ));
        // A surrounding-whitespace identifier is trimmed before storage.
        let (node, _) = take_claim(&s, &r, "  carol  ").unwrap();
        assert_eq!(node.frontmatter.claim.unwrap().by, "carol");
    }

    #[test]
    fn claim_is_independent_of_status() {
        // Claiming never changes state, and a state change never touches the
        // claim — it is a status-independent annotation.
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let r = NodeRef::new("e", n.frontmatter.number);
        take_claim(&s, &r, "alice").unwrap();
        let done = set_node_status(&s, &r, NodeStatusChange::Done).unwrap();
        assert_eq!(done.node.frontmatter.status, NodeState::Done);
        assert_eq!(done.node.frontmatter.claim.unwrap().by, "alice");
    }

    #[test]
    fn blocked_by_survives_state_changes() {
        // The dependency list is orthogonal to status: resolving a node never
        // clears it.
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let a = create_node(&s, new_node("e", None)).unwrap();
        let b = create_node(&s, new_node("e", None)).unwrap();
        let ar = NodeRef::new("e", a.frontmatter.number);
        let br = NodeRef::new("e", b.frontmatter.number);
        add_blocked_by(&s, &ar, std::slice::from_ref(&br)).unwrap();
        set_node_status(
            &s,
            &ar,
            NodeStatusChange::Closed {
                reason: Some("obsolete".into()),
                cascade: false,
            },
        )
        .unwrap();
        assert_eq!(list_blocked_by(&s, &ar).unwrap(), vec![br.to_string()]);
    }

    #[test]
    fn done_refused_with_open_descendant_then_allowed() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let parent = create_node(&s, new_node("e", None)).unwrap();
        let pr = NodeRef::new("e", parent.frontmatter.number);
        let child = create_node(&s, new_node("e", Some(pr.clone()))).unwrap();
        let cr = NodeRef::new("e", child.frontmatter.number);
        // The child is to-do, so parent cannot be done.
        assert!(matches!(
            set_node_status(&s, &pr, NodeStatusChange::Done),
            Err(OpError::Transition(
                domain::TransitionError::DoneWithOpenDescendants { .. }
            ))
        ));
        // Resolve the child, then the parent can be done.
        set_node_status(&s, &cr, NodeStatusChange::Done).unwrap();
        let done = set_node_status(&s, &pr, NodeStatusChange::Done).unwrap();
        assert_eq!(done.node.frontmatter.status, NodeState::Done);
    }

    #[test]
    fn close_needs_reason() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let r = NodeRef::new("e", n.frontmatter.number);
        assert!(matches!(
            set_node_status(
                &s,
                &r,
                NodeStatusChange::Closed {
                    reason: None,
                    cascade: false
                }
            ),
            Err(OpError::Transition(
                domain::TransitionError::CloseNeedsReason
            ))
        ));
    }

    #[test]
    fn close_without_cascade_leaves_child_open_then_cascades() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let parent = create_node(&s, new_node("e", None)).unwrap();
        let pr = NodeRef::new("e", parent.frontmatter.number);
        let child = create_node(&s, new_node("e", Some(pr.clone()))).unwrap();
        let cr = NodeRef::new("e", child.frontmatter.number);

        // A default close resolves only the parent; the open child is left as
        // it was and is never reported as a cascade target.
        let parent_only = set_node_status(
            &s,
            &pr,
            NodeStatusChange::Closed {
                reason: Some("obsolete".into()),
                cascade: false,
            },
        )
        .unwrap();
        assert_eq!(parent_only.node.frontmatter.status, NodeState::Closed);
        assert!(parent_only.cascaded_closed.is_empty());
        let child_untouched = s.read_node("e", cr.number).unwrap();
        assert_eq!(child_untouched.frontmatter.status, NodeState::ToDo);

        // Cascade closes both.
        let closed = set_node_status(
            &s,
            &pr,
            NodeStatusChange::Closed {
                reason: Some("obsolete".into()),
                cascade: true,
            },
        )
        .unwrap();
        assert_eq!(closed.node.frontmatter.status, NodeState::Closed);
        assert_eq!(
            closed.node.frontmatter.close_reason.as_deref(),
            Some("obsolete")
        );
        // The cascade's wider effect is reported: the child is listed.
        assert_eq!(closed.cascaded_closed, vec![cr.number]);
        let child_after = s.read_node("e", cr.number).unwrap();
        assert_eq!(child_after.frontmatter.status, NodeState::Closed);
        assert_eq!(
            child_after.frontmatter.close_reason.as_deref(),
            Some("obsolete")
        );
    }

    #[test]
    fn reactivating_a_closed_node_clears_its_close_reason() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let r = NodeRef::new("e", n.frontmatter.number);

        let closed = set_node_status(
            &s,
            &r,
            NodeStatusChange::Closed {
                reason: Some("superseded".into()),
                cascade: false,
            },
        )
        .unwrap();
        assert_eq!(
            closed.node.frontmatter.close_reason.as_deref(),
            Some("superseded")
        );

        // Moving back to an active state must not keep the stale reason.
        let reopened = set_node_status(&s, &r, NodeStatusChange::InProgress).unwrap();
        assert_eq!(reopened.node.frontmatter.status, NodeState::InProgress);
        assert_eq!(reopened.node.frontmatter.close_reason, None);

        // `done` is terminal but not `closed`, so it carries no reason either.
        let done = set_node_status(&s, &r, NodeStatusChange::Done).unwrap();
        assert_eq!(done.node.frontmatter.close_reason, None);
    }

    #[test]
    fn blocked_requires_a_reason() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let r = NodeRef::new("e", n.frontmatter.number);
        assert!(matches!(
            set_node_status(&s, &r, NodeStatusChange::Blocked { reason: None }),
            Err(OpError::Transition(
                domain::TransitionError::BlockedNeedsReason
            ))
        ));
    }

    #[test]
    fn epic_close_and_reopen() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let closed = set_epic_closed(&s, "e", true, Some("cancelled".into())).unwrap();
        assert!(closed.frontmatter.closed);
        assert_eq!(
            closed.frontmatter.close_reason.as_deref(),
            Some("cancelled")
        );
        let reopened = set_epic_closed(&s, "e", false, None).unwrap();
        assert!(!reopened.frontmatter.closed);
        assert_eq!(reopened.frontmatter.close_reason, None);
    }

    // -- bracketing (the lock spans the read and the publish) --------------

    #[test]
    fn a_write_against_something_that_is_not_there_names_it_rather_than_a_path() {
        // A write answers "it is not there" the same way a read does. Without
        // the check ahead of the lock the caller would instead be told the
        // target changed since it was read, naming a file path it never gave.
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let ghost = NodeRef::new("e", 99);

        assert!(matches!(
            add_labels(&s, &Target::Node(ghost.clone()), &["mine".into()]),
            Err(OpError::NoSuchNode(ref r)) if *r == ghost
        ));
        assert!(matches!(
            add_labels(&s, &Target::Epic("ghost".into()), &["mine".into()]),
            Err(OpError::NoSuchEpic(_))
        ));
        // A reparent names the node being edited, not the parent it proposes:
        // the edited node's absence is answered before the new parent is judged.
        assert!(matches!(
            edit_node(
                &s,
                &ghost,
                NodeEdits {
                    parent: Some(NodeRef::new("other", 1)),
                    ..Default::default()
                }
            ),
            Err(OpError::NoSuchNode(ref r)) if *r == ghost
        ));
        assert!(matches!(
            edit_epic(
                &s,
                "ghost",
                EpicEdits {
                    name: Some("renamed".into()),
                    ..Default::default()
                }
            ),
            Err(OpError::NoSuchEpic(_))
        ));
    }

    #[test]
    fn a_label_add_merges_into_a_change_published_while_it_waited_for_the_lock() {
        let (_d, s) = patient_store();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let r = NodeRef::new("e", n.frontmatter.number);
        let target = Target::Node(r.clone());

        // Hold the node's lock: the add cannot get past acquire, and because the
        // read is inside the bracket it cannot read the node either.
        let held =
            lock::acquire(&s.node_path("e", r.number), &patient_config(), Force::Deny).unwrap();

        std::thread::scope(|scope| {
            let adder = scope.spawn(|| add_labels(&s, &target, &["mine".into()]).unwrap());
            std::thread::sleep(HOLD);
            // Publish a competing label through the held lock, releasing it.
            let mut theirs = s.read_node("e", r.number).unwrap();
            theirs.frontmatter.labels.push("theirs".into());
            held.commit(theirs.to_text().unwrap().as_bytes()).unwrap();
            adder.join().unwrap();
        });

        // The label published while the add waited is in the set the add merged
        // into: the add read the node only once it held the lock, so it could
        // never have merged into a copy that predates that publish.
        assert_eq!(
            s.read_node("e", r.number).unwrap().frontmatter.labels,
            vec!["theirs", "mine"]
        );
    }

    #[test]
    fn a_cascade_leaves_a_descendant_resolved_while_it_waited_exactly_as_it_is() {
        // The idempotence test is made against the descendant read under its own
        // lock, so a resolution that landed while the cascade waited is neither
        // re-resolved nor republished.
        let (_d, s) = patient_store();
        create_epic(&s, new_epic("e")).unwrap();
        let parent = create_node(&s, new_node("e", None)).unwrap();
        let pr = NodeRef::new("e", parent.frontmatter.number);
        let child = create_node(&s, new_node("e", Some(pr.clone()))).unwrap();
        let cr = NodeRef::new("e", child.frontmatter.number);
        let child_path = s.node_path("e", cr.number);

        let held = lock::acquire(&child_path, &patient_config(), Force::Deny).unwrap();

        let resolved_bytes = std::thread::scope(|scope| {
            // The cascade plans against the still-open child, then waits on its
            // lock.
            let closer = scope.spawn(|| {
                set_node_status(
                    &s,
                    &pr,
                    NodeStatusChange::Closed {
                        reason: Some("obsolete".into()),
                        cascade: true,
                    },
                )
                .unwrap()
            });
            std::thread::sleep(HOLD);
            // Another actor resolves the child as done, through the held lock.
            let mut resolved = s.read_node("e", cr.number).unwrap();
            resolved.frontmatter.status = NodeState::Done;
            let bytes = resolved.to_text().unwrap().into_bytes();
            held.commit(&bytes).unwrap();
            closer.join().unwrap();
            bytes
        });

        let after = s.read_node("e", cr.number).unwrap();
        assert_eq!(
            after.frontmatter.status,
            NodeState::Done,
            "a descendant resolved under the cascade's nose keeps its own resolution"
        );
        assert_eq!(after.frontmatter.close_reason, None);
        assert_eq!(
            std::fs::read(&child_path).unwrap(),
            resolved_bytes,
            "a descendant needing no change must not be rewritten"
        );
    }

    // -- labels ------------------------------------------------------------

    #[test]
    fn labels_add_remove_list_on_a_node() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let target = Target::Node(NodeRef::new("e", n.frontmatter.number));
        let after_add = add_labels(&s, &target, &["x".into(), "y".into(), "x".into()]).unwrap();
        assert_eq!(after_add, vec!["x", "y"]);
        let after_rm = remove_labels(&s, &target, &["x".into(), "absent".into()]).unwrap();
        assert_eq!(after_rm, vec!["y"]);
        assert_eq!(list_labels(&s, &target).unwrap(), vec!["y"]);
    }

    #[test]
    fn labels_on_an_epic() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let target = Target::Epic("e".into());
        assert_eq!(add_labels(&s, &target, &["a".into()]).unwrap(), vec!["a"]);
        assert_eq!(list_labels(&s, &target).unwrap(), vec!["a"]);
    }

    // -- comments ----------------------------------------------------------

    #[test]
    fn comment_add_edit_delete_author_only_and_tombstone() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let target = Target::Node(NodeRef::new("e", n.frontmatter.number));

        let bot = Actor::Agent("bot".into());
        let other = Actor::Agent("other".into());

        let c = add_comment(&s, &target, bot.clone(), "hello".into()).unwrap();
        assert_eq!(c.id, 1);
        assert_eq!(c.author, bot);

        // A non-author cannot edit.
        assert!(matches!(
            edit_comment(&s, &target, c.id, other.clone(), "nope".into(), None),
            Err(OpError::CommentAuth(domain::CommentAuthError::NotAuthor))
        ));

        // The author can edit.
        let edited = edit_comment(&s, &target, c.id, bot.clone(), "hi again".into(), None).unwrap();
        assert_eq!(edited.text, "hi again");

        // The author soft-deletes it.
        let deleted = delete_comment(&s, &target, c.id, bot.clone()).unwrap();
        assert!(deleted.deleted);

        // Re-deleting an already-deleted comment is refused.
        assert!(matches!(
            delete_comment(&s, &target, c.id, bot.clone()),
            Err(OpError::CommentAuth(
                domain::CommentAuthError::AlreadyDeleted
            ))
        ));

        // Hidden by default, tombstone under include_deleted.
        assert!(list_comments(&s, &target, false).unwrap().is_empty());
        let shown = list_comments(&s, &target, true).unwrap();
        assert_eq!(shown.len(), 1);
        match &shown[0] {
            CommentView::Tombstone { id, author, .. } => {
                assert_eq!(*id, 1);
                assert_eq!(author, &bot);
            }
            other => panic!("expected a tombstone, got {other:?}"),
        }
    }

    #[test]
    fn comment_ids_are_never_reused_across_soft_delete() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let target = Target::Epic("e".into());
        let a = add_comment(&s, &target, Actor::Human, "one".into()).unwrap();
        delete_comment(&s, &target, a.id, Actor::Human).unwrap();
        let b = add_comment(&s, &target, Actor::Human, "two".into()).unwrap();
        assert_eq!(a.id, 1);
        assert_eq!(b.id, 2);
    }

    #[test]
    fn edit_missing_comment_is_an_error() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let target = Target::Epic("e".into());
        assert!(matches!(
            edit_comment(&s, &target, 42, Actor::Human, "x".into(), None),
            Err(OpError::NoSuchComment(42))
        ));
    }

    // -- assets ------------------------------------------------------------

    #[test]
    fn asset_add_delete_list() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let n = create_node(&s, new_node("e", None)).unwrap();
        let target = Target::Node(NodeRef::new("e", n.frontmatter.number));

        let entry = add_asset(
            &s,
            &target,
            "proof.bin",
            Some("evidence".into()),
            &[1, 2, 3],
        )
        .unwrap();
        assert_eq!(entry.name, "proof.bin");
        // Bytes landed in the companion dir.
        assert!(s
            .node_asset_dir("e", n.frontmatter.number)
            .join("proof.bin")
            .is_file());
        // Indexed.
        assert_eq!(list_assets(&s, &target).unwrap().len(), 1);

        // Delete removes both index and bytes.
        let removed = delete_asset(&s, &target, "proof.bin").unwrap();
        assert_eq!(removed.name, "proof.bin");
        assert!(list_assets(&s, &target).unwrap().is_empty());
        assert!(!s
            .node_asset_dir("e", n.frontmatter.number)
            .join("proof.bin")
            .exists());
    }

    #[test]
    fn asset_add_refuses_duplicate_name() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let target = Target::Epic("e".into());
        add_asset(&s, &target, "a.txt", None, b"first").unwrap();
        // A repeat name is refused; `update` is the replace path.
        assert!(matches!(
            add_asset(&s, &target, "a.txt", Some("second".into()), b"second"),
            Err(OpError::AssetExists(_))
        ));
        // The original entry and bytes are untouched by the refused add.
        let assets = list_assets(&s, &target).unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].description, None);
        assert_eq!(
            std::fs::read(s.epic_asset_dir("e").join("a.txt")).unwrap(),
            b"first"
        );
    }

    #[test]
    fn delete_missing_asset_is_an_error() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let target = Target::Epic("e".into());
        assert!(matches!(
            delete_asset(&s, &target, "ghost"),
            Err(OpError::NoSuchAsset(_))
        ));
    }

    #[test]
    fn read_asset_returns_indexed_bytes() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let target = Target::Epic("e".into());
        add_asset(&s, &target, "a.bin", None, &[0u8, 1, 2, 3]).unwrap();
        assert_eq!(
            read_asset(&s, &target, "a.bin").unwrap(),
            vec![0u8, 1, 2, 3]
        );
    }

    #[test]
    fn read_missing_asset_is_an_error() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let target = Target::Epic("e".into());
        assert!(matches!(
            read_asset(&s, &target, "ghost"),
            Err(OpError::NoSuchAsset(_))
        ));
    }

    #[test]
    fn update_asset_replaces_bytes_and_description() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let target = Target::Epic("e".into());
        add_asset(&s, &target, "a.txt", Some("old".into()), b"first").unwrap();

        // Replace both data and description.
        update_asset(
            &s,
            &target,
            "a.txt",
            Some(Some("new".into())),
            Some(b"second"),
        )
        .unwrap();
        let assets = list_assets(&s, &target).unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].description.as_deref(), Some("new"));
        assert_eq!(read_asset(&s, &target, "a.txt").unwrap(), b"second");
    }

    #[test]
    fn update_asset_can_change_description_only() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let target = Target::Epic("e".into());
        add_asset(&s, &target, "a.txt", Some("old".into()), b"keep").unwrap();

        // Leaving bytes None must not disturb the stored payload.
        update_asset(&s, &target, "a.txt", Some(Some("new".into())), None).unwrap();
        assert_eq!(read_asset(&s, &target, "a.txt").unwrap(), b"keep");
        let assets = list_assets(&s, &target).unwrap();
        assert_eq!(assets[0].description.as_deref(), Some("new"));

        // Clearing the description (Some(None)) leaves the bytes alone too.
        update_asset(&s, &target, "a.txt", Some(None), None).unwrap();
        assert_eq!(list_assets(&s, &target).unwrap()[0].description, None);
        assert_eq!(read_asset(&s, &target, "a.txt").unwrap(), b"keep");
    }

    #[test]
    fn update_missing_asset_is_an_error() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        let target = Target::Epic("e".into());
        assert!(matches!(
            update_asset(&s, &target, "ghost", Some(Some("x".into())), None),
            Err(OpError::NoSuchAsset(_))
        ));
    }

    // -- tree resolution ---------------------------------------------------

    #[test]
    fn descendants_are_resolved_recursively() {
        let (_d, s) = seeded();
        create_epic(&s, new_epic("e")).unwrap();
        // 1 <- 2 <- 3, and 1 <- 4
        let n1 = create_node(&s, new_node("e", None)).unwrap();
        let r1 = NodeRef::new("e", n1.frontmatter.number);
        let n2 = create_node(&s, new_node("e", Some(r1.clone()))).unwrap();
        let r2 = NodeRef::new("e", n2.frontmatter.number);
        let _n3 = create_node(&s, new_node("e", Some(r2))).unwrap();
        let _n4 = create_node(&s, new_node("e", Some(r1.clone()))).unwrap();

        let mut nums: Vec<u64> = descendants_of(&s, &r1)
            .unwrap()
            .iter()
            .map(|d| d.number)
            .collect();
        nums.sort_unstable();
        assert_eq!(nums, vec![2, 3, 4]);

        // A leaf has no descendants.
        assert!(descendants_of(&s, &NodeRef::new("e", 3))
            .unwrap()
            .is_empty());
    }
}
