//! Conformance: effective agent profiles & workflows (`agent`/`workflow`
//! list/show).
//!
//! The normative rules exercised here:
//!   * local (config-relative `agent-root`/`workflow-root`) shadows global
//!     (XDG `loti/agents`/`loti/workflows`) by exact id, before either side is
//!     validated — an invalid local definition is never quietly replaced by a
//!     valid global one;
//!   * `list` rows carry only `id`, `origin`, and diagnostics — never a
//!     separate valid/invalid flag — and an invalid resource is listed, never
//!     dropped, in every format (plain/raw/json/ndjson);
//!   * ordering is bytewise lexical by id;
//!   * `agent show` renders the selected parsed profile (markdown default,
//!     `--json` canonical, `--raw`/`--field` projections), reporting warnings
//!     but succeeding when the recognized schema is valid;
//!   * `workflow show` writes the selected workflow's Markdown to stdout
//!     exactly as loaded — no wrapper, no trailing bytes added;
//!   * an unknown id and an invalid id both fail, with distinct diagnostics;
//!   * a configured local root that does not resolve is a hard error, not a
//!     silently empty local catalog.

use std::path::{Path, PathBuf};

use super::harness::Store;

/// Write a project config at the store root naming both resource roots
/// relative to it, and create the two local directories. The config sits
/// beside `meta` in `--root` mode, so `find_project_config` finds it on the
/// very first step of its upward walk.
fn local_roots(s: &Store) -> (PathBuf, PathBuf) {
    let agents = s.root().join("agents");
    let workflows = s.root().join("workflows");
    std::fs::create_dir_all(&agents).expect("create local agents dir");
    std::fs::create_dir_all(&workflows).expect("create local workflows dir");
    std::fs::write(
        s.root().join(".loti.conf"),
        "agent-root = \"agents\"\nworkflow-root = \"workflows\"\n",
    )
    .expect("write project config");
    (agents, workflows)
}

/// An isolated XDG config home carrying `loti/agents` and `loti/workflows`,
/// for checks that must isolate the user-global root per invocation rather
/// than the test process's own environment.
struct GlobalRoot {
    dir: tempfile::TempDir,
}

impl GlobalRoot {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("make tempdir");
        std::fs::create_dir_all(dir.path().join("loti").join("agents")).unwrap();
        std::fs::create_dir_all(dir.path().join("loti").join("workflows")).unwrap();
        Self { dir }
    }

    fn agents(&self) -> PathBuf {
        self.dir.path().join("loti").join("agents")
    }

    fn workflows(&self) -> PathBuf {
        self.dir.path().join("loti").join("workflows")
    }

    /// The `XDG_CONFIG_HOME` environment pair naming this root.
    fn env(&self) -> [(&str, &str); 1] {
        [("XDG_CONFIG_HOME", self.dir.path().to_str().unwrap())]
    }
}

fn write_profile(dir: &Path, id: &str, command: &str) {
    std::fs::write(
        dir.join(format!("{id}.toml")),
        format!("command = \"{command}\"\nargs = [\"{{{{ loti_prompt }}}}\"]\n"),
    )
    .expect("write profile");
}

fn write_invalid_profile(dir: &Path, id: &str) {
    // Missing the required `args` field.
    std::fs::write(dir.join(format!("{id}.toml")), "command = \"pi\"\n").expect("write profile");
}

fn write_workflow(dir: &Path, id: &str, text: &str) {
    std::fs::write(dir.join(format!("{id}.md")), text).expect("write workflow");
}

fn write_invalid_workflow(dir: &Path) {
    std::fs::write(dir.join("bad.md"), [0x66, 0x6f, 0xff, 0x6f]).expect("write workflow");
}

fn rows(json: &str) -> Vec<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(json)
        .expect("valid JSON")
        .as_array()
        .expect("flat array")
        .clone()
}

fn row<'a>(list: &'a [serde_json::Value], id: &str) -> &'a serde_json::Value {
    list.iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("row {id} missing from {list:?}"))
}

// ---------------------------------------------------------------------------
// agent / workflow list: origin, precedence, diagnostics, ordering
// ---------------------------------------------------------------------------

#[test]
fn local_shadows_global_by_exact_id_and_origins_are_reported() {
    let s = Store::new();
    let (agents, _workflows) = local_roots(&s);
    write_profile(&agents, "shared", "local-cmd");
    write_profile(&agents, "local-only", "pi");

    let global = GlobalRoot::new();
    write_profile(&global.agents(), "shared", "global-cmd");
    write_profile(&global.agents(), "global-only", "pi");

    let out = s.ok_env(&["agent", "list", "--json"], &global.env());
    let list = rows(&out);
    assert_eq!(list.len(), 3, "shared, local-only, global-only: {list:?}");
    assert_eq!(row(&list, "shared")["origin"], "local");
    assert_eq!(row(&list, "local-only")["origin"], "local");
    assert_eq!(row(&list, "global-only")["origin"], "global");
}

#[test]
fn invalid_local_shadows_a_valid_global_definition_without_falling_back() {
    let s = Store::new();
    let (agents, _workflows) = local_roots(&s);
    write_invalid_profile(&agents, "shared");

    let global = GlobalRoot::new();
    write_profile(&global.agents(), "shared", "global-cmd");

    let out = s.ok_env(&["agent", "list", "--json"], &global.env());
    let list = rows(&out);
    assert_eq!(
        list.len(),
        1,
        "the global definition is entirely gone: {list:?}"
    );
    let r = row(&list, "shared");
    assert_eq!(r["origin"], "local");
    assert!(
        r["diagnostics"][0].as_str().unwrap().contains("args"),
        "the local profile's own defect is reported: {r:?}"
    );

    // Confirmed directly too: `agent show` sees the same invalid local
    // definition, never the valid global one it shadows.
    let err = s.fail_env(&["agent", "show", "shared"], &global.env());
    assert!(err.contains("args"), "got: {err}");
}

#[test]
fn list_rows_carry_no_valid_flag_and_severity_lives_in_the_diagnostic_text() {
    let s = Store::new();
    let (agents, _workflows) = local_roots(&s);
    write_profile(&agents, "clean", "pi");
    write_invalid_profile(&agents, "broken");
    // A recognized-but-unrecognized-field profile: a tolerated warning, not an
    // error, distinguishable only in the diagnostic's own prose.
    std::fs::write(
        agents.join("warned.toml"),
        "command = \"pi\"\nargs = [\"a\"]\nnickname = \"x\"\n",
    )
    .unwrap();

    let out = s.ok(&["agent", "list", "--json"]);
    let list = rows(&out);

    let clean = row(&list, "clean");
    let clean_obj = clean.as_object().unwrap();
    let mut keys: Vec<&str> = clean_obj.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["diagnostics", "id", "origin"],
        "no separate valid/invalid field: {clean:?}"
    );
    assert_eq!(clean["diagnostics"], serde_json::json!([]));

    let broken = row(&list, "broken");
    assert!(broken["diagnostics"][0]
        .as_str()
        .unwrap()
        .starts_with("error:"));

    let warned = row(&list, "warned");
    assert!(warned["diagnostics"][0]
        .as_str()
        .unwrap()
        .starts_with("warning:"));
}

#[test]
fn every_list_format_reports_an_invalid_resource_with_origin_and_diagnostics() {
    let s = Store::new();
    let (agents, _workflows) = local_roots(&s);
    write_invalid_profile(&agents, "broken");

    // Plain text.
    let plain = s.ok(&["agent", "list"]);
    assert!(plain.contains("broken (local)"), "got: {plain}");
    assert!(plain.contains("args"), "got: {plain}");

    // Tab-separated raw.
    let raw = s.ok(&["agent", "list", "--raw"]);
    let line = raw.lines().find(|l| l.starts_with("broken\t")).unwrap();
    assert!(line.contains("local"), "got: {line}");
    assert!(line.contains("args"), "got: {line}");

    // NDJSON.
    let ndjson = s.ok(&["agent", "list", "--ndjson"]);
    let obj: serde_json::Value = ndjson
        .lines()
        .find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            (v["id"] == "broken").then_some(v)
        })
        .unwrap();
    assert_eq!(obj["origin"], "local");
    assert!(obj["diagnostics"][0].as_str().unwrap().contains("args"));

    // JSON (already exercised above, checked again for the invalid row).
    let json = s.ok(&["agent", "list", "--json"]);
    let listed = rows(&json);
    let r = row(&listed, "broken");
    assert_eq!(r["origin"], "local");
}

#[test]
fn list_ordering_is_bytewise_lexical_by_id() {
    let s = Store::new();
    let (agents, _workflows) = local_roots(&s);
    // Bytewise ASCII order places every uppercase letter before every
    // lowercase one; a case-insensitive sort would not.
    write_profile(&agents, "b", "pi");
    write_profile(&agents, "A", "pi");
    write_profile(&agents, "a", "pi");

    let out = s.ok(&["agent", "list", "--json"]);
    let listed = rows(&out);
    let ids: Vec<&str> = listed.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["A", "a", "b"]);
}

#[test]
fn workflow_list_shows_the_same_row_shape_and_precedence_as_agents() {
    let s = Store::new();
    let (_agents, workflows) = local_roots(&s);
    write_workflow(&workflows, "shared", "local text");
    write_invalid_workflow(&workflows);

    let global = GlobalRoot::new();
    write_workflow(&global.workflows(), "shared", "global text");
    write_workflow(&global.workflows(), "global-only", "g");

    let out = s.ok_env(&["workflow", "list", "--json"], &global.env());
    let list = rows(&out);
    assert_eq!(list.len(), 3, "shared, bad, global-only: {list:?}");
    assert_eq!(row(&list, "shared")["origin"], "local");
    assert_eq!(row(&list, "global-only")["origin"], "global");
    let bad = row(&list, "bad");
    assert_eq!(bad["origin"], "local");
    assert!(bad["diagnostics"][0].as_str().unwrap().contains("UTF-8"));
}

#[test]
fn no_project_config_means_no_local_resources_not_an_error() {
    let s = Store::new();
    let global = GlobalRoot::new();
    write_profile(&global.agents(), "only-global", "pi");

    let out = s.ok_env(&["agent", "list", "--json"], &global.env());
    let list = rows(&out);
    assert_eq!(list.len(), 1);
    assert_eq!(row(&list, "only-global")["origin"], "global");
}

#[test]
fn a_configured_local_root_that_does_not_resolve_is_a_hard_error() {
    let s = Store::new();
    std::fs::write(
        s.root().join(".loti.conf"),
        "agent-root = \"does-not-exist\"\n",
    )
    .unwrap();

    let err = s.fail(&["agent", "list"]);
    assert!(
        err.contains("agent-root") || err.contains("does not resolve"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------------
// agent list --field / heavy vs listable
// ---------------------------------------------------------------------------

#[test]
fn agent_list_serves_only_its_own_listable_fields() {
    let s = Store::new();
    let (agents, _workflows) = local_roots(&s);
    write_profile(&agents, "solo", "pi");

    // `origin` is listable.
    let out = s.ok(&["agent", "list", "--field", "origin"]);
    assert_eq!(out.trim(), "local");

    // `command` is a real profile field, but it is show-only here.
    let err = s.fail(&["agent", "list", "--field", "command"]);
    assert!(err.contains("command"), "got: {err}");
}

// ---------------------------------------------------------------------------
// agent show
// ---------------------------------------------------------------------------

#[test]
fn agent_show_markdown_default_lists_command_args_env_and_diagnostics() {
    let s = Store::new();
    let (agents, _workflows) = local_roots(&s);
    std::fs::write(
        agents.join("full.toml"),
        "command = \"pi\"\nargs = [\"{{ loti_prompt }}\"]\ncwd = \"/tmp\"\n\
         nickname = \"ignored\"\n[env]\nFOO = \"bar\"\n",
    )
    .unwrap();

    let md = s.ok(&["agent", "show", "full"]);
    let idx = |needle: &str| {
        md.find(needle)
            .unwrap_or_else(|| panic!("missing {needle} in:\n{md}"))
    };
    let meta = idx("| field | value |");
    let args = idx("## Args");
    let env = idx("## Env");
    let diagnostics = idx("## Diagnostics");
    assert!(meta < args && args < env && env < diagnostics);
    assert!(md.contains("| command | pi |"));
    assert!(md.contains("| FOO | bar |"));
    assert!(
        md.contains("warning") && md.contains("nickname"),
        "the tolerated unknown field is surfaced as a warning: {md}"
    );
}

#[test]
fn agent_show_json_is_canonical_and_field_projections_work() {
    let s = Store::new();
    let (agents, _workflows) = local_roots(&s);
    write_profile(&agents, "solo", "pi");

    let json = s.ok(&["agent", "show", "solo", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["id"], "solo");
    assert_eq!(v["origin"], "local");
    assert_eq!(v["command"], "pi");
    assert_eq!(v["diagnostics"], serde_json::json!([]));

    let raw = s.ok(&["agent", "show", "solo", "--raw", "--field", "command"]);
    assert_eq!(raw.trim(), "pi");
}

#[test]
fn agent_show_reports_the_stored_diagnostic_for_an_invalid_profile() {
    let s = Store::new();
    let (agents, _workflows) = local_roots(&s);
    write_invalid_profile(&agents, "broken");

    let err = s.fail(&["agent", "show", "broken"]);
    assert!(err.contains("broken"), "got: {err}");
    assert!(err.contains("args"), "got: {err}");
}

#[test]
fn agent_show_unknown_id_reports_not_found_distinctly_from_invalid() {
    let s = Store::new();
    local_roots(&s);

    let err = s.fail(&["agent", "show", "ghost"]);
    assert!(err.contains("ghost"), "got: {err}");
    assert!(
        err.contains("does not exist") || err.contains("not found"),
        "got: {err}"
    );
    // Distinct wording from the invalid-resource case: never both true.
    assert!(!err.contains("is invalid"), "got: {err}");
}

// ---------------------------------------------------------------------------
// workflow show
// ---------------------------------------------------------------------------

#[test]
fn workflow_show_writes_markdown_verbatim_byte_for_byte() {
    let s = Store::new();
    let (_agents, workflows) = local_roots(&s);
    // Leading blank line, trailing spaces, no final newline: anything a
    // "helpful" normalization pass might be tempted to touch.
    let source = "\n# Title  \n\nBody text.\nNo trailing newline";
    write_workflow(&workflows, "verbatim", source);

    let out = s.cmd(&["workflow", "show", "verbatim"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        out.stdout,
        source.as_bytes(),
        "workflow show must add no wrapper and no trailing bytes"
    );
}

#[test]
fn workflow_show_reports_invalid_utf8_and_unknown_id_distinctly() {
    let s = Store::new();
    let (_agents, workflows) = local_roots(&s);
    write_invalid_workflow(&workflows);

    let err = s.fail(&["workflow", "show", "bad"]);
    assert!(err.contains("UTF-8"), "got: {err}");

    let err = s.fail(&["workflow", "show", "ghost"]);
    assert!(
        err.contains("does not exist") || err.contains("not found"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------------
// cooperative agent-session visibility
// ---------------------------------------------------------------------------

#[test]
fn every_session_marker_hides_agent_commands_before_resource_lookup() {
    let s = Store::new();
    // A broken root would normally fail an agent command. Session visibility
    // must win before any profile-root lookup, for every marker arrangement.
    std::fs::write(
        s.root().join(".loti.conf"),
        "agent-root = \"does-not-exist\"\n",
    )
    .unwrap();
    let marker_sets: &[&[(&str, &str)]] = &[
        &[("LOTI_AGENT_SESSION", "target")],
        &[("LOTI_AGENT_SESSION", "")],
        &[("LOTI_AGENT_WORKFLOW", "review")],
        &[("LOTI_AGENT_WORKFLOW", "")],
        &[
            ("LOTI_AGENT_SESSION", ""),
            ("LOTI_AGENT_WORKFLOW", "review"),
        ],
    ];

    for markers in marker_sets {
        for command in [&["agent", "list"][..], &["agent", "show", "profile"][..]] {
            let err = s.fail_env(command, markers);
            assert!(
                err.contains("agent commands are unavailable"),
                "marker set {markers:?}, command {command:?}: {err}"
            );
            assert!(
                !err.contains("agent-root"),
                "agent resources were consulted for {markers:?}, {command:?}: {err}"
            );
        }
    }
}

#[test]
fn session_marker_without_workflow_marker_keeps_ordinary_workflow_access() {
    let s = Store::new();
    let (_agents, workflows) = local_roots(&s);
    write_workflow(&workflows, "one", "first workflow");
    write_workflow(&workflows, "two", "second workflow");
    let session_only = [("LOTI_AGENT_SESSION", "target")];

    let list = rows(&s.ok_env(&["workflow", "list", "--json"], &session_only));
    let ids: Vec<&str> = list.iter().map(|row| row["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["one", "two"]);
    assert_eq!(
        s.ok_env(&["workflow", "show", "two"], &session_only),
        "second workflow"
    );
}

#[test]
fn workflow_marker_filters_the_effective_roster_to_its_exact_id() {
    let s = Store::new();
    let (_agents, workflows) = local_roots(&s);
    write_workflow(&workflows, "selected", "selected workflow");
    write_workflow(&workflows, "other", "other workflow");

    let list = rows(&s.ok_env(
        &["workflow", "list", "--json"],
        &[("LOTI_AGENT_WORKFLOW", "selected")],
    ));
    let ids: Vec<&str> = list.iter().map(|row| row["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["selected"]);
}

#[test]
fn selected_workflow_show_remains_verbatim_accessible() {
    let s = Store::new();
    let (_agents, workflows) = local_roots(&s);
    let source = "# Selected  \nNo trailing newline";
    write_workflow(&workflows, "selected", source);

    let out = s
        .cmd_env(
            &["workflow", "show", "selected"],
            &[("LOTI_AGENT_WORKFLOW", "selected")],
        )
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, source.as_bytes());
}

#[test]
fn hidden_and_missing_workflow_show_failures_are_byte_identical() {
    let with_hidden = Store::new();
    let (_agents, workflows) = local_roots(&with_hidden);
    write_workflow(&workflows, "selected", "selected workflow");
    write_workflow(&workflows, "other", "hidden workflow");

    let without_hidden = Store::new();
    let (_agents, workflows) = local_roots(&without_hidden);
    write_workflow(&workflows, "selected", "selected workflow");

    let marker = [("LOTI_AGENT_WORKFLOW", "selected")];
    let hidden = with_hidden.fail_env(&["workflow", "show", "other"], &marker);
    let missing = without_hidden.fail_env(&["workflow", "show", "other"], &marker);
    assert_eq!(hidden, missing);
    assert!(hidden.contains("workflow 'other' does not exist"));
}

#[test]
fn absent_selected_workflow_lists_successfully_as_an_empty_roster() {
    let s = Store::new();
    let (_agents, workflows) = local_roots(&s);
    write_workflow(&workflows, "present", "present workflow");

    assert_eq!(
        rows(&s.ok_env(
            &["workflow", "list", "--json"],
            &[("LOTI_AGENT_WORKFLOW", "missing")],
        )),
        Vec::<serde_json::Value>::new()
    );
}

#[test]
fn invalid_selected_workflow_remains_listed_with_its_diagnostic() {
    let s = Store::new();
    let (_agents, workflows) = local_roots(&s);
    write_invalid_workflow(&workflows);
    write_workflow(&workflows, "other", "other workflow");

    let list = rows(&s.ok_env(
        &["workflow", "list", "--json"],
        &[("LOTI_AGENT_WORKFLOW", "bad")],
    ));
    let selected = row(&list, "bad");
    assert_eq!(selected["origin"], "local");
    assert!(selected["diagnostics"][0]
        .as_str()
        .unwrap()
        .contains("UTF-8"));
}

#[test]
fn skill_stays_available_and_static_in_a_cooperative_session() {
    let s = Store::new();
    let ordinary = s.ok(&["skill"]);
    let session = s.ok_env(
        &["skill"],
        &[
            ("LOTI_AGENT_SESSION", "target"),
            ("LOTI_AGENT_WORKFLOW", "selected"),
        ],
    );
    assert_eq!(session, ordinary);
}

// ---------------------------------------------------------------------------
// grammar: id validation happens before dispatch touches the store
// ---------------------------------------------------------------------------

#[test]
fn a_malformed_id_argument_is_rejected_without_a_store_lookup() {
    // No store is even initialised at this path — if the id ever reached
    // dispatch it would fail on discovery, not on the id's shape. Getting the
    // shape error confirms the parser rejected it first.
    let dir = tempfile::tempdir().unwrap();
    let out = assert_cmd::Command::cargo_bin("loti")
        .unwrap()
        .current_dir(dir.path())
        .args(["agent", "show", "bad id"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("invalid value") || err.contains("ASCII"),
        "got: {err}"
    );
}
