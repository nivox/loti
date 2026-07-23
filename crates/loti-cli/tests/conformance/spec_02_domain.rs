//! Conformance: the domain model & state machine.
//!
//! The normative rules exercised here:
//!   * a node becomes `done` only when every descendant is terminal, and a
//!     `closed` descendant counts as resolved;
//!   * closing a node resolves only that node by default, leaving any
//!     non-terminal descendants untouched; a cascade closes the descendants too;
//!   * `blocked` is only ever set explicitly and carries its structured
//!     blocked-by; it is never set or cleared automatically;
//!   * an epic's state is the stored closed flag when set, else computed:
//!     `completed` when it has at least one node and all are terminal, otherwise
//!     `open` (including an epic with no nodes at all).

use super::harness::Store;

#[test]
fn done_is_refused_while_a_descendant_is_open() {
    let s = Store::new();
    s.epic("e");
    let parent = s.ticket("e", "parent");
    let _child = s.subticket("e", &parent, "child");
    // The child is to-do, so the parent cannot be marked done.
    let err = s.fail(&["ticket", "status", &parent, "--done"]);
    assert!(
        err.contains("descendant") && err.to_lowercase().contains("open"),
        "done should be refused with an open descendant, got: {err}"
    );
}

#[test]
fn done_is_allowed_once_all_descendants_are_terminal() {
    let s = Store::new();
    s.epic("e");
    let parent = s.ticket("e", "parent");
    let child = s.subticket("e", &parent, "child");
    // Resolve the child, then the parent may become done.
    s.ok(&["ticket", "status", &child, "--done"]);
    s.ok(&["ticket", "status", &parent, "--done"]);
    let json = s.ok(&["ticket", "show", &parent, "--json"]);
    assert!(json.contains("\"status\": \"done\""), "got: {json}");
}

#[test]
fn a_closed_descendant_counts_as_resolved_for_done() {
    let s = Store::new();
    s.epic("e");
    let parent = s.ticket("e", "parent");
    let child = s.subticket("e", &parent, "child");
    // Close (not complete) the child; it is terminal, so it does not block done.
    s.ok(&[
        "ticket", "status", &child, "--closed", "--reason", "obsolete",
    ]);
    s.ok(&["ticket", "status", &parent, "--done"]);
    let json = s.ok(&["ticket", "show", &parent, "--json"]);
    assert!(json.contains("\"status\": \"done\""), "got: {json}");
}

#[test]
fn closing_requires_a_reason() {
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "t");
    let err = s.fail(&["ticket", "status", &t, "--closed"]);
    assert!(
        err.to_lowercase().contains("reason"),
        "closing without a reason should be refused, got: {err}"
    );
}

#[test]
fn closing_without_cascade_leaves_open_descendants_untouched() {
    let s = Store::new();
    s.epic("e");
    let parent = s.ticket("e", "parent");
    let child = s.subticket("e", &parent, "child");
    // A default close resolves only the parent; the open child keeps its state,
    // so the parent can be reopened without having rewritten the subtree.
    s.ok(&[
        "ticket", "status", &parent, "--closed", "--reason", "obsolete",
    ]);
    let pj = s.ok(&["ticket", "show", &parent, "--json"]);
    let cj = s.ok(&["ticket", "show", &child, "--json"]);
    assert!(pj.contains("\"status\": \"closed\""), "parent: {pj}");
    assert!(
        cj.contains("\"status\": \"to-do\""),
        "child untouched: {cj}"
    );
    // Reopening the parent restores an active node with the subtree intact.
    s.ok(&["ticket", "status", &parent, "--in-progress"]);
    let pj2 = s.ok(&["ticket", "show", &parent, "--json"]);
    let cj2 = s.ok(&["ticket", "show", &child, "--json"]);
    assert!(pj2.contains("\"status\": \"in-progress\""), "parent: {pj2}");
    assert!(cj2.contains("\"status\": \"to-do\""), "child: {cj2}");
}

#[test]
fn cascade_close_resolves_the_descendants_too() {
    let s = Store::new();
    s.epic("e");
    let parent = s.ticket("e", "parent");
    let child = s.subticket("e", &parent, "child");
    let confirm = s.ok(&[
        "ticket",
        "status",
        &parent,
        "--closed",
        "--reason",
        "superseded",
        "--cascade",
    ]);
    // The confirmation names the descendants the cascade also closed, so the
    // wider effect is never silent.
    assert!(
        confirm.contains("cascade also closed") && confirm.contains(&child),
        "cascade must report the closed descendants: {confirm}"
    );
    // Both the node and its descendant are now closed with the reason.
    let pj = s.ok(&["ticket", "show", &parent, "--json"]);
    let cj = s.ok(&["ticket", "show", &child, "--json"]);
    assert!(pj.contains("\"status\": \"closed\""), "parent: {pj}");
    assert!(cj.contains("\"status\": \"closed\""), "child: {cj}");
    assert!(cj.contains("superseded"), "child keeps the reason: {cj}");
}

#[test]
fn blocked_carries_its_structured_blocked_by() {
    let s = Store::new();
    s.epic("e");
    let a = s.ticket("e", "a");
    let b = s.ticket("e", "b");
    s.ok(&[
        "ticket",
        "status",
        &a,
        "--blocked",
        "--blocked-by",
        &b,
        "--reason",
        "waiting on b",
    ]);
    let json = s.ok(&["ticket", "show", &a, "--json"]);
    assert!(json.contains("\"status\": \"blocked\""), "got: {json}");
    assert!(
        json.contains(&b),
        "blocked-by ref should be recorded: {json}"
    );
    assert!(json.contains("waiting on b"), "reason recorded: {json}");
}

#[test]
fn blocked_refuses_an_empty_blocker() {
    // `--blocked` with neither a ref nor a reason is refused: a blocked node
    // must always state why it is blocked.
    let s = Store::new();
    s.epic("e");
    let a = s.ticket("e", "a");
    let err = s.fail(&["ticket", "status", &a, "--blocked"]);
    assert!(
        err.contains("requires a blocker"),
        "empty blocker must be refused: {err}"
    );
    // The node stays in its prior state, not moved to blocked.
    let json = s.ok(&["ticket", "show", &a, "--json"]);
    assert!(json.contains("\"status\": \"to-do\""), "unchanged: {json}");
}

#[test]
fn reactivating_a_closed_node_drops_its_close_reason() {
    // A non-closed node must not carry a close-reason; leaving `closed` clears
    // it, mirroring how leaving `blocked` clears blocked-by.
    let s = Store::new();
    s.epic("e");
    let a = s.ticket("e", "a");
    s.ok(&["ticket", "status", &a, "--closed", "--reason", "superseded"]);
    let closed = s.ok(&["ticket", "show", &a, "--json"]);
    assert!(closed.contains("superseded"), "reason stored: {closed}");

    s.ok(&["ticket", "status", &a, "--in-progress"]);
    let reopened = s.ok(&["ticket", "show", &a, "--json"]);
    assert!(
        reopened.contains("\"status\": \"in-progress\""),
        "reactivated: {reopened}"
    );
    assert!(
        reopened.contains("\"close-reason\": null"),
        "stale close-reason must be cleared: {reopened}"
    );
}

#[test]
fn blocked_is_never_set_automatically_by_other_operations() {
    let s = Store::new();
    s.epic("e");
    let a = s.ticket("e", "a");
    let b = s.ticket("e", "b");
    // Block a on b, then resolve b. loti must not clear a's blocked state.
    s.ok(&[
        "ticket",
        "status",
        &a,
        "--blocked",
        "--blocked-by",
        &b,
        "--reason",
        "waiting",
    ]);
    s.ok(&["ticket", "status", &b, "--done"]);
    let json = s.ok(&["ticket", "show", &a, "--json"]);
    assert!(
        json.contains("\"status\": \"blocked\""),
        "blocked must not be cleared automatically, got: {json}"
    );
}

#[test]
fn epic_with_no_nodes_is_open_not_completed() {
    let s = Store::new();
    s.epic("e");
    let json = s.ok(&["epic", "show", "e", "--json"]);
    assert!(
        json.contains("\"status\": \"open\""),
        "an epic with no nodes is open, got: {json}"
    );
}

#[test]
fn epic_is_completed_when_all_nodes_are_terminal() {
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "t");
    s.ok(&["ticket", "status", &t, "--done"]);
    let json = s.ok(&["epic", "show", "e", "--json"]);
    assert!(
        json.contains("\"status\": \"completed\""),
        "all nodes terminal => completed, got: {json}"
    );
}

#[test]
fn epic_open_when_any_node_is_non_terminal() {
    let s = Store::new();
    s.epic("e");
    let a = s.ticket("e", "a");
    let _b = s.ticket("e", "b");
    s.ok(&["ticket", "status", &a, "--done"]);
    let json = s.ok(&["epic", "show", "e", "--json"]);
    assert!(
        json.contains("\"status\": \"open\""),
        "one open node => epic open, got: {json}"
    );
}

#[test]
fn epic_closed_flag_takes_precedence_over_computed_state() {
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "t");
    s.ok(&["ticket", "status", &t, "--done"]); // would compute to completed
    s.ok(&["epic", "status", "e", "--closed", "--reason", "cancelled"]);
    let json = s.ok(&["epic", "show", "e", "--json"]);
    assert!(
        json.contains("\"status\": \"closed\""),
        "closed flag wins over completed, got: {json}"
    );
    // The flag is reversible: reopening returns to the computed state.
    s.ok(&["epic", "status", "e", "--open"]);
    let json = s.ok(&["epic", "show", "e", "--json"]);
    assert!(
        json.contains("\"status\": \"completed\""),
        "reopening restores the computed state, got: {json}"
    );
}
