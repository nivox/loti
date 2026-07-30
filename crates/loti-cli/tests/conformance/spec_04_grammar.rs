//! Conformance: the CLI command surface & grammar.
//!
//! The normative rules exercised here:
//!   * grammar is noun-verb (`epic`/`ticket` + verbs, collections nesting);
//!   * free-form/binary payloads (body, comment text, asset data) come only
//!     from stdin or `--file`, never an inline flag — there is no `--body`;
//!   * both stdin and `--file` are accepted, and `--file` wins over stdin;
//!   * an actor (`-u` xor `-a <name>`) is required on comment add/edit/delete
//!     and only there; every other operation is actor-agnostic.

use super::harness::Store;

#[test]
fn body_has_no_inline_flag() {
    let s = Store::new();
    // `--body` does not exist: the parser rejects it outright.
    let err = s.fail_stdin(
        &[
            "epic",
            "create",
            "e",
            "--name",
            "n",
            "--summary",
            "s",
            "--body",
            "inline",
        ],
        "",
    );
    assert!(
        err.contains("--body") || err.to_lowercase().contains("unexpected"),
        "an inline --body must be rejected, got: {err}"
    );
}

#[test]
fn body_can_come_from_stdin() {
    let s = Store::new();
    s.ok_stdin(
        &["epic", "create", "e", "--name", "n", "--summary", "s"],
        "body from stdin\n",
    );
    let raw = std::fs::read_to_string(s.path("e/epic.md")).unwrap();
    assert!(
        raw.ends_with("body from stdin\n"),
        "stdin body stored: {raw}"
    );
}

#[test]
fn body_can_come_from_a_file_and_file_wins_over_stdin() {
    let s = Store::new();
    let f = s.path("payload.md");
    std::fs::write(&f, b"body from file\n").unwrap();
    // Feed a different stdin to prove `--file` takes precedence.
    s.ok_stdin(
        &[
            "epic",
            "create",
            "e",
            "--name",
            "n",
            "--summary",
            "s",
            "--file",
            f.to_str().unwrap(),
        ],
        "body from stdin\n",
    );
    let raw = std::fs::read_to_string(s.path("e/epic.md")).unwrap();
    assert!(raw.ends_with("body from file\n"), "--file wins: {raw}");
}

#[test]
fn file_dash_names_stdin_explicitly() {
    // `--file -` is the conventional Unix name for stdin: it must read the pipe,
    // not a file literally named "-".
    let s = Store::new();
    s.ok_stdin(
        &[
            "epic",
            "create",
            "e",
            "--name",
            "n",
            "--summary",
            "s",
            "--file",
            "-",
        ],
        "body via dash\n",
    );
    let raw = std::fs::read_to_string(s.path("e/epic.md")).unwrap();
    assert!(
        raw.ends_with("body via dash\n"),
        "--file - reads stdin: {raw}"
    );
}

#[test]
fn comment_text_from_file_is_recorded() {
    let s = Store::new();
    s.epic("e");
    let f = s.path("note.txt");
    std::fs::write(&f, b"a filed note").unwrap();
    s.ok(&[
        "epic",
        "comment",
        "add",
        "e",
        "-u",
        "--file",
        f.to_str().unwrap(),
    ]);
    let listed = s.ok(&["epic", "comment", "list", "e"]);
    assert!(
        listed.contains("a filed note"),
        "comment text stored: {listed}"
    );
}

#[test]
fn comment_add_requires_an_actor() {
    let s = Store::new();
    s.epic("e");
    // No -u/-a: the required actor group makes this a parse error.
    let err = s.fail_stdin(&["epic", "comment", "add", "e"], "text");
    assert!(
        err.to_lowercase().contains("required") || err.contains("--user"),
        "comment add must require an actor, got: {err}"
    );
}

#[test]
fn actor_is_exclusive_user_xor_agent() {
    let s = Store::new();
    s.epic("e");
    let err = s.fail_stdin(&["epic", "comment", "add", "e", "-u", "-a", "bot"], "text");
    assert!(
        err.to_lowercase().contains("cannot be used with")
            || err.to_lowercase().contains("conflict"),
        "-u and -a are mutually exclusive, got: {err}"
    );
}

#[test]
fn comment_edit_and_delete_require_an_actor() {
    let s = Store::new();
    s.epic("e");
    s.ok_stdin(&["epic", "comment", "add", "e", "-a", "bot"], "hello");
    // edit / delete both demand an actor.
    let e_edit = s.fail_stdin(&["epic", "comment", "edit", "e", "1"], "changed");
    assert!(
        e_edit.to_lowercase().contains("required") || e_edit.contains("--user"),
        "comment edit needs an actor: {e_edit}"
    );
    let e_del = s.fail(&["epic", "comment", "delete", "e", "1"]);
    assert!(
        e_del.to_lowercase().contains("required") || e_del.contains("--user"),
        "comment delete needs an actor: {e_del}"
    );
}

#[test]
fn non_comment_operations_are_actor_agnostic() {
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "t");
    // None of these accept or need an actor; they simply succeed.
    s.ok(&["ticket", "label", "add", &t, "urgent"]);
    s.ok(&["ticket", "status", &t, "--in-progress"]);
    let f = s.path("asset.bin");
    std::fs::write(&f, b"data").unwrap();
    s.ok(&[
        "ticket",
        "asset",
        "add",
        &t,
        "--name",
        "asset.bin",
        "--file",
        f.to_str().unwrap(),
    ]);
    s.ok(&["ticket", "asset", "list", &t]);
    // Passing an actor to a non-comment op is rejected by the parser (the flag
    // is not defined there), confirming actor is scoped to comments only.
    let err = s.fail(&["ticket", "label", "add", &t, "-u", "more"]);
    assert!(
        err.to_lowercase().contains("unexpected") || err.contains("-u"),
        "actor flags are not accepted outside comments: {err}"
    );
}

#[test]
fn claim_take_requires_the_as_flag() {
    // The claimer identifier is a one-liner supplied inline via --as; omitting
    // it is a grammar error (never read from stdin).
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "t");
    let err = s.fail(&["ticket", "claim", "take", &t]);
    assert!(
        err.contains("--as"),
        "take without --as is a grammar error naming the flag, got: {err}"
    );
}

#[test]
fn claim_confirmations_echo_the_target_name_and_reassignment() {
    // A mutation confirmation names the target (ref + name); a reassignment also
    // names the prior holder so overwriting a claim is never silent.
    let s = Store::new();
    s.epic("checkout");
    let t = s.ticket("checkout", "Add step-up prompt");

    let claimed = s.ok(&["ticket", "claim", "take", &t, "--as", "alice"]);
    assert!(
        claimed.contains("(Add step-up prompt)") && claimed.contains("alice"),
        "claim take echoes the name and holder: {claimed}"
    );

    let reclaimed = s.ok(&["ticket", "claim", "take", &t, "--as", "bob"]);
    assert!(
        reclaimed.contains("bob") && reclaimed.contains("alice"),
        "reassignment names both the new and prior holder: {reclaimed}"
    );

    // Re-taking with the same holder refreshes the timestamp rather than
    // reporting a reassignment.
    let refreshed = s.ok(&["ticket", "claim", "take", &t, "--as", "bob"]);
    assert!(
        refreshed.to_lowercase().contains("refresh") && refreshed.contains("bob"),
        "re-taking by the same holder is a refresh: {refreshed}"
    );

    let released = s.ok(&["ticket", "claim", "release", &t]);
    assert!(
        released.contains("(Add step-up prompt)") && released.contains("bob"),
        "release echoes the name and the prior holder: {released}"
    );
}

#[test]
fn status_and_edit_confirmations_echo_the_target_name() {
    // Refs are numeric, so a wrong-but-valid number would apply silently to the
    // wrong node. Mutation confirmations therefore echo the resolved name, so a
    // mistargeted command is caught by eye against the printed name.
    let s = Store::new();
    s.epic("checkout");
    let t = s.ticket("checkout", "Add step-up prompt");

    let moved = s.ok(&["ticket", "status", &t, "--in-progress"]);
    assert!(
        moved.contains("(Add step-up prompt)") && moved.contains("in-progress"),
        "ticket status echoes the name: {moved}"
    );

    let edited = s.ok(&["ticket", "edit", &t, "--summary", "revised"]);
    assert!(
        edited.contains("(Add step-up prompt)"),
        "ticket edit echoes the name: {edited}"
    );

    let epic_status = s.ok(&["epic", "status", "checkout", "--closed", "--reason", "r"]);
    assert!(
        epic_status.contains("(checkout)") && epic_status.contains("closed"),
        "epic status echoes the name: {epic_status}"
    );

    let epic_edit = s.ok(&["epic", "edit", "checkout", "--summary", "revised"]);
    assert!(
        epic_edit.contains("(checkout)"),
        "epic edit echoes the name: {epic_edit}"
    );
}

#[test]
fn the_write_precondition_is_offered_only_where_a_whole_field_is_replaced() {
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "t");
    let stamp = s
        .ok(&["ticket", "show", &t, "--raw", "--field", "updated"])
        .trim()
        .to_string();

    // Creating and appending replace nobody's text, and a status pick's conflict
    // is simply the later of two deliberate choices: none of them takes a stamp,
    // so the parser rejects the flag outright.
    for args in [
        vec!["epic", "create", "e2", "--name", "n", "--summary", "s"],
        vec!["ticket", "comment", "add", &t, "-u"],
        vec!["ticket", "status", &t, "--in-progress"],
        vec!["ticket", "label", "add", &t, "l"],
    ] {
        let mut with_flag = args.clone();
        with_flag.extend(["--expect-updated", &stamp]);
        let err = s.fail_stdin(&with_flag, "x");
        assert!(
            err.contains("--expect-updated") || err.to_lowercase().contains("unexpected"),
            "{args:?} must not accept a write precondition, got: {err}"
        );
    }
}

#[test]
fn an_unparseable_expected_stamp_is_refused_before_anything_is_written() {
    let s = Store::new();
    s.epic("e");
    let err = s.fail_stdin(
        &[
            "epic",
            "edit",
            "e",
            "--name",
            "renamed",
            "--expect-updated",
            "yesterday",
        ],
        "",
    );
    assert!(
        err.contains("updated") && err.contains("show"),
        "the refusal must say where the stamp comes from, got: {err}"
    );
    let json = s.ok(&["epic", "show", "e", "--json"]);
    assert!(
        !json.contains("renamed"),
        "a rejected stamp leaves the target untouched: {json}"
    );
}
