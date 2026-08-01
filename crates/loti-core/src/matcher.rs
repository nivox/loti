//! External match implementations and the protocol that drives them.
//!
//! The match family of the roster filter can be served by an external command
//! instead of the built-in regex. An external matcher is configured as an
//! argv-array command template — a list of arguments, never a shell string, so
//! there is no quoting or word-splitting surprise. Two placeholders are
//! expanded when the command is built:
//!
//!   * `<QUERY>` is replaced, in place, by exactly one argument: the query.
//!   * `<CANDIDATES>` is replaced by N arguments: one per candidate file path.
//!
//! The protocol: the caller resolves scope and the structured filters down to a
//! set of candidate files (the whole node files, frontmatter included), and
//! hands their paths to the matcher. The matcher prints, on stdout, a
//! newline-separated subset of those exact paths — the ones that match — and
//! their order is significant and preserved. Any printed line that is not one of
//! the candidate paths, or is otherwise unparseable, is ignored with a warning
//! rather than trusted. Exit handling follows the grep convention: a non-zero
//! exit with empty stdout means "no matches" (not a failure), while a non-zero
//! exit that wrote to stderr is a real failure and the stderr is surfaced.
//!
//! Configuration is layered: a user-global file under the XDG config directory
//! is merged with the project config, and the project wins on a name collision.
//! The name of the built-in matcher is reserved and can never be redefined by
//! configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::filter::BUILTIN_MATCHER_NAME;

#[cfg(test)]
pub(crate) static XDG_CONFIG_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The `<QUERY>` placeholder, replaced by exactly one argument.
const QUERY_PLACEHOLDER: &str = "<QUERY>";

/// The `<CANDIDATES>` placeholder, replaced by one argument per candidate path.
const CANDIDATES_PLACEHOLDER: &str = "<CANDIDATES>";

/// One configured external matcher: an argv-array command template.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MatcherConfig {
    /// The command template as an argv array. Must contain the program as its
    /// first element; placeholders may appear anywhere among the arguments.
    pub command: Vec<String>,
}

/// The set of configured external matchers, keyed by name. Built by layering a
/// user-global file under the project config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatcherRegistry {
    /// Configured matchers by name. The built-in is not stored here — its name
    /// is reserved and always resolves to the built-in behaviour.
    impls: BTreeMap<String, MatcherConfig>,
}

/// Why a matcher could not be resolved or run.
#[derive(Debug, thiserror::Error)]
pub enum MatcherError {
    /// The requested `--match-impl` name is neither the built-in nor a
    /// configured external matcher. The message lists what is available.
    #[error("unknown match implementation '{requested}'; available: {available}")]
    Unknown {
        /// The name that was asked for.
        requested: String,
        /// A comma-separated list of the names that do exist.
        available: String,
    },
    /// A configured command template was empty, so there is no program to run.
    #[error("match implementation '{0}' has an empty command template")]
    EmptyCommand(String),
    /// The external matcher process could not be spawned or waited on.
    #[error("could not run match implementation '{name}': {source}")]
    Spawn {
        /// The matcher name.
        name: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The external matcher exited non-zero and wrote to stderr, so it is a real
    /// failure rather than a grep-style "no matches".
    #[error("match implementation '{name}' failed: {stderr}")]
    Failed {
        /// The matcher name.
        name: String,
        /// The captured stderr, surfaced to the user.
        stderr: String,
    },
}

/// The TOML shape both the user-global and project config parse into. Only the
/// matcher tables are read here; other keys (e.g. the project root pointer) are
/// ignored so the same project file can carry both.
#[derive(Debug, Clone, Default, Deserialize)]
struct ConfigDoc {
    #[serde(rename = "match-impl", default)]
    match_impl: BTreeMap<String, MatcherConfig>,
}

impl MatcherRegistry {
    /// Build a registry by layering a user-global config under a project config.
    /// The project wins on a name collision — a project may override a
    /// user-global matcher of the same name. A missing or unreadable file
    /// contributes nothing rather than failing, so an absent user config is fine.
    ///
    /// The reserved built-in name is stripped from configured matchers: config
    /// can never redefine it, so a stray `[match-impl.regex]` table is dropped
    /// and the built-in behaviour stands.
    pub fn layered(user_global: Option<&Path>, project: Option<&Path>) -> Self {
        let mut impls = BTreeMap::new();
        // User-global first, then project overlays it, so project wins.
        if let Some(path) = user_global {
            for (name, cfg) in read_match_impls(path) {
                impls.insert(name, cfg);
            }
        }
        if let Some(path) = project {
            for (name, cfg) in read_match_impls(path) {
                impls.insert(name, cfg);
            }
        }
        // The built-in name is reserved: configuration never redefines it.
        impls.remove(BUILTIN_MATCHER_NAME);
        Self { impls }
    }

    /// Build a registry directly from a name→config map, for tests and callers
    /// that assemble the layering themselves.
    pub fn from_map(impls: BTreeMap<String, MatcherConfig>) -> Self {
        let mut impls = impls;
        impls.remove(BUILTIN_MATCHER_NAME);
        Self { impls }
    }

    /// The configured external matcher of this name, if any.
    pub fn get(&self, name: &str) -> Option<&MatcherConfig> {
        self.impls.get(name)
    }

    /// All names a `--match-impl` may take, the reserved built-in first, then
    /// the configured external matchers in name order. Used for the error that
    /// lists what is available.
    pub fn available_names(&self) -> Vec<String> {
        let mut names = vec![BUILTIN_MATCHER_NAME.to_string()];
        names.extend(self.impls.keys().cloned());
        names
    }

    /// Resolve a requested `--match-impl` name to how it should be run. The
    /// reserved built-in name resolves to the built-in; any other name must be a
    /// configured external matcher or it is an error listing the alternatives.
    pub fn resolve(&self, requested: &str) -> Result<ResolvedMatcher<'_>, MatcherError> {
        if requested == BUILTIN_MATCHER_NAME {
            return Ok(ResolvedMatcher::Builtin);
        }
        match self.impls.get(requested) {
            Some(cfg) => Ok(ResolvedMatcher::External {
                name: requested.to_string(),
                config: cfg,
            }),
            None => Err(MatcherError::Unknown {
                requested: requested.to_string(),
                available: self.available_names().join(", "),
            }),
        }
    }
}

/// A resolved match implementation: either the built-in regex, or a configured
/// external command.
#[derive(Debug)]
pub enum ResolvedMatcher<'a> {
    /// The built-in regex matcher (name + summary + body).
    Builtin,
    /// A configured external matcher command template.
    External {
        /// The matcher name (for diagnostics).
        name: String,
        /// Its argv command template.
        config: &'a MatcherConfig,
    },
}

/// A warning raised while interpreting an external matcher's output. These do
/// not fail the run; the caller surfaces them (typically to stderr) and drops
/// the offending line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchWarning {
    /// A printed line was not one of the candidate paths handed to the matcher.
    PathOutsideCandidateSet(String),
}

impl std::fmt::Display for MatchWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MatchWarning::PathOutsideCandidateSet(line) => write!(
                f,
                "match implementation returned a path outside the candidate set, ignoring: {line}"
            ),
        }
    }
}

/// The outcome of running an external matcher: the matching candidate paths (a
/// subset of the input, in the order the matcher printed them) plus any warnings
/// raised while validating the output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchOutcome {
    /// The matched candidate paths, order preserved from the matcher's stdout.
    pub matched: Vec<PathBuf>,
    /// Non-fatal warnings about ignored output lines.
    pub warnings: Vec<MatchWarning>,
}

/// Build the concrete argv for an external matcher by expanding the template:
/// `<QUERY>` becomes one argument, `<CANDIDATES>` becomes one argument per
/// candidate path. A template element that is not a placeholder is passed
/// through verbatim, so a matcher can carry fixed flags alongside the
/// placeholders.
fn expand_command(template: &[String], query: &str, candidates: &[PathBuf]) -> Vec<String> {
    let mut argv = Vec::new();
    for element in template {
        match element.as_str() {
            QUERY_PLACEHOLDER => argv.push(query.to_string()),
            CANDIDATES_PLACEHOLDER => {
                argv.extend(candidates.iter().map(|p| p.to_string_lossy().into_owned()));
            }
            other => argv.push(other.to_string()),
        }
    }
    argv
}

/// Run an external matcher over a candidate set and interpret its output per the
/// protocol.
///
/// The candidate paths are handed to the command via the expanded template. The
/// matcher's stdout is read as newline-separated paths; each is kept only if it
/// is one of the candidates (so the matcher can only ever narrow the set, never
/// invent members), and the surviving order is the matcher's. A line outside the
/// candidate set is dropped with a warning.
///
/// Exit handling is grep-style: a non-zero exit with empty stdout is "no
/// matches" and yields an empty result, not an error; a non-zero exit that wrote
/// to stderr is a failure and surfaces that stderr.
pub fn run_external(
    name: &str,
    config: &MatcherConfig,
    query: &str,
    candidates: &[PathBuf],
) -> Result<MatchOutcome, MatcherError> {
    let argv = expand_command(&config.command, query, candidates);
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| MatcherError::EmptyCommand(name.to_string()))?;

    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|source| MatcherError::Spawn {
            name: name.to_string(),
            source,
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        // grep convention: non-zero with nothing on stdout is "no matches",
        // which is a valid empty result, not a failure. A non-zero exit that
        // also wrote a diagnostic is a genuine failure worth surfacing.
        if stdout.trim().is_empty() {
            if stderr.trim().is_empty() {
                return Ok(MatchOutcome {
                    matched: Vec::new(),
                    warnings: Vec::new(),
                });
            }
            return Err(MatcherError::Failed {
                name: name.to_string(),
                stderr: stderr.trim().to_string(),
            });
        }
        // Non-zero but with stdout: still surface the stderr as a failure, since
        // the exit code says the matcher did not complete cleanly.
        if !stderr.trim().is_empty() {
            return Err(MatcherError::Failed {
                name: name.to_string(),
                stderr: stderr.trim().to_string(),
            });
        }
    }

    Ok(interpret_output(&stdout, candidates))
}

/// Interpret a matcher's stdout against the candidate set: keep only lines that
/// are exactly one of the candidate paths, preserving the matcher's order, and
/// warn about any line that falls outside the set. Blank lines are skipped
/// silently (a trailing newline is normal output, not an error).
fn interpret_output(stdout: &str, candidates: &[PathBuf]) -> MatchOutcome {
    let candidate_set: std::collections::HashSet<&Path> =
        candidates.iter().map(|p| p.as_path()).collect();

    let mut matched = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in stdout.lines() {
        let trimmed = line.trim_end_matches(['\r']);
        if trimmed.is_empty() {
            continue;
        }
        let path = Path::new(trimmed);
        if candidate_set.contains(path) {
            // Preserve order; guard against a matcher echoing a path twice.
            if seen.insert(path.to_path_buf()) {
                matched.push(path.to_path_buf());
            }
        } else {
            warnings.push(MatchWarning::PathOutsideCandidateSet(trimmed.to_string()));
        }
    }
    MatchOutcome { matched, warnings }
}

/// Read the `[match-impl.*]` tables from a config file, returning an empty map
/// if the file is missing or unparseable. Discovery already validates the root
/// pointer; here a malformed file simply contributes no matchers rather than
/// failing an unrelated `list`.
fn read_match_impls(path: &Path) -> BTreeMap<String, MatcherConfig> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    match toml::from_str::<ConfigDoc>(&text) {
        Ok(doc) => doc.match_impl,
        Err(_) => BTreeMap::new(),
    }
}

/// The conventional user-global config path under the XDG config home, if it can
/// be determined. Honours `XDG_CONFIG_HOME`, falling back to `~/.config`.
pub fn user_global_config_path() -> Option<PathBuf> {
    Some(xdg_config_home()?.join("loti").join("config.toml"))
}

/// The XDG config home directory, if it can be determined: `$XDG_CONFIG_HOME`
/// when set and non-empty, else `~/.config` from `$HOME`. Shared by every
/// user-global path loti derives from it, so they cannot disagree on the base.
pub(crate) fn xdg_config_home() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg));
        }
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".config"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(command: &[&str]) -> MatcherConfig {
        MatcherConfig {
            command: command.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn expand_query_and_candidates_placeholders() {
        let template = vec![
            "matcher".to_string(),
            "--pattern".to_string(),
            QUERY_PLACEHOLDER.to_string(),
            CANDIDATES_PLACEHOLDER.to_string(),
        ];
        let candidates = vec![PathBuf::from("/a/1.md"), PathBuf::from("/a/2.md")];
        let argv = expand_command(&template, "needle", &candidates);
        assert_eq!(
            argv,
            vec!["matcher", "--pattern", "needle", "/a/1.md", "/a/2.md"]
        );
    }

    #[test]
    fn candidates_expands_to_zero_args_when_empty() {
        let template = vec!["m".to_string(), CANDIDATES_PLACEHOLDER.to_string()];
        let argv = expand_command(&template, "q", &[]);
        assert_eq!(argv, vec!["m"]);
    }

    #[test]
    fn interpret_keeps_subset_in_order_and_warns_on_outsiders() {
        let candidates = vec![
            PathBuf::from("/a/1.md"),
            PathBuf::from("/a/2.md"),
            PathBuf::from("/a/3.md"),
        ];
        // Matcher returns 3 then 1 (order significant), plus a bogus path.
        let stdout = "/a/3.md\n/a/1.md\n/elsewhere/9.md\n";
        let outcome = interpret_output(stdout, &candidates);
        assert_eq!(
            outcome.matched,
            vec![PathBuf::from("/a/3.md"), PathBuf::from("/a/1.md")]
        );
        assert_eq!(outcome.warnings.len(), 1);
        assert_eq!(
            outcome.warnings[0],
            MatchWarning::PathOutsideCandidateSet("/elsewhere/9.md".into())
        );
    }

    #[test]
    fn interpret_skips_blank_lines_silently() {
        let candidates = vec![PathBuf::from("/a/1.md")];
        let outcome = interpret_output("\n/a/1.md\n\n", &candidates);
        assert_eq!(outcome.matched, vec![PathBuf::from("/a/1.md")]);
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn resolve_builtin_by_reserved_name() {
        let reg = MatcherRegistry::default();
        assert!(matches!(
            reg.resolve("regex").unwrap(),
            ResolvedMatcher::Builtin
        ));
    }

    #[test]
    fn resolve_unknown_lists_available() {
        let mut map = BTreeMap::new();
        map.insert("rg".to_string(), cfg(&["rg"]));
        let reg = MatcherRegistry::from_map(map);
        let err = reg.resolve("nope").unwrap_err();
        match err {
            MatcherError::Unknown {
                requested,
                available,
            } => {
                assert_eq!(requested, "nope");
                // Built-in first, then configured names.
                assert_eq!(available, "regex, rg");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn config_regex_name_is_reserved_and_dropped() {
        let mut map = BTreeMap::new();
        map.insert("regex".to_string(), cfg(&["evil"]));
        map.insert("rg".to_string(), cfg(&["rg"]));
        let reg = MatcherRegistry::from_map(map);
        // The reserved name still resolves to the built-in, not the config.
        assert!(matches!(
            reg.resolve("regex").unwrap(),
            ResolvedMatcher::Builtin
        ));
        assert_eq!(reg.available_names(), vec!["regex", "rg"]);
    }

    #[test]
    fn layered_project_wins_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        let project = dir.path().join("project.toml");
        std::fs::write(
            &user,
            "[match-impl.shared]\ncommand = [\"user-cmd\"]\n\
             [match-impl.user-only]\ncommand = [\"u\"]\n",
        )
        .unwrap();
        std::fs::write(
            &project,
            "loti-root = \".\"\n\
             [match-impl.shared]\ncommand = [\"project-cmd\"]\n\
             [match-impl.project-only]\ncommand = [\"p\"]\n",
        )
        .unwrap();
        let reg = MatcherRegistry::layered(Some(&user), Some(&project));
        // Project wins the collision.
        assert_eq!(reg.get("shared").unwrap().command, vec!["project-cmd"]);
        // Both exclusive entries survive.
        assert_eq!(reg.get("user-only").unwrap().command, vec!["u"]);
        assert_eq!(reg.get("project-only").unwrap().command, vec!["p"]);
    }

    #[test]
    fn layered_tolerates_missing_files() {
        let reg = MatcherRegistry::layered(
            Some(Path::new("/nonexistent/user.toml")),
            Some(Path::new("/nonexistent/project.toml")),
        );
        assert_eq!(reg.available_names(), vec!["regex"]);
    }

    #[test]
    fn xdg_config_path_honours_env() {
        let _env = XDG_CONFIG_HOME_LOCK.lock().unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        let path = user_global_config_path().unwrap();
        match previous {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        assert_eq!(path, dir.path().join("loti").join("config.toml"));
    }

    // -- external matcher process protocol (hermetic, via a fake matcher) ---

    /// Write a POSIX shell fake matcher into `dir` and return its path. Tests
    /// invoke it through `/bin/sh`, so a freshly written script is never exec'd
    /// while the filesystem may still consider it busy.
    fn write_matcher(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        path
    }

    /// Build the command template every fake matcher uses. The interpreter is
    /// the executable; the temporary script is an argument it reads.
    fn shell_matcher(script: &Path) -> MatcherConfig {
        MatcherConfig {
            command: vec![
                "/bin/sh".to_string(),
                script.to_string_lossy().into_owned(),
                QUERY_PLACEHOLDER.to_string(),
                CANDIDATES_PLACEHOLDER.to_string(),
            ],
        }
    }

    fn candidate_files(dir: &Path, count: usize) -> Vec<PathBuf> {
        (1..=count)
            .map(|n| {
                let p = dir.join(format!("{n}.md"));
                std::fs::write(&p, format!("file {n}")).unwrap();
                p
            })
            .collect()
    }

    #[test]
    fn external_returns_subset_with_order_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let candidates = candidate_files(dir.path(), 3);
        // Echo the third candidate then the first: order significant.
        let script = write_matcher(dir.path(), "m.sh", "shift; printf '%s\\n' \"$3\" \"$1\"");
        // Template: /bin/sh script <QUERY> <CANDIDATES>. After `shift` the
        // candidates are $1..$3; the script prints $3 then $1.
        let config = shell_matcher(&script);
        let outcome = run_external("m", &config, "needle", &candidates).unwrap();
        assert_eq!(
            outcome.matched,
            vec![candidates[2].clone(), candidates[0].clone()]
        );
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn external_paths_outside_set_are_ignored_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let candidates = candidate_files(dir.path(), 2);
        // Print candidate 1 then a bogus path not in the set.
        let script = write_matcher(
            dir.path(),
            "m.sh",
            "shift; printf '%s\\n' \"$1\"; printf '%s\\n' \"/nowhere/9.md\"",
        );
        let config = shell_matcher(&script);
        let outcome = run_external("m", &config, "q", &candidates).unwrap();
        assert_eq!(outcome.matched, vec![candidates[0].clone()]);
        assert_eq!(outcome.warnings.len(), 1);
        assert!(matches!(
            &outcome.warnings[0],
            MatchWarning::PathOutsideCandidateSet(p) if p == "/nowhere/9.md"
        ));
    }

    #[test]
    fn external_unparseable_line_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let candidates = candidate_files(dir.path(), 1);
        // Emit garbage plus the real candidate; the garbage is not a candidate
        // path, so it is dropped with a warning and the real one survives.
        let script = write_matcher(
            dir.path(),
            "m.sh",
            "shift; printf '%s\\n' 'not a path at all'; printf '%s\\n' \"$1\"",
        );
        let config = shell_matcher(&script);
        let outcome = run_external("m", &config, "q", &candidates).unwrap();
        assert_eq!(outcome.matched, vec![candidates[0].clone()]);
        assert_eq!(outcome.warnings.len(), 1);
    }

    #[test]
    fn external_nonzero_empty_stdout_is_zero_matches_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let candidates = candidate_files(dir.path(), 2);
        // grep convention: exit 1 with no stdout, no stderr = no matches.
        let script = write_matcher(dir.path(), "m.sh", "exit 1");
        let config = shell_matcher(&script);
        let outcome = run_external("m", &config, "q", &candidates).unwrap();
        assert!(outcome.matched.is_empty());
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn external_nonzero_with_stderr_is_surfaced_as_failure() {
        let dir = tempfile::tempdir().unwrap();
        let candidates = candidate_files(dir.path(), 1);
        let script = write_matcher(dir.path(), "m.sh", "echo 'boom' 1>&2; exit 2");
        let config = shell_matcher(&script);
        let err = run_external("m", &config, "q", &candidates).unwrap_err();
        match err {
            MatcherError::Failed { name, stderr } => {
                assert_eq!(name, "m");
                assert!(stderr.contains("boom"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn external_receives_query_and_candidate_args() {
        // Prove the argv expansion reaches the process: the script writes its
        // received query and candidate count to files we then inspect.
        let dir = tempfile::tempdir().unwrap();
        let candidates = candidate_files(dir.path(), 3);
        let query_out = dir.path().join("seen-query");
        let count_out = dir.path().join("seen-count");
        // $1 is the query (before shift); after shift, $# is the candidate count.
        let body = format!(
            "printf '%s' \"$1\" > {q}; shift; printf '%s' \"$#\" > {c}; printf '%s\\n' \"$1\"",
            q = query_out.to_string_lossy(),
            c = count_out.to_string_lossy(),
        );
        let script = write_matcher(dir.path(), "m.sh", &body);
        let config = shell_matcher(&script);
        let outcome = run_external("m", &config, "find-me", &candidates).unwrap();
        assert_eq!(std::fs::read_to_string(&query_out).unwrap(), "find-me");
        // Three candidate path args were expanded.
        assert_eq!(std::fs::read_to_string(&count_out).unwrap(), "3");
        assert_eq!(outcome.matched, vec![candidates[0].clone()]);
    }

    #[test]
    fn external_matchers_survive_parallel_fresh_script_writes() {
        const WORKERS: usize = 8;
        const RUNS_PER_WORKER: usize = 20;

        // Every run writes a fresh script immediately before invoking it. Any
        // failure is terminal; this increases contention without retrying it.
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..WORKERS)
                .map(|worker| {
                    scope.spawn(move || {
                        for run in 0..RUNS_PER_WORKER {
                            let dir = tempfile::tempdir().unwrap();
                            let candidates = candidate_files(dir.path(), 2);
                            let script = write_matcher(
                                dir.path(),
                                "m.sh",
                                "test \"$1\" = needle || exit 9; shift; test \"$#\" -eq 2 || exit 9; printf '%s\\n' \"$1\"",
                            );
                            let outcome = run_external(
                                "m",
                                &shell_matcher(&script),
                                "needle",
                                &candidates,
                            )
                            .unwrap_or_else(|error| {
                                panic!("worker {worker}, run {run} failed: {error}")
                            });
                            assert_eq!(
                                outcome.matched,
                                vec![candidates[0].clone()],
                                "worker {worker}, run {run}"
                            );
                            assert!(outcome.warnings.is_empty(), "worker {worker}, run {run}");
                        }
                    })
                })
                .collect();

            for worker in workers {
                worker.join().unwrap();
            }
        });
    }
}
