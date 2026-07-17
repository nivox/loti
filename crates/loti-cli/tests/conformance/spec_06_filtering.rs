//! Conformance: the filtering model for `list`.
//!
//! The normative rules exercised here:
//!   * scope depth is one rule at both scopes — the default is the full tree
//!     rooted at the scope, and `--shallow` keeps only the immediate level;
//!   * the four filter families combine with AND;
//!   * `--label` repeated is AND, a comma within one `--label` is an OR-group,
//!     `--not-label` is "has none of";
//!   * `--status` comma is OR and repeating it is an error; `--open`/`--resolved`
//!     aggregators conflict with each other and with `--status`;
//!   * the built-in `regex` matcher is the default and matches name + summary +
//!     body with zero external dependency;
//!   * an external matcher, configured as an argv template in `.loti.conf`,
//!     receives the candidate paths, returns a newline-separated subset in an
//!     order it chooses (preserved), and any printed path outside the set is
//!     ignored; a non-zero exit with empty stdout means zero matches;
//!   * an unknown `--match-impl` errors, listing the available implementations.

use super::harness::Store;

/// Seed an epic with three labelled nodes of varied state and body:
///   e/1 in-progress [backend]        body mentions "alpha"
///   e/2 to-do       [frontend,urgent] body mentions "beta"
///   e/3 done        [backend,urgent]  body mentions "gamma"
fn seed(s: &Store) {
    s.epic("e");
    s.ok_stdin(
        &["ticket", "create", "e", "--name", "One", "--summary", "s1"],
        "alpha body\n",
    );
    s.ok(&["ticket", "label", "add", "e/1", "backend"]);
    s.ok(&["ticket", "status", "e/1", "--in-progress"]);
    s.ok_stdin(
        &["ticket", "create", "e", "--name", "Two", "--summary", "s2"],
        "beta body\n",
    );
    s.ok(&["ticket", "label", "add", "e/2", "frontend", "urgent"]);
    s.ok_stdin(
        &[
            "ticket",
            "create",
            "e",
            "--name",
            "Three",
            "--summary",
            "s3",
        ],
        "gamma body\n",
    );
    s.ok(&["ticket", "label", "add", "e/3", "backend", "urgent"]);
    s.ok(&["ticket", "status", "e/3", "--done"]);
}

/// The `ref` column of a `--raw` list, as a set-ish sorted vector.
fn refs(raw: &str) -> Vec<String> {
    let mut v: Vec<String> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('\t').next().unwrap().to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn scope_defaults_to_full_tree_and_shallow_keeps_only_the_immediate_level() {
    // The scope depth rule is one rule at both scopes: the default is the full
    // tree rooted at the scope; `--shallow` keeps only the immediate level (an
    // epic's top-level nodes, or a node's direct children).
    let s = Store::new();
    s.epic("e");
    let top = s.ticket("e", "top"); // e/1
    let child = s.subticket("e", &top, "child"); // e/2 under e/1
    let _grand = s.subticket("e", &child, "grand"); // e/3 under e/2
    let _top2 = s.ticket("e", "top2"); // e/4

    // Epic scope, default: the whole tree.
    let all = s.ok(&["ticket", "list", "e", "--raw"]);
    assert_eq!(refs(&all), vec!["e/1", "e/2", "e/3", "e/4"], "got: {all}");

    // Epic scope, shallow: only the top-level nodes.
    let top_only = s.ok(&["ticket", "list", "e", "--shallow", "--raw"]);
    assert_eq!(refs(&top_only), vec!["e/1", "e/4"], "got: {top_only}");

    // Node scope, default: the whole subtree beneath the anchor (anchor itself
    // excluded).
    let subtree = s.ok(&["ticket", "list", "e/1", "--raw"]);
    assert_eq!(refs(&subtree), vec!["e/2", "e/3"], "got: {subtree}");

    // Node scope, shallow: only the anchor's direct children.
    let direct = s.ok(&["ticket", "list", "e/1", "--shallow", "--raw"]);
    assert_eq!(refs(&direct), vec!["e/2"], "got: {direct}");
}

#[test]
fn label_repeated_is_and_comma_is_or_group() {
    let s = Store::new();
    seed(&s);
    // `--label urgent --label backend,frontend` => urgent AND (backend OR frontend).
    // e/2 urgent+frontend and e/3 urgent+backend qualify; e/1 lacks urgent.
    let out = s.ok(&[
        "ticket",
        "list",
        "e",
        "--raw",
        "--label",
        "urgent",
        "--label",
        "backend,frontend",
    ]);
    assert_eq!(refs(&out), vec!["e/2", "e/3"], "got: {out}");
}

#[test]
fn not_label_excludes_any_of_the_named() {
    let s = Store::new();
    seed(&s);
    let out = s.ok(&["ticket", "list", "e", "--raw", "--not-label", "urgent"]);
    assert_eq!(refs(&out), vec!["e/1"], "only the non-urgent node: {out}");
}

#[test]
fn state_comma_is_or() {
    let s = Store::new();
    seed(&s);
    let out = s.ok(&["ticket", "list", "e", "--raw", "--status", "to-do,done"]);
    assert_eq!(refs(&out), vec!["e/2", "e/3"], "got: {out}");
}

#[test]
fn state_repeated_is_an_error() {
    let s = Store::new();
    seed(&s);
    let err = s.fail(&[
        "ticket", "list", "e", "--status", "to-do", "--status", "done",
    ]);
    assert!(
        !err.is_empty(),
        "repeating --status must be an error, got empty stderr"
    );
}

#[test]
fn open_aggregator_selects_non_terminal() {
    let s = Store::new();
    seed(&s);
    let out = s.ok(&["ticket", "list", "e", "--raw", "--open"]);
    assert_eq!(
        refs(&out),
        vec!["e/1", "e/2"],
        "open = to-do|in-progress|blocked: {out}"
    );
}

#[test]
fn resolved_aggregator_selects_terminal() {
    let s = Store::new();
    seed(&s);
    let out = s.ok(&["ticket", "list", "e", "--raw", "--resolved"]);
    assert_eq!(refs(&out), vec!["e/3"], "resolved = done|closed: {out}");
}

#[test]
fn open_and_state_conflict() {
    let s = Store::new();
    seed(&s);
    let err = s.fail(&["ticket", "list", "e", "--open", "--status", "done"]);
    assert!(
        !err.is_empty(),
        "--open with --status must conflict, got empty stderr"
    );
}

#[test]
fn open_and_resolved_conflict() {
    let s = Store::new();
    seed(&s);
    let err = s.fail(&["ticket", "list", "e", "--open", "--resolved"]);
    assert!(!err.is_empty(), "--open with --resolved must conflict");
}

#[test]
fn builtin_regex_matches_name_summary_and_body() {
    let s = Store::new();
    seed(&s);
    // "alpha" appears only in e/1's body — the built-in default reaches bodies.
    let by_body = s.ok(&["ticket", "list", "e", "--raw", "--match", "alpha"]);
    assert_eq!(refs(&by_body), vec!["e/1"], "body match: {by_body}");
    // A name match ("Three") also works with no --match-impl given (regex default).
    let by_name = s.ok(&["ticket", "list", "e", "--raw", "--match", "Three"]);
    assert_eq!(refs(&by_name), vec!["e/3"], "name match: {by_name}");
}

#[test]
fn structured_filters_and_match_combine_with_and() {
    let s = Store::new();
    seed(&s);
    // backend AND body-matches-"gamma" => only e/3 (e/1 is backend but body alpha).
    let out = s.ok(&[
        "ticket", "list", "e", "--raw", "--label", "backend", "--match", "gamma",
    ]);
    assert_eq!(refs(&out), vec!["e/3"], "AND across families: {out}");
}

#[test]
fn unknown_match_impl_errors_listing_available() {
    let s = Store::new();
    seed(&s);
    let err = s.fail(&[
        "ticket",
        "list",
        "e",
        "--match",
        "x",
        "--match-impl",
        "nope",
    ]);
    assert!(
        err.contains("nope") && err.contains("regex"),
        "unknown impl should list available (incl. regex), got: {err}"
    );
}

// -- external matcher protocol ---------------------------------------------

/// Write an executable POSIX shell matcher script into `dir` and return its
/// path. `body` is the script body after the shebang. The scripts implement the
/// protocol: read a query and candidate paths from argv, print a subset.
#[cfg(unix)]
fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

/// Configure an external matcher named `impl_name` in the store's `.loti.conf`
/// as an argv template invoking `script` with `<QUERY>` then `<CANDIDATES>`.
#[cfg(unix)]
fn configure_matcher(s: &Store, impl_name: &str, script: &std::path::Path) {
    let conf = s.path(".loti.conf");
    // The store root is itself the project; `.loti.conf` here configures it.
    let contents = format!(
        "loti-root = \"{root}\"\n[match-impl.{name}]\ncommand = [\"{script}\", \"<QUERY>\", \"<CANDIDATES>\"]\n",
        root = s.root().display(),
        name = impl_name,
        script = script.display(),
    );
    std::fs::write(conf, contents).unwrap();
}

#[cfg(unix)]
#[test]
fn external_matcher_narrows_to_the_subset_it_selects() {
    let s = Store::new();
    seed(&s);
    // A matcher that greps candidate files for the query and prints only the
    // matching paths — the tool keeps exactly that subset.
    let script = write_script(
        s.root(),
        "grep-match.sh",
        r#"q="$1"; shift
for f in "$@"; do
  if grep -q "$q" "$f"; then printf '%s\n' "$f"; fi
done
"#,
    );
    configure_matcher(&s, "grepmatch", &script);

    // "gamma" appears only in e/3's body: the external matcher narrows to it.
    let out = s.ok(&[
        "ticket",
        "list",
        "e",
        "--raw",
        "--match",
        "gamma",
        "--match-impl",
        "grepmatch",
    ]);
    assert_eq!(refs(&out), vec!["e/3"], "external matcher subset: {out}");

    // A query present in every body keeps the whole candidate set — the matcher
    // can only ever narrow, never invent members.
    let all = s.ok(&[
        "ticket",
        "list",
        "e",
        "--raw",
        "--match",
        "body",
        "--match-impl",
        "grepmatch",
    ]);
    assert_eq!(
        refs(&all),
        vec!["e/1", "e/2", "e/3"],
        "full subset kept: {all}"
    );
}

#[cfg(unix)]
#[test]
fn external_matcher_paths_outside_the_candidate_set_are_ignored() {
    let s = Store::new();
    seed(&s);
    // A matcher that echoes one real candidate plus a bogus path outside the set.
    let script = write_script(
        s.root(),
        "inject.sh",
        r#"shift
first="$1"
printf '%s\n/not/a/candidate/999.md\n' "$first"
"#,
    );
    configure_matcher(&s, "inject", &script);
    let out = s.ok(&[
        "ticket",
        "list",
        "e",
        "--raw",
        "--match",
        "anything",
        "--match-impl",
        "inject",
    ]);
    // Only the in-set path survives; the invented one is dropped.
    let got = refs(&out);
    assert_eq!(got.len(), 1, "out-of-set path dropped, one survivor: {out}");
    assert!(got[0].starts_with("e/"), "survivor is a real ref: {out}");
}

#[cfg(unix)]
#[test]
fn external_matcher_nonzero_empty_stdout_is_zero_matches() {
    let s = Store::new();
    seed(&s);
    // grep-style: exit 1 with no stdout means "no matches", not a failure.
    let script = write_script(s.root(), "nomatch.sh", "exit 1\n");
    configure_matcher(&s, "nomatch", &script);
    let out = s.ok(&[
        "ticket",
        "list",
        "e",
        "--raw",
        "--match",
        "x",
        "--match-impl",
        "nomatch",
    ]);
    assert!(
        out.trim().is_empty(),
        "non-zero exit with empty stdout => zero matches, got: {out:?}"
    );
}
