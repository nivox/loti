//! Conformance: concurrency & multi-actor safety on a local POSIX filesystem.
//!
//! The normative rules exercised here (precondition: a POSIX local FS with
//! atomic same-directory rename and atomic exclusive-create):
//!   * competing writers creating nodes in one epic each get a distinct number,
//!     none reused, and no file is lost — the node-creation race is resolved by
//!     exclusive-create;
//!   * a mutation never leaves a torn/partial target file (atomic rename);
//!   * a stale advisory lock (an abandoned staging temp) makes a mutation fail
//!     fast rather than hang or clobber;
//!   * a cascade is idempotent — re-running one that stopped partway converges;
//!   * the commands that replace a whole field carry the opt-in write
//!     precondition: named, the stamp must still match or the write is refused
//!     with nothing written; omitted, the last write wins as it always did.

use std::sync::mpsc;
use std::thread;

use super::harness::Store;

/// The target's current `updated` stamp, read back through the CLI exactly as a
/// caller composing a replacement would read it.
fn updated_of(s: &Store, noun: &str, reference: &str) -> String {
    s.ok(&[noun, "show", reference, "--raw", "--field", "updated"])
        .trim()
        .to_string()
}

#[test]
fn competing_writers_get_distinct_never_reused_numbers() {
    let s = Store::new();
    s.epic("e");

    const WRITERS: usize = 12;
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for i in 0..WRITERS {
        // Each writer is an independent process invocation against the same
        // store, contending for numbers in the same epic.
        let root = s.root().to_path_buf();
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            let out = assert_cmd::Command::cargo_bin("loti")
                .unwrap()
                .arg("--root")
                .arg(&root)
                .args(["ticket", "create", "e", "--name"])
                .arg(format!("n{i}"))
                .args(["--summary", "s"])
                .env("NO_COLOR", "1")
                .write_stdin("")
                .output()
                .unwrap();
            tx.send(out.status.success()).unwrap();
        }));
    }
    drop(tx);
    for h in handles {
        h.join().unwrap();
    }
    let all_ok = rx.iter().all(|ok| ok);
    assert!(all_ok, "every concurrent create should succeed");

    // Collect the node numbers actually on disk: exactly WRITERS distinct files,
    // numbered 1..=WRITERS with none missing or duplicated.
    let mut numbers: Vec<u64> = std::fs::read_dir(s.path("e"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stem = name.strip_suffix(".md")?;
            stem.parse::<u64>().ok()
        })
        .collect();
    numbers.sort_unstable();
    let expected: Vec<u64> = (1..=WRITERS as u64).collect();
    assert_eq!(
        numbers, expected,
        "distinct, contiguous, never-reused numbers under contention"
    );

    // Every node file parses back cleanly (no torn writes).
    for n in &numbers {
        let show = s.ok(&["ticket", "show", &format!("e/{n}"), "--json"]);
        let _: serde_json::Value = serde_json::from_str(&show).expect("each node parses");
    }
}

#[test]
fn a_mutation_never_leaves_a_torn_target_file() {
    // Repeated edits, each read back as valid — the atomic rename means a reader
    // only ever sees a whole file, never a half-written one.
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "t");
    for i in 0..8 {
        s.ok(&["ticket", "edit", &t, "--name", &format!("rename{i}")]);
        let json = s.ok(&["ticket", "show", &t, "--json"]);
        let v: serde_json::Value = serde_json::from_str(&json).expect("whole file, always parses");
        assert_eq!(v["name"], format!("rename{i}"));
        // No staging temp is left behind after a completed mutation.
        assert!(
            !s.path("e/.1.md.tmp").exists(),
            "the staging temp is consumed by the rename"
        );
    }
}

#[test]
fn a_stale_lock_makes_a_mutation_fail_fast() {
    let s = Store::new();
    s.epic("e");
    let _t = s.ticket("e", "t");

    // Plant an abandoned staging temp for node 1 and backdate its mtime well
    // past the liveness threshold, so it reads as stale (owner presumed dead).
    let temp = s.path("e/.1.md.tmp");
    std::fs::write(&temp, b"").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
    let f = std::fs::OpenOptions::new().write(true).open(&temp).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(old))
        .unwrap();

    // A mutation must fail fast (not hang), and its message should point at how
    // to recover from an interrupted operation.
    let err = s.fail(&["ticket", "edit", "e/1", "--name", "renamed"]);
    assert!(
        err.to_lowercase().contains("stale") || err.to_lowercase().contains("lock"),
        "a stale lock should fail fast with a clear message, got: {err}"
    );
    // The target was not modified.
    let json = s.ok(&["ticket", "show", "e/1", "--json"]);
    assert!(
        !json.contains("renamed"),
        "a refused mutation leaves the target untouched: {json}"
    );
}

#[test]
fn cascade_close_is_idempotent_and_re_runnable() {
    // A cascade closes a subtree; running it again is a harmless no-op that
    // converges to the same state (the re-run of a partial cascade in practice).
    let s = Store::new();
    s.epic("e");
    let root = s.ticket("e", "root");
    let mid = s.subticket("e", &root, "mid");
    let leaf = s.subticket("e", &mid, "leaf");

    s.ok(&[
        "ticket",
        "status",
        &root,
        "--closed",
        "--reason",
        "obsolete",
        "--cascade",
    ]);
    // Everything in the subtree is closed.
    for r in [&root, &mid, &leaf] {
        let json = s.ok(&["ticket", "show", r, "--json"]);
        assert!(
            json.contains("\"status\": \"closed\""),
            "{r} closed: {json}"
        );
    }
    // Re-running the same cascade converges (idempotent) and still succeeds.
    s.ok(&[
        "ticket",
        "status",
        &root,
        "--closed",
        "--reason",
        "obsolete",
        "--cascade",
    ]);
    for r in [&root, &mid, &leaf] {
        let json = s.ok(&["ticket", "show", r, "--json"]);
        assert!(
            json.contains("\"status\": \"closed\""),
            "{r} still closed: {json}"
        );
    }
}

#[test]
fn an_epic_edit_applies_on_a_current_stamp_and_is_refused_on_a_stale_one() {
    let s = Store::new();
    s.epic("e");
    let stamp = updated_of(&s, "epic", "e");

    // The stamp the caller read is still the stored one, so the replacement lands.
    s.ok_stdin(
        &[
            "epic",
            "edit",
            "e",
            "--name",
            "kept",
            "--expect-updated",
            &stamp,
        ],
        "",
    );

    // Another actor touches the same epic in a way that replaces none of the
    // fields being edited. Granularity is the epic, not the field, so the stamp
    // the caller is still holding has gone stale.
    s.ok(&["epic", "label", "add", "e", "mid-edit"]);
    let err = s.fail_stdin(
        &[
            "epic",
            "edit",
            "e",
            "--name",
            "lost",
            "--expect-updated",
            &stamp,
        ],
        "",
    );
    assert!(
        err.contains("changed since it was read") && err.contains("nothing was written"),
        "a stale stamp is refused as a conflict, got: {err}"
    );

    // Nothing was written: the name is the one the accepted edit left, and the
    // concurrent label survived.
    let json = s.ok(&["epic", "show", "e", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["name"], "kept", "the refused replacement never landed");
    assert_eq!(
        v["labels"],
        serde_json::json!(["mid-edit"]),
        "the concurrent change was not rolled back"
    );
}

#[test]
fn a_ticket_edit_applies_on_a_current_stamp_and_is_refused_on_a_stale_one() {
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "t");
    let stamp = updated_of(&s, "ticket", &t);

    s.ok_stdin(
        &[
            "ticket",
            "edit",
            &t,
            "--summary",
            "kept",
            "--expect-updated",
            &stamp,
        ],
        "",
    );

    s.ok(&["ticket", "label", "add", &t, "mid-edit"]);
    let err = s.fail_stdin(
        &[
            "ticket",
            "edit",
            &t,
            "--summary",
            "lost",
            "--expect-updated",
            &stamp,
        ],
        "",
    );
    assert!(
        err.contains("changed since it was read") && err.contains("nothing was written"),
        "a stale stamp is refused as a conflict, got: {err}"
    );

    let json = s.ok(&["ticket", "show", &t, "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["summary"], "kept", "the refused replacement never landed");
}

#[test]
fn a_comment_edit_applies_on_a_current_stamp_and_is_refused_on_a_stale_one() {
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "t");
    s.ok_stdin(&["ticket", "comment", "add", &t, "-u"], "first draft");
    let stamp = updated_of(&s, "ticket", &t);

    s.ok_stdin(
        &[
            "ticket",
            "comment",
            "edit",
            &t,
            "1",
            "-u",
            "--expect-updated",
            &stamp,
        ],
        "kept",
    );

    // An appended comment moves the ticket's stamp, so a replacement of an
    // earlier comment's text composed before it arrived is refused.
    s.ok_stdin(
        &["ticket", "comment", "add", &t, "-u"],
        "a note arriving mid-edit",
    );
    let err = s.fail_stdin(
        &[
            "ticket",
            "comment",
            "edit",
            &t,
            "1",
            "-u",
            "--expect-updated",
            &stamp,
        ],
        "lost",
    );
    assert!(
        err.contains("changed since it was read") && err.contains("nothing was written"),
        "a stale stamp is refused as a conflict, got: {err}"
    );

    let listed = s.ok(&["ticket", "comment", "list", &t]);
    assert!(
        listed.contains("kept") && !listed.contains("lost"),
        "the refused replacement never landed: {listed}"
    );
    assert!(
        listed.contains("a note arriving mid-edit"),
        "the concurrent append survived: {listed}"
    );
}

#[test]
fn naming_no_stamp_still_overwrites_a_target_that_changed() {
    // Omitting the precondition is exactly the behaviour that shipped before it
    // existed: the write applies to whatever is there now.
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "t");
    s.ok(&[
        "ticket",
        "edit",
        &t,
        "--summary",
        "written by another actor",
    ]);
    s.ok_stdin(&["ticket", "edit", &t, "--summary", "last write wins"], "");

    let json = s.ok(&["ticket", "show", &t, "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["summary"], "last write wins");
}
