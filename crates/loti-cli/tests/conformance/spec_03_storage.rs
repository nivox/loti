//! Conformance: on-disk storage format, layout, and root discovery.
//!
//! The normative rules exercised here:
//!   * one flat directory per epic — `<epic>/epic.md` and `<epic>/<n>.md` — with
//!     no nested folders; the tree is a `parent` field, not a location;
//!   * all structured data lives in YAML frontmatter, then a free-form body;
//!   * a writer round-trips (preserves) unknown frontmatter keys it does not
//!     understand — never dropping or erroring on them;
//!   * root discovery walks upward to a `.loti/` marker or a `.loti.conf`
//!     pointer, the config file winning at a level, and `--root` overriding;
//!   * numbers are a flat per-epic pool: unique within an epic, never reused,
//!     and free to collide across epics.

use super::harness::Store;

#[test]
fn layout_is_flat_files_per_epic_with_no_nested_folders() {
    let s = Store::new();
    s.epic("e");
    let parent = s.ticket("e", "parent");
    let _child = s.subticket("e", &parent, "child");

    // The epic file and both node files sit directly in the epic directory.
    assert!(s.path("e/epic.md").is_file(), "epic file present");
    assert!(s.path("e/1.md").is_file(), "node 1 present");
    assert!(s.path("e/2.md").is_file(), "node 2 present");

    // No nested subtree directory encodes the parent/child relationship.
    assert!(
        !s.path("e/1").is_dir() || s.path("e/1").read_dir().unwrap().next().is_none(),
        "the parent relationship must not be a nested folder"
    );
    // The child records its parent as metadata, not by location.
    let child_json = s.ok(&["ticket", "show", "e/2", "--json"]);
    assert!(
        child_json.contains("\"parent\": \"e/1\""),
        "child parent is metadata: {child_json}"
    );
}

#[test]
fn frontmatter_and_body_are_split_by_the_delimiter() {
    let s = Store::new();
    s.epic("e");
    s.ok_stdin(
        &["ticket", "create", "e", "--name", "t", "--summary", "sum"],
        "the free body\nsecond line\n",
    );
    let raw = std::fs::read_to_string(s.path("e/1.md")).unwrap();
    // The file opens with a frontmatter block delimited by `---`, then the body
    // verbatim below it.
    assert!(raw.starts_with("---\n"), "opens with a delimiter: {raw}");
    let (_front, body) = raw[4..].split_once("\n---\n").expect("closing delimiter");
    assert_eq!(
        body, "the free body\nsecond line\n",
        "verbatim body preserved"
    );
}

#[test]
fn unknown_frontmatter_keys_survive_a_mutation() {
    // The tolerant-reader rule: a writer preserves keys it does not understand.
    let s = Store::new();
    s.epic("e");
    s.ticket("e", "t");

    // Hand-inject an unknown key (and a nested unknown block) into the node's
    // frontmatter, as a future format version or a human might.
    let path = s.path("e/1.md");
    let raw = std::fs::read_to_string(&path).unwrap();
    let injected = raw.replacen(
        "status: to-do\n",
        "status: to-do\nfuture-scalar: keep-me\nfuture-block:\n  a: 1\n  b: two\n",
        1,
    );
    assert_ne!(injected, raw, "injection point found");
    std::fs::write(&path, &injected).unwrap();

    // Drive a mutation through the CLI that rewrites the whole file.
    s.ok(&["ticket", "edit", "e/1", "--name", "renamed"]);

    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("renamed"), "the mutation applied: {after}");
    assert!(
        after.contains("future-scalar: keep-me"),
        "unknown scalar key must be preserved: {after}"
    );
    assert!(
        after.contains("future-block:") && after.contains("two"),
        "unknown nested block must be preserved: {after}"
    );
}

#[test]
fn discovery_finds_the_store_from_a_subdirectory_via_the_marker() {
    // With no --root, a command run from a nested directory walks upward to the
    // nearest `.loti/` marker.
    let s = Store::new_discovered();
    s.epic("e");
    let nested = s.path("deep/nested/dir");
    std::fs::create_dir_all(&nested).unwrap();

    let mut cmd = assert_cmd::Command::cargo_bin("loti").unwrap();
    cmd.current_dir(&nested).env("NO_COLOR", "1");
    let out = cmd
        .args(["epic", "show", "e", "--raw", "--field", "id"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "discovery from a subdir should find the store: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "e");
}

#[test]
fn config_pointer_wins_and_root_override_beats_discovery() {
    // A `.loti.conf` pointing elsewhere directs discovery to that data root.
    let outer = tempfile::TempDir::new().unwrap();
    let data = tempfile::TempDir::new().unwrap();
    // Real data store lives in `data`.
    std::fs::create_dir_all(data.path().join(".loti")).unwrap();
    std::fs::write(
        data.path().join(".loti").join("meta"),
        "format-version = \"0.1\"\n",
    )
    .unwrap();
    // `outer` carries only a pointer to it.
    std::fs::write(
        outer.path().join(".loti.conf"),
        format!("loti-root = \"{}\"\n", data.path().display()),
    )
    .unwrap();

    // Seed an epic in the pointed-at data root (via --root, unambiguously).
    let seed = assert_cmd::Command::cargo_bin("loti")
        .unwrap()
        .args(["--root"])
        .arg(data.path())
        .args(["epic", "create", "viaconf", "--name", "n", "--summary", "s"])
        .write_stdin("")
        .output()
        .unwrap();
    assert!(seed.status.success());

    // Running from `outer` with no --root must resolve through the pointer.
    let mut cmd = assert_cmd::Command::cargo_bin("loti").unwrap();
    cmd.current_dir(outer.path()).env("NO_COLOR", "1");
    let out = cmd
        .args(["epic", "show", "viaconf", "--raw", "--field", "id"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the config pointer should direct discovery: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "viaconf");
}

#[test]
fn numbers_are_unique_and_never_reused_within_an_epic() {
    let s = Store::new();
    s.epic("e");
    let a = s.ticket("e", "a");
    let b = s.ticket("e", "b");
    assert_eq!(a, "e/1");
    assert_eq!(b, "e/2");
    // Even if a node file is removed, the counter has advanced past it, so the
    // number is never handed out again.
    std::fs::remove_file(s.path("e/1.md")).unwrap();
    let c = s.ticket("e", "c");
    assert_eq!(c, "e/3", "a freed number is never reused");
}

#[test]
fn numbers_may_collide_across_epics() {
    let s = Store::new();
    s.epic("a");
    s.epic("b");
    let na = s.ticket("a", "first");
    let nb = s.ticket("b", "first");
    // Each epic has its own pool, so number 1 appears in both.
    assert_eq!(na, "a/1");
    assert_eq!(nb, "b/1");
}
