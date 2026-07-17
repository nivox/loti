//! The domain rules and state machine the commands enforce.
//!
//! This module is the single home of the invariants that keep a store
//! consistent: which state transitions are allowed, what closing a node with
//! open descendants entails, how an epic's state is computed, and who may edit
//! or delete a comment. The rules live here as pure functions and small value
//! types so they can be exercised in isolation and reused by every surface
//! (CLI today, others later) without reaching into the filesystem.
//!
//! Everything that needs to know about a node's descendants takes that
//! information as an injected slice, never by walking the store itself — the
//! caller resolves the tree and hands it in. That keeps the rules I/O-free and
//! makes each one directly unit-testable.

use std::fmt;

use crate::{Actor, NodeState};

/// A reference to one node: its epic and its flat per-epic number. This
/// addresses exactly one node regardless of its depth, because the parent/child
/// relationship is metadata (a parent number on the node), never encoded in the
/// number itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeRef {
    /// The owning epic's identifier.
    pub epic_id: String,
    /// The node's number within that epic's flat pool.
    pub number: u64,
}

/// Why a `<epic-id>/<n>` reference could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefParseError {
    /// The reference did not contain the `/` separating the epic from the
    /// number.
    #[error("'{0}' is not a node reference; expected the form <epic-id>/<number>")]
    MissingSeparator(String),
    /// The epic-id portion before the `/` was empty.
    #[error("node reference '{0}' has an empty epic id before the '/'")]
    EmptyEpicId(String),
    /// The number portion after the `/` was empty.
    #[error("node reference '{0}' has no number after the '/'")]
    EmptyNumber(String),
    /// The number portion was present but not a non-negative integer.
    #[error("node reference '{reference}' has an invalid number '{number}'")]
    InvalidNumber {
        /// The whole reference as given.
        reference: String,
        /// The offending number portion.
        number: String,
    },
    /// The reference contained more than one `/`, so it is ambiguous — a bare
    /// number never encodes hierarchy, so a nested-looking reference is
    /// rejected rather than guessed at.
    #[error("node reference '{0}' has more than one '/'; a reference is <epic-id>/<number>")]
    TooManyParts(String),
}

impl NodeRef {
    /// Build a reference directly from its parts.
    pub fn new(epic_id: impl Into<String>, number: u64) -> Self {
        Self {
            epic_id: epic_id.into(),
            number,
        }
    }

    /// Parse a `<epic-id>/<n>` reference. The epic id may itself contain no
    /// `/`; exactly one separator is required, so the parse is unambiguous.
    pub fn parse(reference: &str) -> Result<Self, RefParseError> {
        let mut parts = reference.split('/');
        let epic_id = parts
            .next()
            .ok_or_else(|| RefParseError::MissingSeparator(reference.to_string()))?;
        let number = match parts.next() {
            Some(n) => n,
            None => return Err(RefParseError::MissingSeparator(reference.to_string())),
        };
        // A third segment means the caller wrote something like `e/1/2`; a
        // reference never nests, so this is rejected rather than truncated.
        if parts.next().is_some() {
            return Err(RefParseError::TooManyParts(reference.to_string()));
        }
        if epic_id.is_empty() {
            return Err(RefParseError::EmptyEpicId(reference.to_string()));
        }
        if number.is_empty() {
            return Err(RefParseError::EmptyNumber(reference.to_string()));
        }
        let number = number
            .parse::<u64>()
            .map_err(|_| RefParseError::InvalidNumber {
                reference: reference.to_string(),
                number: number.to_string(),
            })?;
        Ok(Self {
            epic_id: epic_id.to_string(),
            number,
        })
    }
}

impl fmt::Display for NodeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.epic_id, self.number)
    }
}

impl std::str::FromStr for NodeRef {
    type Err = RefParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A node as the state machine sees it: just its number and current state. The
/// full node carries much more, but the transition rules only ever need these
/// two facts about a node and its descendants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeStatus {
    /// The node's number within its epic.
    pub number: u64,
    /// Its current state.
    pub state: NodeState,
}

impl NodeStatus {
    /// Construct a status pair.
    pub fn new(number: u64, state: NodeState) -> Self {
        Self { number, state }
    }
}

/// Why a requested state transition is refused. Each variant states the rule it
/// enforces so the message needs no external reference to be understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    /// `done` requires every descendant to be resolved first. A `closed`
    /// descendant counts as resolved; only `to-do`/`in-progress`/`blocked`
    /// descendants block it.
    #[error(
        "cannot mark this node done while {count} descendant(s) are still \
         open (first: {first}); resolve or close them first"
    )]
    DoneWithOpenDescendants {
        /// How many descendants are still non-terminal.
        count: usize,
        /// The lowest-numbered offending descendant, for a concrete pointer.
        first: u64,
    },
    /// `closed` must carry a reason explaining why it was resolved without
    /// completing.
    #[error("closing a node requires a reason")]
    CloseNeedsReason,
    /// Closing a node with non-terminal descendants is refused unless the
    /// caller explicitly asks to cascade the close to them.
    #[error(
        "cannot close this node while {count} descendant(s) are still open \
         (first: {first}); re-run with cascade to close them too"
    )]
    CloseWithOpenDescendants {
        /// How many descendants are still non-terminal.
        count: usize,
        /// The lowest-numbered offending descendant.
        first: u64,
    },
    /// Entering `blocked` is only ever an explicit request; it is never set (or
    /// cleared) as a side effect of another operation.
    #[error("the blocked state is only ever set explicitly, never automatically")]
    BlockedMustBeExplicit,
    /// `blocked` must carry a non-empty structured blocker: at least one node
    /// reference or a free-form reason, so a blocked node always says why.
    #[error("blocking a node requires a blocker: pass --blocked-by and/or --reason")]
    BlockedNeedsBlocker,
}

/// Whether a set of descendant statuses are all resolved (terminal). An empty
/// set is vacuously all-terminal, which is what lets a leaf become `done`.
fn all_terminal(descendants: &[NodeStatus]) -> bool {
    descendants.iter().all(|d| d.state.is_terminal())
}

/// The non-terminal descendants, in ascending node-number order. This is both
/// the blocking set for a `done`/`close` check and, for a close, the set a
/// cascade would resolve — computed once, ordered so any multi-node effect
/// visits nodes in the same order the concurrency layer takes locks.
fn open_descendants(descendants: &[NodeStatus]) -> Vec<NodeStatus> {
    let mut open: Vec<NodeStatus> = descendants
        .iter()
        .copied()
        .filter(|d| !d.state.is_terminal())
        .collect();
    open.sort_by_key(|d| d.number);
    open
}

/// Validate a transition of a node to `done`.
///
/// A node may become `done` only when every descendant is terminal; a `closed`
/// descendant counts as resolved and does not block. `descendants` is the full
/// recursive descendant set (the caller resolves the tree and injects it); an
/// empty set means a leaf, which is always allowed.
pub fn validate_done(descendants: &[NodeStatus]) -> Result<(), TransitionError> {
    if all_terminal(descendants) {
        return Ok(());
    }
    let open = open_descendants(descendants);
    Err(TransitionError::DoneWithOpenDescendants {
        count: open.len(),
        first: open[0].number,
    })
}

/// Whether cascade was requested when closing a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cascade {
    /// Close only this node; refuse if it has non-terminal descendants.
    No,
    /// Close this node and every non-terminal descendant too.
    Yes,
}

/// The concrete effect of a validated close: the descendant nodes that must
/// also be closed, in the order they should be visited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosePlan {
    /// Non-terminal descendants to close as well, in ascending node-number
    /// order so a multi-file close takes locks in a deadlock-free order. Empty
    /// for a node whose descendants are all already terminal.
    pub cascade_targets: Vec<u64>,
}

/// Validate closing a node and compute what closing it entails.
///
/// Closing always requires a reason. A node with non-terminal descendants is
/// refused unless cascade is requested; when it is, the plan lists those
/// descendants (ascending order) so the caller can close them too. Already-
/// terminal descendants are never re-closed and never appear in the plan.
pub fn plan_close(
    descendants: &[NodeStatus],
    reason: Option<&str>,
    cascade: Cascade,
) -> Result<ClosePlan, TransitionError> {
    // A close is a resolution-without-completion and must record why.
    match reason {
        Some(r) if !r.trim().is_empty() => {}
        _ => return Err(TransitionError::CloseNeedsReason),
    }

    let open = open_descendants(descendants);
    if open.is_empty() {
        return Ok(ClosePlan {
            cascade_targets: Vec::new(),
        });
    }

    match cascade {
        Cascade::No => Err(TransitionError::CloseWithOpenDescendants {
            count: open.len(),
            first: open[0].number,
        }),
        Cascade::Yes => Ok(ClosePlan {
            cascade_targets: open.into_iter().map(|d| d.number).collect(),
        }),
    }
}

/// Validate a request to enter the `blocked` state.
///
/// Two rules hold. First, `loti` never sets or clears `blocked` as a side
/// effect: `explicitly_requested` must be true — false means some path tried to
/// reach `blocked` implicitly, which is refused. Second, the blocker must not be
/// empty: `has_blocker` records whether at least one node reference or a
/// free-form reason was supplied, so a blocked node always states why.
pub fn validate_blocked(
    explicitly_requested: bool,
    has_blocker: bool,
) -> Result<(), TransitionError> {
    if !explicitly_requested {
        return Err(TransitionError::BlockedMustBeExplicit);
    }
    if !has_blocker {
        return Err(TransitionError::BlockedNeedsBlocker);
    }
    Ok(())
}

/// The three states an epic can be in. `Closed` is stored; the other two are
/// computed from the epic's nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpicStatus {
    /// Explicitly closed via the stored flag. Takes precedence over the
    /// computed states and is reversible (reopening clears the flag).
    Closed,
    /// Computed: the epic has at least one node and every node is terminal.
    Completed,
    /// Computed: any other case, including an epic with no nodes at all.
    Open,
}

impl EpicStatus {
    /// The lower-case name used in output.
    pub fn wire_name(self) -> &'static str {
        match self {
            EpicStatus::Closed => "closed",
            EpicStatus::Completed => "completed",
            EpicStatus::Open => "open",
        }
    }
}

impl fmt::Display for EpicStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wire_name())
    }
}

/// Compute an epic's state from its stored closed flag and its nodes.
///
/// The stored closed flag wins outright. Otherwise the state is computed:
/// `completed` when there is at least one node and all nodes are terminal,
/// `open` in every other case — including an epic with no nodes, which is open,
/// not completed.
pub fn epic_status(closed_flag: bool, nodes: &[NodeStatus]) -> EpicStatus {
    if closed_flag {
        return EpicStatus::Closed;
    }
    if !nodes.is_empty() && nodes.iter().all(|n| n.state.is_terminal()) {
        EpicStatus::Completed
    } else {
        EpicStatus::Open
    }
}

/// The kinds of operation, for the actor-requirement rule. Attribution is
/// required only for comment operations; every other operation is
/// actor-agnostic because comments are the sole attribution channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    /// Adding, editing, or deleting a comment — the only attributed operations.
    Comment,
    /// Anything else (create/edit/status/label/asset/read): actor-agnostic.
    Other,
}

/// An operation requires an actor exactly when it is a comment operation.
/// Everything else is actor-agnostic, so attributing it is neither required nor
/// meaningful — a status change or asset is attributed by adding a comment.
pub fn requires_actor(kind: OperationKind) -> bool {
    matches!(kind, OperationKind::Comment)
}

/// Why a comment edit or delete is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommentAuthError {
    /// Only a comment's own author may edit or delete it. Attribution is
    /// cooperative, not security, but the rule keeps a clean audit trail.
    #[error("only a comment's own author may edit or delete it")]
    NotAuthor,
    /// The comment was already soft-deleted, so there is nothing to edit or
    /// re-delete.
    #[error("this comment has already been deleted")]
    AlreadyDeleted,
}

/// Whether `actor` may edit or delete a comment authored by `author`, given
/// whether that comment is already deleted.
///
/// A comment is editable and soft-deletable only by its own author; a comment
/// that is already deleted cannot be edited or deleted again.
pub fn authorize_comment_mutation(
    author: &Actor,
    deleted: bool,
    actor: &Actor,
) -> Result<(), CommentAuthError> {
    if deleted {
        return Err(CommentAuthError::AlreadyDeleted);
    }
    if author != actor {
        return Err(CommentAuthError::NotAuthor);
    }
    Ok(())
}

/// Whether a comment is visible in a listing, given whether deleted comments
/// were explicitly requested.
///
/// A soft-deleted comment is hidden by default and shown only when deleted
/// entries are explicitly asked for; a live comment is always visible. The
/// caller decides how to render a shown-but-deleted comment (its text is
/// withheld in favour of a tombstone).
pub fn comment_is_visible(deleted: bool, include_deleted: bool) -> bool {
    !deleted || include_deleted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(number: u64) -> NodeStatus {
        NodeStatus::new(number, NodeState::InProgress)
    }
    fn done(number: u64) -> NodeStatus {
        NodeStatus::new(number, NodeState::Done)
    }
    fn closed(number: u64) -> NodeStatus {
        NodeStatus::new(number, NodeState::Closed)
    }

    // -- reference parse/format --------------------------------------------

    #[test]
    fn ref_round_trips() {
        let r = NodeRef::parse("my-epic/7").unwrap();
        assert_eq!(r.epic_id, "my-epic");
        assert_eq!(r.number, 7);
        assert_eq!(r.to_string(), "my-epic/7");
        // Format then parse is the identity.
        assert_eq!(NodeRef::parse(&r.to_string()).unwrap(), r);
    }

    #[test]
    fn ref_accepts_hyphenated_and_numeric_epic_ids() {
        assert_eq!(NodeRef::parse("a-b-c/12").unwrap().epic_id, "a-b-c");
        assert_eq!(NodeRef::parse("2024/1").unwrap().epic_id, "2024");
    }

    #[test]
    fn ref_rejects_malformed_forms() {
        assert!(matches!(
            NodeRef::parse("noseparator"),
            Err(RefParseError::MissingSeparator(_))
        ));
        assert!(matches!(
            NodeRef::parse("/7"),
            Err(RefParseError::EmptyEpicId(_))
        ));
        assert!(matches!(
            NodeRef::parse("epic/"),
            Err(RefParseError::EmptyNumber(_))
        ));
        assert!(matches!(
            NodeRef::parse("epic/abc"),
            Err(RefParseError::InvalidNumber { .. })
        ));
        assert!(matches!(
            NodeRef::parse("epic/-1"),
            Err(RefParseError::InvalidNumber { .. })
        ));
        assert!(matches!(
            NodeRef::parse("epic/1/2"),
            Err(RefParseError::TooManyParts(_))
        ));
    }

    #[test]
    fn ref_parses_via_fromstr() {
        let r: NodeRef = "e/3".parse().unwrap();
        assert_eq!(r, NodeRef::new("e", 3));
    }

    // -- done transition ---------------------------------------------------

    #[test]
    fn leaf_can_become_done() {
        assert!(validate_done(&[]).is_ok());
    }

    #[test]
    fn done_allowed_when_all_descendants_terminal() {
        // A closed descendant counts as resolved and does not block done.
        assert!(validate_done(&[done(2), closed(3)]).is_ok());
    }

    #[test]
    fn done_refused_with_a_non_terminal_descendant() {
        let err = validate_done(&[done(2), open(5), open(3)]).unwrap_err();
        match err {
            TransitionError::DoneWithOpenDescendants { count, first } => {
                assert_eq!(count, 2);
                // The first offender is reported in ascending number order.
                assert_eq!(first, 3);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    // -- close transition & cascade ---------------------------------------

    #[test]
    fn close_requires_a_reason() {
        assert_eq!(
            plan_close(&[], None, Cascade::No),
            Err(TransitionError::CloseNeedsReason)
        );
        // A blank/whitespace reason does not count.
        assert_eq!(
            plan_close(&[], Some("   "), Cascade::No),
            Err(TransitionError::CloseNeedsReason)
        );
    }

    #[test]
    fn close_leaf_or_all_terminal_needs_no_cascade() {
        assert_eq!(
            plan_close(&[], Some("obsolete"), Cascade::No).unwrap(),
            ClosePlan {
                cascade_targets: Vec::new()
            }
        );
        assert_eq!(
            plan_close(&[done(2), closed(3)], Some("obsolete"), Cascade::No)
                .unwrap()
                .cascade_targets,
            Vec::<u64>::new()
        );
    }

    #[test]
    fn close_refused_without_cascade_when_descendants_open() {
        let err =
            plan_close(&[open(4), open(2), done(9)], Some("won't do"), Cascade::No).unwrap_err();
        match err {
            TransitionError::CloseWithOpenDescendants { count, first } => {
                assert_eq!(count, 2);
                assert_eq!(first, 2);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn cascade_close_lists_open_descendants_in_ascending_order() {
        let plan = plan_close(
            &[open(7), open(2), closed(5), open(4)],
            Some("superseded"),
            Cascade::Yes,
        )
        .unwrap();
        // Only the non-terminal descendants, ascending; the closed one is left
        // out because it is already resolved.
        assert_eq!(plan.cascade_targets, vec![2, 4, 7]);
    }

    // -- blocked is explicit-only -----------------------------------------

    #[test]
    fn blocked_must_be_explicit() {
        assert!(validate_blocked(true, true).is_ok());
        assert_eq!(
            validate_blocked(false, true),
            Err(TransitionError::BlockedMustBeExplicit)
        );
    }

    #[test]
    fn blocked_requires_a_non_empty_blocker() {
        // An explicit request with neither a ref nor a reason is refused.
        assert_eq!(
            validate_blocked(true, false),
            Err(TransitionError::BlockedNeedsBlocker)
        );
    }

    // -- epic state --------------------------------------------------------

    #[test]
    fn epic_closed_flag_takes_precedence() {
        // Even with all nodes terminal, the stored closed flag wins.
        assert_eq!(epic_status(true, &[done(1)]), EpicStatus::Closed);
        // And even with open nodes.
        assert_eq!(epic_status(true, &[open(1)]), EpicStatus::Closed);
    }

    #[test]
    fn epic_completed_requires_at_least_one_node_all_terminal() {
        assert_eq!(
            epic_status(false, &[done(1), closed(2)]),
            EpicStatus::Completed
        );
    }

    #[test]
    fn epic_with_no_nodes_is_open_not_completed() {
        assert_eq!(epic_status(false, &[]), EpicStatus::Open);
    }

    #[test]
    fn epic_with_any_open_node_is_open() {
        assert_eq!(epic_status(false, &[done(1), open(2)]), EpicStatus::Open);
    }

    #[test]
    fn epic_status_names() {
        assert_eq!(EpicStatus::Closed.to_string(), "closed");
        assert_eq!(EpicStatus::Completed.to_string(), "completed");
        assert_eq!(EpicStatus::Open.to_string(), "open");
    }

    // -- attribution -------------------------------------------------------

    #[test]
    fn only_comment_operations_require_an_actor() {
        assert!(requires_actor(OperationKind::Comment));
        assert!(!requires_actor(OperationKind::Other));
    }

    // -- comment author-only edit/delete ----------------------------------

    #[test]
    fn author_may_mutate_their_own_live_comment() {
        let author = Actor::Agent("bot".into());
        assert!(authorize_comment_mutation(&author, false, &author).is_ok());
        assert!(authorize_comment_mutation(&Actor::Human, false, &Actor::Human).is_ok());
    }

    #[test]
    fn non_author_is_refused() {
        let author = Actor::Agent("bot".into());
        let other = Actor::Agent("other".into());
        assert_eq!(
            authorize_comment_mutation(&author, false, &other),
            Err(CommentAuthError::NotAuthor)
        );
        assert_eq!(
            authorize_comment_mutation(&author, false, &Actor::Human),
            Err(CommentAuthError::NotAuthor)
        );
    }

    #[test]
    fn an_already_deleted_comment_cannot_be_mutated_even_by_its_author() {
        let author = Actor::Human;
        assert_eq!(
            authorize_comment_mutation(&author, true, &author),
            Err(CommentAuthError::AlreadyDeleted)
        );
    }

    // -- comment visibility ------------------------------------------------

    #[test]
    fn soft_deleted_comments_are_hidden_unless_requested() {
        assert!(comment_is_visible(false, false));
        assert!(comment_is_visible(false, true));
        assert!(!comment_is_visible(true, false));
        assert!(comment_is_visible(true, true));
    }
}
