//! Conformance: the `skill` subcommand & help system.
//!
//! The normative rules exercised here:
//!   * `loti skill` prints static, hand-authored content carrying the hard rule
//!     that agents must drive the CLI and never hand-edit the store files;
//!   * `--help-full` emits the complete command tree in one pass, carrying the
//!     per-flag input rule (stdin / `--file`) and the actor requirement on
//!     comment operations;
//!   * user-facing help/skill output carries no specification-section or ticket
//!     references (behaviour is described in plain terms).

use super::harness::run_bare;

fn stdout_of(args: &[&str]) -> String {
    let out = run_bare(args);
    assert!(
        out.status.success(),
        "{args:?} should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn skill_prints_the_hard_rule() {
    let text = stdout_of(&["skill"]);
    // The prominent MUST-NOT: never hand-edit the store / never bypass the CLI.
    let lower = text.to_lowercase();
    assert!(
        (lower.contains("never") && lower.contains("hand"))
            || lower.contains("do not hand")
            || lower.contains("must not"),
        "skill must carry the hard no-hand-edit rule:\n{text}"
    );
    assert!(
        lower.contains("cli"),
        "skill must tell the agent to drive the CLI:\n{text}"
    );
}

#[test]
fn skill_guides_launched_agents_to_their_workflow_not_operator_profiles() {
    let text = stdout_of(&["skill"]);
    assert!(
        text.contains("follow the workflow named for your session"),
        "skill must direct a launched agent to its workflow:\n{text}"
    );
    assert!(
        text.contains("Do not use the\noperator-facing `loti agent` profile commands"),
        "skill must keep launched agents away from operator profiles:\n{text}"
    );
}

#[test]
fn skill_is_self_contained_prose_with_concepts() {
    let text = stdout_of(&["skill"]);
    // Distilled core concepts ship in the skill (states, epics/tickets), so it
    // stands alone without the separate vocabulary document.
    let lower = text.to_lowercase();
    assert!(lower.contains("epic"), "concepts include epics:\n{text}");
    assert!(
        lower.contains("ticket"),
        "concepts include tickets:\n{text}"
    );
    assert!(
        lower.contains("blocked") || lower.contains("in-progress") || lower.contains("to-do"),
        "concepts include node states:\n{text}"
    );
}

#[test]
fn help_full_covers_the_whole_surface() {
    let text = stdout_of(&["--help-full"]);
    // Nouns, verbs, and collections all appear in the one-pass dump.
    for needle in [
        "epic",
        "ticket",
        "create",
        "show",
        "edit",
        "status",
        "label",
        "comment",
        "asset",
        "list",
        "init",
        "agent",
        "workflow",
        "skill",
        "tui",
        "migrate-store",
    ] {
        assert!(text.contains(needle), "--help-full missing '{needle}'");
    }
}

#[test]
fn tui_is_part_of_the_surface_and_says_it_needs_a_terminal() {
    // The browser is a top-level command like any other, and its help states the
    // one precondition a caller cannot discover from the grammar: it takes over
    // an interactive terminal, so it cannot be piped or scripted.
    let text = stdout_of(&["tui", "--help"]).to_lowercase();
    assert!(
        text.contains("terminal"),
        "`tui --help` must state that it needs an interactive terminal:\n{text}"
    );
}

#[test]
fn help_full_annotates_input_rule_and_actor_requirement() {
    let text = stdout_of(&["--help-full"]);
    let lower = text.to_lowercase();
    // The per-flag input rule: payloads come from stdin or --file.
    assert!(
        lower.contains("stdin") && lower.contains("--file"),
        "--help-full must annotate the stdin/--file input rule:\n{text}"
    );
    // The actor requirement on comment operations.
    assert!(
        lower.contains("actor"),
        "--help-full must annotate the actor requirement on comments:\n{text}"
    );
}

#[test]
fn skill_and_help_full_carry_no_spec_or_ticket_references() {
    // User-facing text describes behaviour in plain terms — no drifting pointers.
    for args in [&["skill"][..], &["--help-full"][..]] {
        let text = stdout_of(args);
        assert!(
            !text.contains('§'),
            "{args:?} must not contain a section sign"
        );
        assert!(!text.contains("SPEC"), "{args:?} must not mention SPEC");
        assert!(
            !text.contains("loti-impl/") && !text.contains("loti-spec/"),
            "{args:?} must not reference tracker tickets"
        );
    }
}
