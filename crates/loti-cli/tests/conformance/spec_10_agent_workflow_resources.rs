//! Conformance: effective agent profiles, workflows, and foreground launch
//! (`agent`/`workflow` list/show plus `agent run`).
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
//!     silently empty local catalog;
//!   * `agent run` requires an explicit profile and workflow before store
//!     lookup, accepts either an epic or ticket target, and refuses a
//!     cooperative session, non-terminal streams, invalid selections, or an
//!     invalid launch plan without mutating the tracker;
//!   * on Unix a successful launch replaces the CLI with the prepared direct
//!     child, preserving its terminal streams, planned argv/cwd/environment,
//!     and exit status.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Output};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::harness::{run_bare, Store};

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

fn write_launch_profile(dir: &Path, id: &str, command: &Path, args: &str) {
    std::fs::write(
        dir.join(format!("{id}.toml")),
        format!(
            "command = {}\nargs = {args}\ncwd = \"{{{{ project_root }}}}\"\n\
             [env]\nRENDERED = \"rendered {{{{ loti_ref_name }}}}\"\n",
            toml_string(command.to_str().expect("UTF-8 fixture path")),
        ),
    )
    .expect("write launch profile");
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('\"', "\\\""))
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
// generated help: agent/workflow grammar and resource-list row shape
// ---------------------------------------------------------------------------

#[test]
fn generated_help_names_agent_workflow_groups_and_resource_list_rows() {
    let root = run_bare(&["--help"]);
    assert!(
        root.status.success(),
        "root help failed: {}",
        String::from_utf8_lossy(&root.stderr)
    );
    let root = String::from_utf8_lossy(&root.stdout);
    assert!(
        root.contains("loti <epic|ticket|agent|workflow> <verb> ..."),
        "root grammar omits resource groups:\n{root}"
    );
    assert!(
        root.contains("`agent` manages effective agent profiles and foreground launches"),
        "root description does not identify the agent group:\n{root}"
    );
    assert!(
        root.contains("`workflow` manages effective workflow instructions"),
        "root description does not identify the workflow group:\n{root}"
    );

    for args in [
        &["agent", "list", "--help"][..],
        &["workflow", "list", "--help"][..],
    ] {
        let output = run_bare(args);
        assert!(
            output.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let help = String::from_utf8_lossy(&output.stdout);
        assert!(
            help.contains("Flat JSON array of resource rows: `id`, `origin`, and `diagnostics`"),
            "{args:?} must describe its own row shape:\n{help}"
        );
        assert!(
            !help.contains("parent"),
            "{args:?} must not claim resource rows carry parent pointers:\n{help}"
        );
    }
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

// ---------------------------------------------------------------------------
// foreground agent launch
// ---------------------------------------------------------------------------

#[test]
fn agent_run_requires_both_resource_selections_before_store_lookup() {
    let dir = tempfile::tempdir().unwrap();
    for (args, missing) in [
        (
            ["agent", "run", "epic", "--agent", "profile"].as_slice(),
            "--workflow",
        ),
        (
            ["agent", "run", "epic", "--workflow", "review"].as_slice(),
            "--agent",
        ),
    ] {
        let out = assert_cmd::Command::cargo_bin("loti")
            .unwrap()
            .current_dir(dir.path())
            .args(args)
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "arguments {args:?} unexpectedly passed"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(missing),
            "missing selection {missing} was not diagnosed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(unix)]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Run the shipping binary in a pseudo-terminal. The launch preflight requires
/// real terminal streams, so this wrapper is the normal subprocess boundary,
/// not a bypass of terminal detection.
#[cfg(unix)]
fn pty_run(s: &Store, args: &[&str], env: &[(&str, &str)]) -> Output {
    let loti = assert_cmd::cargo::cargo_bin("loti");
    let mut words = vec![loti.to_string_lossy().into_owned(), "--root".to_string()];
    words.push(s.root().display().to_string());
    words.extend(args.iter().map(|arg| (*arg).to_string()));
    let direct_command = words
        .iter()
        .map(|word| shell_quote(word))
        .collect::<Vec<_>>()
        .join(" ");
    // The shell's PID becomes loti's PID. The fixture records it alongside its
    // own, so equality proves loti replaced itself rather than wrapping a child.
    let command = format!("export LOTI_TEST_EXPECTED_PID=$$; exec {direct_command}");
    let mut script = Command::new("script");
    script.args(["-qefc", &command, "/dev/null"]);
    script.env("NO_COLOR", "1");
    for (key, value) in env {
        script.env(key, value);
    }
    script.output().expect("run loti in a pseudo-terminal")
}

#[cfg(unix)]
fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(unix)]
fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).expect("read fixture directory") {
            let entry = entry.expect("read fixture entry");
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, snapshot);
            } else {
                snapshot.insert(
                    path.strip_prefix(root)
                        .expect("fixture-relative path")
                        .to_path_buf(),
                    std::fs::read(&path).expect("read fixture file"),
                );
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

#[cfg(unix)]
fn launch_fixture(dir: &Path) -> PathBuf {
    let path = dir.join("agent-fixture");
    std::fs::write(
        &path,
        "#!/bin/sh\n\
printf '%s\\0' \"$#\" \"$PWD\" \"$@\" \"$INHERITED\" \"$RENDERED\" \\\n\"$LOTI_AGENT_SESSION\" \"$LOTI_AGENT_WORKFLOW\" > \"$LOTI_TEST_RECORD\"\n\
for fd in 0 1 2; do\n\
  if [ -t \"$fd\" ]; then printf '%s\\0' yes >> \"$LOTI_TEST_RECORD\"; else printf '%s\\0' no >> \"$LOTI_TEST_RECORD\"; fi\n\
done\n\
printf '%s\\0' \"$$\" \"$LOTI_TEST_EXPECTED_PID\" >> \"$LOTI_TEST_RECORD\"\n\
exit \"${LOTI_TEST_EXIT:-0}\"\n",
    )
    .expect("write fixture executable");
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("make fixture executable");
    path
}

#[cfg(unix)]
fn nul_fields(path: &Path) -> Vec<String> {
    let mut fields = std::fs::read(path)
        .expect("read fixture record")
        .split(|byte| *byte == 0)
        .map(|field| String::from_utf8(field.to_vec()).expect("UTF-8 fixture field"))
        .collect::<Vec<_>>();
    assert_eq!(
        fields.pop().as_deref(),
        Some(""),
        "fixture record terminator"
    );
    fields
}

#[cfg(unix)]
#[test]
fn agent_run_replaces_itself_with_the_direct_ticket_child_and_preserves_its_payload() {
    let s = Store::new();
    s.epic("epic");
    let ticket = s.ticket("epic", "ticket target");
    let (agents, workflows) = local_roots(&s);
    let fixture_dir = tempfile::tempdir().unwrap();
    let fixture = launch_fixture(fixture_dir.path());
    write_launch_profile(
        &agents,
        "profile",
        &fixture,
        "[\"--fixed\", \"{{ loti_prompt }}\", \"{{ loti_ref }}\"]",
    );
    write_workflow(&workflows, "review", "follow this workflow");
    let record = fixture_dir.path().join("record");

    let output = pty_run(
        &s,
        &[
            "agent",
            "run",
            &ticket,
            "--agent",
            "profile",
            "--workflow",
            "review",
        ],
        &[
            ("INHERITED", "kept"),
            ("LOTI_TEST_RECORD", record.to_str().unwrap()),
            ("LOTI_TEST_EXIT", "23"),
        ],
    );
    assert_eq!(output.status.code(), Some(23), "{}", output_text(&output));

    let fields = nul_fields(&record);
    assert_eq!(fields[0], "3", "fixture record: {fields:?}");
    assert_eq!(fields[1], s.root().display().to_string());
    assert_eq!(fields[2], "--fixed");
    assert!(
        fields[3].contains(&format!("ticket \"{ticket}\" (ticket target)")),
        "bootstrap was not passed as one direct argument: {fields:?}"
    );
    assert_eq!(fields[4], ticket);
    assert_eq!(fields[5], "kept");
    assert_eq!(fields[6], "rendered ticket target");
    assert_eq!(fields[7], "epic/1");
    assert_eq!(fields[8], "review");
    assert_eq!(&fields[9..12], ["yes", "yes", "yes"]);
    assert_eq!(fields[12], fields[13], "loti did not replace itself");
}

#[cfg(unix)]
#[test]
fn agent_run_resolves_an_epic_target_for_the_shared_bootstrap() {
    let s = Store::new();
    s.epic("epic");
    let (agents, workflows) = local_roots(&s);
    let fixture_dir = tempfile::tempdir().unwrap();
    let fixture = launch_fixture(fixture_dir.path());
    write_launch_profile(&agents, "profile", &fixture, "[\"{{ loti_prompt }}\"]");
    write_workflow(&workflows, "review", "follow this workflow");
    let record = fixture_dir.path().join("record");

    let output = pty_run(
        &s,
        &[
            "agent",
            "run",
            "epic",
            "--agent",
            "profile",
            "--workflow",
            "review",
        ],
        &[("LOTI_TEST_RECORD", record.to_str().unwrap())],
    );
    assert!(output.status.success(), "{}", output_text(&output));
    let fields = nul_fields(&record);
    assert!(
        fields[2].contains("epic \"epic\" (epic)"),
        "bootstrap did not identify the epic target: {fields:?}"
    );
    assert_eq!(fields[5], "epic");
}

#[cfg(unix)]
#[test]
fn every_agent_run_preflight_refusal_leaves_the_store_byte_for_byte_unchanged() {
    let s = Store::new();
    s.epic("epic");
    let (agents, workflows) = local_roots(&s);
    let fixture_dir = tempfile::tempdir().unwrap();
    let fixture = launch_fixture(fixture_dir.path());
    write_launch_profile(&agents, "profile", &fixture, "[\"{{ loti_prompt }}\"]");
    write_launch_profile(&agents, "bad-plan", &fixture, "[\"--missing-prompt\"]");
    write_invalid_profile(&agents, "invalid-profile");
    write_workflow(&workflows, "review", "follow this workflow");
    write_invalid_workflow(&workflows);

    let before = snapshot_tree(s.root());
    let terminal = s
        .cmd(&[
            "agent",
            "run",
            "epic",
            "--agent",
            "profile",
            "--workflow",
            "review",
        ])
        .output()
        .unwrap();
    assert!(!terminal.status.success());
    assert!(String::from_utf8_lossy(&terminal.stderr).contains("stdin, stdout, and stderr"));
    assert_eq!(
        snapshot_tree(s.root()),
        before,
        "terminal refusal wrote to the store"
    );

    type Refusal<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], &'a str);
    let refusals: &[Refusal<'_>] = &[
        (
            &[
                "agent",
                "run",
                "epic",
                "--agent",
                "profile",
                "--workflow",
                "review",
            ],
            &[("LOTI_AGENT_SESSION", "epic")],
            "cooperative agent session",
        ),
        (
            &[
                "agent",
                "run",
                "missing",
                "--agent",
                "profile",
                "--workflow",
                "review",
            ],
            &[],
            "does not exist",
        ),
        (
            &[
                "agent",
                "run",
                "epic",
                "--agent",
                "missing",
                "--workflow",
                "review",
            ],
            &[],
            "agent profile 'missing' does not exist",
        ),
        (
            &[
                "agent",
                "run",
                "epic",
                "--agent",
                "invalid-profile",
                "--workflow",
                "review",
            ],
            &[],
            "invalid",
        ),
        (
            &[
                "agent",
                "run",
                "epic",
                "--agent",
                "profile",
                "--workflow",
                "missing",
            ],
            &[],
            "workflow 'missing' does not exist",
        ),
        (
            &[
                "agent",
                "run",
                "epic",
                "--agent",
                "profile",
                "--workflow",
                "bad",
            ],
            &[],
            "workflow 'bad' is invalid",
        ),
        (
            &[
                "agent",
                "run",
                "epic",
                "--agent",
                "bad-plan",
                "--workflow",
                "review",
            ],
            &[],
            "loti_prompt",
        ),
    ];
    for (args, env, expected) in refusals {
        let output = pty_run(&s, args, env);
        assert!(!output.status.success(), "{args:?} unexpectedly launched");
        assert!(
            output_text(&output).contains(expected),
            "{args:?} did not report {expected:?}: {}",
            output_text(&output)
        );
        assert_eq!(
            snapshot_tree(s.root()),
            before,
            "{args:?} changed the store during preflight"
        );
    }
}
