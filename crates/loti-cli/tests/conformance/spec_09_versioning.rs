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
//! Testability note: this binary's format version has major 0, which is the
//! lowest possible major, so a store with a *smaller* major than the binary
//! cannot be constructed to exercise the "older major => reads OK, mutations
//! refused" branch of the matrix black-box. That branch is covered by the
//! core-crate unit tests over the version matrix; here we assert every branch
//! that a major-0 binary can actually observe. The minor-behind path (compatible
//! reads+writes, and a meta-only migrate) stands in for the same-major behaviour
//! the older-major path would settle into after migration.

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
    s.set_meta(&meta("0.1"));
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
    // A store one minor behind (0.0 vs binary 0.1) reads and writes normally.
    s.set_meta(&meta("0.0"));
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
    s.set_meta(&meta("0.1-migrate"));
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

    s.set_meta(&meta("0.0"));
    let out = s.ok(&["migrate-store"]);
    assert!(
        out.to_lowercase().contains("no rewrite") || out.to_lowercase().contains("version"),
        "a minor migration reports a meta-only bump, got: {out}"
    );

    // The recorded version is now the binary's, and the store bytes are intact.
    let recorded = std::fs::read_to_string(s.path(".loti/meta")).unwrap();
    assert!(
        recorded.contains("0.1"),
        "version bumped to current: {recorded}"
    );
    let after = std::fs::read(s.path("e/1.md")).unwrap();
    assert_eq!(
        before, after,
        "a minor migration does not rewrite the store"
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
