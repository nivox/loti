//! Conformance: format versioning & migration.
//!
//! The normative rules exercised here:
//!   * a store whose major is newer than this binary refuses everything,
//!     including reads, with an "upgrade loti" message — never read-guessing,
//!     never writing;
//!   * a minor difference within a major is compatible in both directions and
//!     gates nothing;
//!   * the mid-migration sentinel makes the store read-only for every binary
//!     (reads still work, mutations are refused pointing at migrate-store);
//!   * `migrate-store` on a store that is only minor-behind is a metadata-only
//!     version bump with no store rewrite.
//!
//! Testability note: this binary's format version is major 1, so a store one
//! major behind (major 0) can be constructed to exercise the real major
//! migration — the 0->1 split of the coupled `blocked-by` map into an
//! independent dependency list plus `block-reason`. The minor-behind path
//! (compatible reads+writes, and a meta-only migrate) is exercised against a
//! `1.0` store, one minor below this binary's `1.2`.

use super::harness::Store;

/// A store metadata string for a fabricated `major.minor`.
fn meta(version: &str) -> String {
    format!("format-version = \"{version}\"\n")
}

#[test]
fn a_store_newer_major_refuses_reads() {
    let s = Store::new();
    s.epic("e");
    s.ticket("e", "t");
    s.set_meta(&meta("9.0"));
    let err = s.fail(&["ticket", "show", "e/1", "--json"]);
    assert!(
        err.to_lowercase().contains("newer") || err.to_lowercase().contains("upgrade"),
        "a too-new store must refuse reads with an upgrade message, got: {err}"
    );
}

#[test]
fn a_store_newer_major_refuses_mutations() {
    let s = Store::new();
    s.epic("e");
    s.set_meta(&meta("9.0"));
    let err = s.fail_stdin(&["epic", "comment", "add", "e", "-u"], "x");
    assert!(
        err.to_lowercase().contains("newer") || err.to_lowercase().contains("upgrade"),
        "a too-new store must refuse mutations, got: {err}"
    );
    // And it truly did not write: restoring a current meta shows no comment.
    s.set_meta(&meta("1.2"));
    let listed = s.ok(&["epic", "comment", "list", "e"]);
    assert!(
        listed.trim().is_empty(),
        "a refused mutation must not have written: {listed}"
    );
}

#[test]
fn a_minor_difference_within_a_major_is_compatible() {
    let s = Store::new();
    s.epic("e");
    // A store one minor behind (1.0 vs binary 1.2) reads and writes normally.
    s.set_meta(&meta("1.0"));
    let read = s.ok(&["epic", "show", "e", "--raw", "--field", "id"]);
    assert_eq!(read.trim(), "e", "minor-behind reads are fine");
    s.ok_stdin(&["epic", "comment", "add", "e", "-u"], "compatible write");
    let listed = s.ok(&["epic", "comment", "list", "e"]);
    assert!(
        listed.contains("compatible write"),
        "minor-behind writes are fine: {listed}"
    );
}

#[test]
fn the_migration_sentinel_makes_the_store_read_only() {
    let s = Store::new();
    s.epic("e");
    s.ticket("e", "t");
    // A store observed mid-migration: reads still work, mutations are refused.
    s.set_meta(&meta("1.1-migrate"));
    // A read still succeeds under the sentinel.
    let read = s.ok(&["ticket", "show", "e/1", "--raw", "--field", "name"]);
    assert_eq!(read.trim(), "t", "reads work under the sentinel");
    // A mutation is refused, pointing at migrate-store.
    let err = s.fail_stdin(&["epic", "comment", "add", "e", "-u"], "x");
    assert!(
        err.to_lowercase().contains("migrat"),
        "the sentinel must refuse mutations pointing at migrate-store, got: {err}"
    );
}

#[test]
fn migrate_store_minor_behind_is_a_meta_only_bump() {
    let s = Store::new();
    s.epic("e");
    s.ticket("e", "t");
    // Capture the node bytes before migration to prove no store rewrite happens.
    let before = std::fs::read(s.path("e/1.md")).unwrap();

    s.set_meta(&meta("1.0"));
    let out = s.ok(&["migrate-store"]);
    assert!(
        out.to_lowercase().contains("no rewrite") || out.to_lowercase().contains("version"),
        "a minor migration reports a meta-only bump, got: {out}"
    );

    // The recorded version is now the binary's, and the store bytes are intact.
    let recorded = std::fs::read_to_string(s.store_path("meta")).unwrap();
    assert!(
        recorded.contains("1.2"),
        "version bumped to current: {recorded}"
    );
    let after = std::fs::read(s.path("e/1.md")).unwrap();
    assert_eq!(
        before, after,
        "a minor migration does not rewrite the store"
    );
}

#[test]
fn migrate_store_major_behind_splits_the_old_blocked_by_map() {
    // A store one major behind (major 0) carries the coupled
    // `blocked-by: {refs, reason}` map on a blocked node. Migrating to this
    // binary's major must split it into an independent `blocked-by` ref list
    // plus a `block-reason`, so the two concepts are decoupled afterwards.
    let s = Store::new();
    s.epic("e");
    s.ticket("e", "blocker"); // e/1
    s.ticket("e", "t"); // e/2

    // Overwrite e/2 with the old major-0 shape by hand, then mark the store
    // major-0 so migrate-store runs the real transform.
    let old = "---\n\
         number: 2\n\
         name: t\n\
         summary: s\n\
         status: blocked\n\
         blocked-by:\n  refs:\n  - e/1\n  reason: waiting on a key\n\
         created: 2024-01-01T00:00:00Z\n\
         updated: 2024-01-01T00:00:00Z\n\
         ---\nbody\n";
    std::fs::write(s.path("e/2.md"), old).unwrap();
    s.set_meta(&meta("0.1"));

    s.ok(&["migrate-store"]);

    // The migrated node exposes the new decoupled shape: blocked-by is a plain
    // ref array, and the old reason now lives in block-reason.
    let json = s.ok(&["ticket", "show", "e/2", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["blocked-by"], serde_json::json!(["e/1"]));
    assert_eq!(v["block-reason"], "waiting on a key");
    assert_eq!(v["status"], "blocked");

    // The store is fully usable again after the major migration.
    s.ok_stdin(
        &["epic", "comment", "add", "e", "-u"],
        "usable after migrate",
    );
}

#[test]
fn migrate_store_at_current_version_is_a_no_op_and_leaves_it_usable() {
    let s = Store::new();
    s.epic("e");
    // Already current: migrate-store should succeed without harm and leave the
    // store fully mutable.
    s.ok(&["migrate-store"]);
    s.ok_stdin(&["epic", "comment", "add", "e", "-u"], "still writable");
    let listed = s.ok(&["epic", "comment", "list", "e"]);
    assert!(
        listed.contains("still writable"),
        "usable after migrate: {listed}"
    );
}
