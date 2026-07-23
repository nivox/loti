//! Conformance: read & output formats.
//!
//! The normative rules exercised here:
//!   * `--json` is the canonical whole-entity form on every read;
//!   * `show --markdown` (the default) emits its sections in order: metadata
//!     table, name (H1), summary (blockquote), children table, assets table,
//!     body, then comments;
//!   * `show --raw` yields one value per line and is strict-unambiguous — a
//!     structured/multi-valued selection is a hard error pointing at `--json`;
//!   * `list` has a default indented tree, a flat `--json` array with parent
//!     pointers, an `--ndjson` stream, and a tab-separated `--raw`; it rejects
//!     heavy/structured fields;
//!   * the default plain tree closes with a per-status progress footer over the
//!     nodes listed; machine formats carry no such footer;
//!   * machine formats (`--json`/`--ndjson`/`--raw`) are never coloured.

use super::harness::{contains_ansi, Store};

#[test]
fn show_json_is_canonical_with_all_fields() {
    let s = Store::new();
    s.epic("e");
    s.ok_stdin(
        &["ticket", "create", "e", "--name", "T", "--summary", "sum"],
        "the body\n",
    );
    let json = s.ok(&["ticket", "show", "e/1", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    // The whole entity: identity, status, the body, and the empty collections
    // are all present in the canonical form.
    for key in [
        "ref", "number", "name", "summary", "status", "body", "labels", "comments", "assets",
    ] {
        assert!(v.get(key).is_some(), "canonical JSON missing {key}: {json}");
    }
    assert_eq!(v["ref"], "e/1");
    assert_eq!(v["body"], "the body\n");
}

#[test]
fn show_markdown_emits_sections_in_order() {
    let s = Store::new();
    s.epic("e");
    let parent = s.ticket("e", "Parentname");
    let _child = s.subticket("e", &parent, "Childname");
    let md = s.ok(&["ticket", "show", &parent]);

    // Locate each section and assert the specified ordering.
    let idx = |needle: &str| {
        md.find(needle)
            .unwrap_or_else(|| panic!("missing {needle} in:\n{md}"))
    };
    let meta = idx("| field | value |");
    let name = idx("# Parentname");
    let summary = idx("> ");
    let children = idx("## Subtickets");
    let assets = idx("## Assets");
    let body = idx("## Body");
    let comments = idx("## Comments");
    assert!(
        meta < name && name < summary && summary < children,
        "metadata -> name -> summary -> children order"
    );
    assert!(
        children < assets && assets < body && body < comments,
        "children -> assets -> body -> comments order"
    );
    // The direct child appears in the children table.
    assert!(
        md.contains("Childname"),
        "children table lists the child: {md}"
    );
}

#[test]
fn show_raw_single_leaf_is_one_bare_value() {
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "t");
    s.ok(&["ticket", "status", &t, "--in-progress"]);
    let out = s.ok(&["ticket", "show", &t, "--raw", "--field", "status"]);
    assert_eq!(out.trim(), "in-progress");
}

#[test]
fn show_raw_structured_selection_is_a_hard_error() {
    let s = Store::new();
    s.epic("e");
    s.ticket("e", "t");
    // A structured/multi-valued field cannot render as one unambiguous value.
    let err = s.fail(&["ticket", "show", "e/1", "--raw", "--field", "comments"]);
    assert!(
        err.contains("--json"),
        "the ambiguity error should point at --json, got: {err}"
    );
}

#[test]
fn plain_list_closes_with_a_per_status_progress_footer_absent_from_machine_forms() {
    let s = Store::new();
    s.epic("e");
    s.ticket("e", "a"); // e/1 to-do
    s.ticket("e", "b"); // e/2 to-do
    s.ticket("e", "c"); // e/3 -> done
    s.ok(&["ticket", "status", "e/3", "--done"]);

    let plain = s.ok(&["ticket", "list", "e"]);
    let footer = plain.lines().last().unwrap();
    assert_eq!(
        footer, "3 tickets · 2 to-do · 1 done",
        "plain list closes with the progress footer: {plain}"
    );

    // A narrowed listing tags the footer so a partial count is not read as the
    // whole scope.
    let narrowed = s.ok(&["ticket", "list", "e", "--status", "to-do"]);
    assert!(
        narrowed.lines().last().unwrap().contains("(filtered)"),
        "filtered count is tagged: {narrowed}"
    );

    // Machine formats stay pure data — no footer text leaks in.
    for mode in ["--json", "--ndjson", "--raw"] {
        let out = s.ok(&["ticket", "list", "e", mode]);
        assert!(
            !out.contains("tickets") && !out.contains('\u{2500}'),
            "{mode} carries no progress footer: {out}"
        );
    }
}

#[test]
fn list_default_is_an_indented_depth_first_tree_with_blocked_by_tag() {
    let s = Store::new();
    s.epic("e");
    let parent = s.ticket("e", "parent");
    let child = s.subticket("e", &parent, "child");
    let other = s.ticket("e", "other");
    // The trailing tag reflects the blocked-by dependency list, independent of
    // status: record a dependency on `other`.
    s.ok(&["ticket", "blocked-by", "add", &child, &other]);

    let out = s.ok(&["ticket", "list", "e"]);
    let lines: Vec<&str> = out.lines().collect();
    // Depth-first: the child line immediately follows its parent, and is
    // indented relative to it.
    let p = lines.iter().position(|l| l.contains("parent")).unwrap();
    let c = lines.iter().position(|l| l.contains("child")).unwrap();
    assert_eq!(c, p + 1, "child follows parent depth-first: {out}");
    assert!(
        lines[c].starts_with(' ') && !lines[p].starts_with(' '),
        "child is indented under the parent: {out}"
    );
    assert!(
        lines[c].contains("[blocked-by:") && lines[c].contains(&other),
        "a node with dependencies carries a trailing blocked-by tag: {out}"
    );
}

#[test]
fn list_json_is_a_flat_array_with_parent_pointers() {
    let s = Store::new();
    s.epic("e");
    let parent = s.ticket("e", "parent");
    let _child = s.subticket("e", &parent, "child");
    let out = s.ok(&["ticket", "list", "e", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    let arr = v.as_array().expect("flat array");
    assert_eq!(arr.len(), 2, "flat, not nested: {out}");
    // The child carries a parent pointer; no nesting is used to encode it.
    let child = arr.iter().find(|n| n["ref"] == "e/2").unwrap();
    assert_eq!(child["parent"], "e/1", "child points at its parent: {out}");
    assert!(!contains_ansi(&out), "JSON is never coloured");
}

#[test]
fn list_ndjson_is_one_object_per_line() {
    let s = Store::new();
    s.epic("e");
    s.ticket("e", "a");
    s.ticket("e", "b");
    let out = s.ok(&["ticket", "list", "e", "--ndjson"]);
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "one object per line: {out}");
    for line in lines {
        let _: serde_json::Value = serde_json::from_str(line).expect("each line is JSON");
    }
    assert!(!contains_ansi(&out), "NDJSON is never coloured");
}

#[test]
fn list_raw_is_tab_separated_and_uncoloured() {
    let s = Store::new();
    s.epic("e");
    s.ticket("e", "a");
    let out = s.ok(&["ticket", "list", "e", "--raw"]);
    assert!(out.contains('\t'), "raw rows are tab-separated: {out:?}");
    assert!(!contains_ansi(&out), "raw is never coloured");
}

#[test]
fn list_rejects_heavy_fields() {
    let s = Store::new();
    s.epic("e");
    s.ticket("e", "a");
    // `body` is a show-only heavy field; requesting it on list is a hard error.
    let err = s.fail(&["ticket", "list", "e", "--field", "body"]);
    assert!(
        err.to_lowercase().contains("body") && err.contains("show"),
        "heavy field on list must error pointing at show, got: {err}"
    );
}

#[test]
fn fields_projection_selects_dotted_leaves() {
    let s = Store::new();
    s.epic("e");
    let t = s.ticket("e", "a");
    s.ok(&["ticket", "label", "add", &t, "x", "y"]);
    // A dotted/summary field is selectable on list.
    let out = s.ok(&["ticket", "list", "e", "--raw", "--fields", "ref,status"]);
    assert!(out.contains("e/1"), "projection includes ref: {out}");
    assert!(out.contains("to-do"), "projection includes status: {out}");
}
