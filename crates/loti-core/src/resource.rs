//! Discovering and loading agent harness profiles and Markdown workflows.
//!
//! A resource (a harness profile or a workflow) is identified by the
//! case-sensitive, non-empty filename stem of a direct `.toml` (profile) or
//! `.md` (workflow) file under one of two shallow directories: a repository
//! *local* root and a user-machine *global* root. Only direct children are
//! considered — nested paths, and files with any other or mixed-case
//! extension, are not candidates at all.
//!
//! A local candidate shadows a global candidate of the exact same raw
//! filename stem *before* either is validated, so a broken local override is
//! reported as broken rather than silently falling back to the global
//! definition. Everything this module does is exposed through [`Effective`]:
//! a discovered candidate always carries its [`Origin`] and, whether it
//! loaded cleanly or not, an ordered list of [`Diagnostic`]s (warnings beside
//! a usable value, or the error(s) that made it unusable). This is the one
//! place that owns discovery and precedence; a CLI or TUI consumer lists or
//! resolves effective resources and never reimplements shadowing itself.
//!
//! The local root for each kind is not a fixed directory: it exists only when
//! the project config names it via the optional `agent-root` / `workflow-root`
//! keys, resolved the same way as `loti-root` (absolute, or relative to the
//! config file). The global root is always the XDG-derived
//! `~/.config/loti/agents` or `~/.config/loti/workflows` (honouring
//! `XDG_CONFIG_HOME`).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::discovery::resolve_relative_to_config;
use crate::matcher::xdg_config_home;

/// Where an effective resource's definition came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Found under the repository-local root.
    Local,
    /// Found under the user-global root.
    Global,
}

impl Origin {
    /// The lower-case name used in output.
    pub fn wire_name(self) -> &'static str {
        match self {
            Origin::Local => "local",
            Origin::Global => "global",
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wire_name())
    }
}

/// A candidate or requested resource identifier: a non-empty string of ASCII
/// letters, digits, hyphens, and underscores. Comparison is case-sensitive —
/// IDs that differ only by letter case are distinct (if non-portable across
/// case-insensitive filesystems).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(String);

/// Why a requested or candidate ID failed the format rule.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The empty string is never a valid ID.
    #[error("resource id must not be empty")]
    Empty,
    /// A character outside ASCII letters, digits, `-`, and `_`.
    #[error("resource id '{0}' must contain only ASCII letters, digits, '-', or '_'")]
    InvalidCharacters(String),
}

impl ResourceId {
    /// Validate a requested or candidate ID string. This is the one place the
    /// format rule is stated, so a filename stem discovered on disk and an ID
    /// typed by an operator are held to exactly the same rule.
    pub fn parse(raw: &str) -> Result<Self, IdError> {
        if raw.is_empty() {
            return Err(IdError::Empty);
        }
        if !raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(IdError::InvalidCharacters(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    /// The validated ID string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The severity a [`Diagnostic`] carries. A resource with only warnings is
/// still usable; one carrying an error is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Tolerated: the resource remains usable.
    Warning,
    /// Blocking: the resource is invalid.
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Warning => f.write_str("warning"),
            Severity::Error => f.write_str("error"),
        }
    }
}

/// One reported issue with a candidate or effective resource. The message
/// states its own severity via [`Display`](fmt::Display) (derived from
/// `severity`, so the two can never say different things), matching the rule
/// that a resource row carries diagnostics but no separate valid/invalid flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Whether this diagnostic blocks the resource or is merely tolerated.
    pub severity: Severity,
    /// The human-readable explanation, without a severity prefix (added by
    /// `Display`).
    pub message: String,
}

impl Diagnostic {
    fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.severity, self.message)
    }
}

/// One resource discovered under an ID: its origin, and either a loaded value
/// (with any non-fatal warnings) or the diagnostics that made it invalid.
/// Always present regardless of validity, so a roster can list every candidate
/// without dropping the broken ones, and a single lookup can report why a
/// selected resource is unusable instead of just disappearing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective<T> {
    /// The raw filename stem. Not a [`ResourceId`] here: an invalid-ID
    /// candidate is still reported, and its raw stem is what a diagnostic
    /// refers to.
    pub id: String,
    /// Where this resource's definition came from.
    pub origin: Origin,
    /// The loaded value, when this resource is usable.
    pub value: Option<T>,
    /// Ordered diagnostics: warnings beside a present value, or the error(s)
    /// explaining why `value` is absent.
    pub diagnostics: Vec<Diagnostic>,
}

impl<T> Effective<T> {
    /// Whether this resource loaded to a usable value (independent of whether
    /// it also carries warnings).
    pub fn is_valid(&self) -> bool {
        self.value.is_some()
    }
}

/// A harness profile's recognized shape: a direct executable, its ordered
/// argument templates, an optional working directory template, and an
/// optional environment-value template map. Values are held verbatim here —
/// rendering the `{{ ... }}` placeholders they may contain is a later
/// boundary's job; this module only establishes that the shape parses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The executable name or path, run directly (never through a shell).
    pub command: String,
    /// The ordered argument templates.
    pub args: Vec<String>,
    /// The working-directory template, if the profile overrides the default.
    pub cwd: Option<String>,
    /// The environment-value template map, if the profile adds any.
    pub env: Option<BTreeMap<String, String>>,
}

/// Failure to resolve the two directories a resource kind's candidates come
/// from into one roster: the local root is validated eagerly when the project
/// config loads, so a failure here means a root (almost always the global one)
/// exists but could not be listed — an unreadable directory, or a path that is
/// not a directory at all. A root that simply does not exist contributes no
/// entries and is not an error.
#[derive(Debug, thiserror::Error)]
#[error("could not read {root}: {source}")]
pub struct RootError {
    /// The root directory that could not be listed.
    pub root: PathBuf,
    /// The underlying I/O error.
    #[source]
    pub source: std::io::Error,
}

/// Failure to resolve a requested ID plus list its roster: the ID may be
/// malformed (checked before any filesystem access) or a root may be
/// unreadable (checked while building the roster to search).
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// The requested ID string is not a valid [`ResourceId`].
    #[error(transparent)]
    Id(#[from] IdError),
    /// A root could not be read while building the roster.
    #[error(transparent)]
    Root(#[from] RootError),
}

/// The two directories discovery draws one resource kind's candidates from.
/// Either may be absent: `local` only exists when its project-config key is
/// set, `global` only when its XDG home cannot be determined.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    /// The repository-local root, if configured.
    pub local: Option<PathBuf>,
    /// The user-global root, if determinable.
    pub global: Option<PathBuf>,
}

/// The `agent-root` / `workflow-root` keys read from a project config file.
#[derive(Debug, Clone, Default, Deserialize)]
struct RootsDoc {
    #[serde(rename = "agent-root")]
    agent_root: Option<String>,
    #[serde(rename = "workflow-root")]
    workflow_root: Option<String>,
}

/// The local roots named by a project config file, before a global root is
/// layered on. `None` for a kind whose key is absent — there is no default
/// local directory, so a kind with no configured key has no local resources.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalRoots {
    /// The `agent-root` value, resolved and validated.
    pub agents: Option<PathBuf>,
    /// The `workflow-root` value, resolved and validated.
    pub workflows: Option<PathBuf>,
}

/// Failure to read the project config's resource-root keys, or a configured
/// root that does not resolve to an existing directory.
#[derive(Debug, thiserror::Error)]
pub enum ProjectConfigError {
    /// The config file could not be read.
    #[error("could not read project config {path}: {source}")]
    Read {
        /// The config file path.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The config file could not be parsed as TOML, or a recognized key had
    /// the wrong type.
    #[error("project config {path} is malformed: {message}")]
    Parse {
        /// The config file path.
        path: PathBuf,
        /// A human-readable explanation.
        message: String,
    },
    /// A configured root does not resolve, through any symlinks, to an
    /// existing directory.
    #[error("'{key}' in {path} does not resolve to an existing directory ({raw}): {reason}")]
    InvalidRoot {
        /// The config key (`agent-root` or `workflow-root`).
        key: &'static str,
        /// The config file path.
        path: PathBuf,
        /// The raw configured value.
        raw: String,
        /// Why it does not resolve.
        reason: String,
    },
}

/// Read and validate the `agent-root` / `workflow-root` keys from a project
/// config file. Each is optional; a key that is absent contributes no local
/// root for its kind rather than defaulting to one. A key that is present must
/// resolve, through any symlinks, to an existing directory, or this is a
/// configuration error — a broken configured root is never silently ignored
/// the way a merely-absent one is.
pub fn local_roots(config_path: &Path) -> Result<LocalRoots, ProjectConfigError> {
    let text = std::fs::read_to_string(config_path).map_err(|source| ProjectConfigError::Read {
        path: config_path.to_path_buf(),
        source,
    })?;
    let doc: RootsDoc = toml::from_str(&text).map_err(|e| ProjectConfigError::Parse {
        path: config_path.to_path_buf(),
        message: e.to_string(),
    })?;
    Ok(LocalRoots {
        agents: resolve_configured_root(config_path, "agent-root", doc.agent_root.as_deref())?,
        workflows: resolve_configured_root(
            config_path,
            "workflow-root",
            doc.workflow_root.as_deref(),
        )?,
    })
}

/// Resolve and validate one optional root key: `None` when the key is absent,
/// otherwise the canonicalized directory or a [`ProjectConfigError`].
fn resolve_configured_root(
    config_path: &Path,
    key: &'static str,
    raw: Option<&str>,
) -> Result<Option<PathBuf>, ProjectConfigError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let candidate = resolve_relative_to_config(config_path, raw);
    let canonical =
        std::fs::canonicalize(&candidate).map_err(|source| ProjectConfigError::InvalidRoot {
            key,
            path: config_path.to_path_buf(),
            raw: raw.to_string(),
            reason: source.to_string(),
        })?;
    if !canonical.is_dir() {
        return Err(ProjectConfigError::InvalidRoot {
            key,
            path: config_path.to_path_buf(),
            raw: raw.to_string(),
            reason: "not a directory".to_string(),
        });
    }
    Ok(Some(canonical))
}

/// The user-global agent-profile root: `<XDG config home>/loti/agents`.
/// `None` only when the XDG config home itself cannot be determined.
pub fn global_agent_root() -> Option<PathBuf> {
    Some(xdg_config_home()?.join("loti").join("agents"))
}

/// The user-global workflow root: `<XDG config home>/loti/workflows`. `None`
/// only when the XDG config home itself cannot be determined.
pub fn global_workflow_root() -> Option<PathBuf> {
    Some(xdg_config_home()?.join("loti").join("workflows"))
}

/// One directory candidate: a resource file's raw filename stem (unvalidated
/// as an ID) and its path.
struct Candidate {
    stem: String,
    path: PathBuf,
}

/// The direct children of `dir` whose extension exactly matches `extension`
/// (case-sensitive), ignoring every other entry — nested paths are never
/// visited, since `read_dir` is not recursive. A directory that does not exist
/// contributes no candidates, since a missing root is normal (no local root is
/// configured, or no global root has ever been created); one that exists but
/// cannot be listed (unreadable, or not a directory at all) fails outright,
/// since silently treating that as empty would understate the effective
/// catalog.
fn scan_dir(dir: &Path, extension: &str) -> Result<Vec<Candidate>, RootError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(RootError {
                root: dir.to_path_buf(),
                source,
            })
        }
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| RootError {
            root: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(extension) {
            continue;
        }
        // `file_stem` on a name with no non-extension part (e.g. `.toml`)
        // returns the whole name including the leading dot; such a name is
        // never a valid ID and is reported as such once loaded, rather than
        // silently skipped here.
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        candidates.push(Candidate {
            stem: stem.to_string(),
            path,
        });
    }
    Ok(candidates)
}

/// Enumerate one resource kind's effective roster: local and global
/// candidates, local-over-global shadowing by exact raw stem (before either
/// side is validated), then load and validate whatever survives. The roster is
/// sorted by bytewise lexical ID, so presentation never depends on filesystem
/// enumeration order.
fn discover<T>(
    roots: &Roots,
    extension: &str,
    load: impl Fn(&Path) -> Result<(T, Vec<Diagnostic>), Diagnostic>,
) -> Result<Vec<Effective<T>>, RootError> {
    let local = match &roots.local {
        Some(dir) => scan_dir(dir, extension)?,
        None => Vec::new(),
    };
    let global = match &roots.global {
        Some(dir) => scan_dir(dir, extension)?,
        None => Vec::new(),
    };

    // Shadowing happens on the raw stem, before ID-format validation: a local
    // candidate whose stem is not even a valid ID still shadows a global
    // candidate of that same stem, so a broken local override is reported as
    // broken rather than falling back to a valid global definition.
    let local_stems: std::collections::HashSet<String> =
        local.iter().map(|c| c.stem.clone()).collect();

    let mut entries: Vec<Effective<T>> = local
        .into_iter()
        .map(|c| build_entry(Origin::Local, c, &load))
        .collect();
    entries.extend(
        global
            .into_iter()
            .filter(|c| !local_stems.contains(c.stem.as_str()))
            .map(|c| build_entry(Origin::Global, c, &load)),
    );

    entries.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    Ok(entries)
}

/// Validate a candidate's stem as an ID, then load its contents. Either
/// failure yields an invalid [`Effective`] carrying its diagnostic; both share
/// this one path so a roster and a single resolve can never disagree about
/// what makes a candidate invalid.
fn build_entry<T>(
    origin: Origin,
    candidate: Candidate,
    load: &impl Fn(&Path) -> Result<(T, Vec<Diagnostic>), Diagnostic>,
) -> Effective<T> {
    if let Err(e) = ResourceId::parse(&candidate.stem) {
        return Effective {
            id: candidate.stem,
            origin,
            value: None,
            diagnostics: vec![Diagnostic::error(e.to_string())],
        };
    }
    match load(&candidate.path) {
        Ok((value, diagnostics)) => Effective {
            id: candidate.stem,
            origin,
            value: Some(value),
            diagnostics,
        },
        Err(diagnostic) => Effective {
            id: candidate.stem,
            origin,
            value: None,
            diagnostics: vec![diagnostic],
        },
    }
}

/// Read a `.toml` profile file and parse its recognized shape.
fn load_profile(path: &Path) -> Result<(Profile, Vec<Diagnostic>), Diagnostic> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Diagnostic::error(format!("could not read profile: {e}")))?;
    parse_profile(&text)
}

/// Parse a profile's recognized fields out of its TOML table, treating an
/// unrecognized top-level key as a warning rather than an error: `command`
/// (required string), `args` (required array of strings), `cwd` (optional
/// string), `env` (optional table of strings). A missing or wrong-typed
/// recognized field is a hard error.
fn parse_profile(text: &str) -> Result<(Profile, Vec<Diagnostic>), Diagnostic> {
    let mut table: toml::Table =
        toml::from_str(text).map_err(|e| Diagnostic::error(format!("malformed profile: {e}")))?;

    let command = match table.remove("command") {
        Some(toml::Value::String(s)) => s,
        Some(_) => return Err(Diagnostic::error("'command' must be a string")),
        None => return Err(Diagnostic::error("missing required field 'command'")),
    };

    let args = match table.remove("args") {
        Some(toml::Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    toml::Value::String(s) => out.push(s),
                    _ => return Err(Diagnostic::error("'args' must be an array of strings")),
                }
            }
            out
        }
        Some(_) => return Err(Diagnostic::error("'args' must be an array of strings")),
        None => return Err(Diagnostic::error("missing required field 'args'")),
    };

    let cwd = match table.remove("cwd") {
        Some(toml::Value::String(s)) => Some(s),
        Some(_) => return Err(Diagnostic::error("'cwd' must be a string")),
        None => None,
    };

    let env = match table.remove("env") {
        Some(toml::Value::Table(t)) => {
            let mut map = BTreeMap::new();
            for (k, v) in t {
                match v {
                    toml::Value::String(s) => {
                        map.insert(k, s);
                    }
                    _ => return Err(Diagnostic::error(format!("'env.{k}' must be a string"))),
                }
            }
            Some(map)
        }
        Some(_) => return Err(Diagnostic::error("'env' must be a table of strings")),
        None => None,
    };

    // Whatever is left in the table is unrecognized. `toml::Table` is ordered
    // by key (it is a `BTreeMap` without the `preserve_order` feature), giving
    // a stable, deterministic order for these warnings without this module
    // having to track source order itself.
    let warnings = table
        .keys()
        .map(|k| Diagnostic::warning(format!("ignoring unknown field '{k}'")))
        .collect();

    Ok((
        Profile {
            command,
            args,
            cwd,
            env,
        },
        warnings,
    ))
}

/// Read a `.md` workflow file. A workflow is opaque Markdown: its valid UTF-8
/// source text is retained completely unchanged, with no parsing or
/// normalization. Invalid UTF-8 is the only way a workflow file is invalid.
fn load_workflow(path: &Path) -> Result<(String, Vec<Diagnostic>), Diagnostic> {
    let bytes = std::fs::read(path)
        .map_err(|e| Diagnostic::error(format!("could not read workflow: {e}")))?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok((text, Vec::new())),
        Err(_) => Err(Diagnostic::error("workflow file is not valid UTF-8")),
    }
}

/// The effective agent-profile roster: every local and global `.toml`
/// candidate, local-over-global shadowed, loaded, and sorted by ID.
pub fn list_profiles(roots: &Roots) -> Result<Vec<Effective<Profile>>, RootError> {
    discover(roots, "toml", load_profile)
}

/// The effective workflow roster: every local and global `.md` candidate,
/// local-over-global shadowed, loaded, and sorted by ID.
pub fn list_workflows(roots: &Roots) -> Result<Vec<Effective<String>>, RootError> {
    discover(roots, "md", load_workflow)
}

/// Resolve one requested agent-profile ID against the effective roster. The
/// requested ID is validated before any filesystem access: a malformed
/// request never triggers a directory scan. `Ok(None)` means no effective
/// resource of that ID exists at all (valid or invalid).
pub fn resolve_profile(
    roots: &Roots,
    requested: &str,
) -> Result<Option<Effective<Profile>>, ResolveError> {
    resolve(roots, requested, "toml", load_profile)
}

/// Resolve one requested workflow ID against the effective roster. The
/// requested ID is validated before any filesystem access: a malformed
/// request never triggers a directory scan. `Ok(None)` means no effective
/// resource of that ID exists at all (valid or invalid).
pub fn resolve_workflow(
    roots: &Roots,
    requested: &str,
) -> Result<Option<Effective<String>>, ResolveError> {
    resolve(roots, requested, "md", load_workflow)
}

/// Shared resolve implementation: validate the requested ID, then build the
/// same roster `list_*` would and pick the matching entry out of it, so
/// listing and resolving can never disagree about precedence or validity.
fn resolve<T>(
    roots: &Roots,
    requested: &str,
    extension: &str,
    load: impl Fn(&Path) -> Result<(T, Vec<Diagnostic>), Diagnostic>,
) -> Result<Option<Effective<T>>, ResolveError> {
    let id = ResourceId::parse(requested)?;
    let roster = discover(roots, extension, load)?;
    Ok(roster.into_iter().find(|e| e.id == id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn valid_profile(command: &str) -> String {
        format!("command = \"{command}\"\nargs = [\"a\"]\n")
    }

    #[test]
    fn origin_display_matches_wire_name() {
        assert_eq!(Origin::Local.to_string(), "local");
        assert_eq!(Origin::Global.to_string(), "global");
    }

    // -- ID validation ------------------------------------------------------

    #[test]
    fn id_rejects_empty() {
        assert_eq!(ResourceId::parse("").unwrap_err(), IdError::Empty);
    }

    #[test]
    fn id_rejects_space_and_dot() {
        assert!(matches!(
            ResourceId::parse("bad id").unwrap_err(),
            IdError::InvalidCharacters(_)
        ));
        assert!(matches!(
            ResourceId::parse("bad.id").unwrap_err(),
            IdError::InvalidCharacters(_)
        ));
    }

    #[test]
    fn id_accepts_letters_digits_hyphen_underscore() {
        assert!(ResourceId::parse("Agent-9_profile").is_ok());
    }

    #[test]
    fn resolve_rejects_malformed_requested_id_before_touching_disk() {
        // The roots point nowhere real; if resolution reached the filesystem
        // it would surface a `RootError`, not an `IdError`. Getting an
        // `IdError` back proves the request was rejected before any lookup.
        let roots = Roots {
            local: Some(PathBuf::from("/nonexistent/local")),
            global: Some(PathBuf::from("/nonexistent/global")),
        };
        let err = resolve_profile(&roots, "bad id").unwrap_err();
        assert!(matches!(
            err,
            ResolveError::Id(IdError::InvalidCharacters(_))
        ));
    }

    // -- origin and precedence -----------------------------------------------

    #[test]
    fn local_only_resource_has_local_origin() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        write(&local, "solo.toml", &valid_profile("pi"));

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].id, "solo");
        assert_eq!(roster[0].origin, Origin::Local);
        assert!(roster[0].is_valid());
    }

    #[test]
    fn global_only_resource_has_global_origin() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        std::fs::create_dir_all(&global).unwrap();
        write(&global, "solo.toml", &valid_profile("pi"));

        let roster = list_profiles(&Roots {
            local: None,
            global: Some(global),
        })
        .unwrap();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].origin, Origin::Global);
    }

    #[test]
    fn global_roots_use_their_distinct_xdg_directories() {
        let _env = crate::matcher::XDG_CONFIG_HOME_LOCK.lock().unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        let agent_root = global_agent_root().unwrap();
        let workflow_root = global_workflow_root().unwrap();
        match previous {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }

        assert_eq!(agent_root, dir.path().join("loti").join("agents"));
        assert_eq!(workflow_root, dir.path().join("loti").join("workflows"));
    }

    #[test]
    fn exact_same_id_local_shadows_global() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        let global = dir.path().join("global");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        write(&local, "shared.toml", &valid_profile("local-cmd"));
        write(&global, "shared.toml", &valid_profile("global-cmd"));

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: Some(global),
        })
        .unwrap();
        // Only the local definition survives — the global one is entirely
        // gone, not merely shadowed for display.
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].origin, Origin::Local);
        assert_eq!(roster[0].value.as_ref().unwrap().command, "local-cmd");
    }

    #[test]
    fn invalid_local_shadows_valid_global_without_falling_back() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        let global = dir.path().join("global");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        // Local definition is malformed TOML; global is a valid profile.
        write(&local, "shared.toml", "not = [valid");
        write(&global, "shared.toml", &valid_profile("global-cmd"));

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: Some(global),
        })
        .unwrap();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].origin, Origin::Local);
        assert!(!roster[0].is_valid());
        assert_eq!(roster[0].diagnostics.len(), 1);
        assert_eq!(roster[0].diagnostics[0].severity, Severity::Error);

        let resolved = resolve_profile(
            &Roots {
                local: Some(dir.path().join("local")),
                global: Some(dir.path().join("global")),
            },
            "shared",
        )
        .unwrap()
        .unwrap();
        assert!(!resolved.is_valid());
    }

    #[test]
    fn invalid_id_local_shadows_same_stem_global_before_validation() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        let global = dir.path().join("global");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        write(&local, "bad id.toml", &valid_profile("local-cmd"));
        write(&global, "bad id.toml", &valid_profile("global-cmd"));

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: Some(global),
        })
        .unwrap();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].id, "bad id");
        assert_eq!(roster[0].origin, Origin::Local);
        assert!(!roster[0].is_valid());
    }

    #[test]
    fn unreadable_local_shadows_valid_global_where_portable() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        let global = dir.path().join("global");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        let local_path = write(&local, "shared.toml", &valid_profile("local-cmd"));
        write(&global, "shared.toml", &valid_profile("global-cmd"));
        if !make_unreadable(&local_path) {
            eprintln!("skipping: file permissions are not enforced (likely running as root)");
            return;
        }

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: Some(global),
        })
        .unwrap();
        // The broken local definition shadows the valid global one outright: it
        // is reported invalid, never silently replaced by the global value.
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].origin, Origin::Local);
        assert!(!roster[0].is_valid());
    }

    #[test]
    fn resolve_valid_id_not_in_roster_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        write(&local, "present.toml", &valid_profile("pi"));

        let found = resolve_profile(
            &Roots {
                local: Some(local),
                global: None,
            },
            "absent",
        )
        .unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn case_only_distinct_ids_are_both_effective() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        write(&local, "Foo.toml", &valid_profile("upper"));
        write(&local, "foo.toml", &valid_profile("lower"));

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        assert_eq!(roster.len(), 2);
        let ids: Vec<&str> = roster.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["Foo", "foo"]);
    }

    // -- shallow discovery ----------------------------------------------------

    #[test]
    fn nested_and_wrong_extension_entries_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(local.join("nested")).unwrap();
        write(&local, "keep.toml", &valid_profile("kept"));
        write(
            &local.join("nested"),
            "ignored.toml",
            &valid_profile("nested"),
        );
        write(&local, "ignored.txt", "not a profile");
        write(&local, "ignored.TOML", &valid_profile("mixed-case"));
        write(&local, "ignored.Toml", &valid_profile("mixed-case"));

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].id, "keep");
    }

    // -- invalid IDs ------------------------------------------------------------

    #[test]
    fn invalid_id_candidate_is_reported_invalid_but_does_not_block_others() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        write(&local, "bad id.toml", &valid_profile("x"));
        write(&local, "good-id.toml", &valid_profile("y"));

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        assert_eq!(roster.len(), 2);
        let bad = roster.iter().find(|e| e.id == "bad id").unwrap();
        assert!(!bad.is_valid());
        assert_eq!(bad.diagnostics[0].severity, Severity::Error);
        let good = roster.iter().find(|e| e.id == "good-id").unwrap();
        assert!(good.is_valid());
    }

    // -- malformed profiles -----------------------------------------------------

    #[test]
    fn missing_required_field_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        write(&local, "no-args.toml", "command = \"pi\"\n");

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        assert!(!roster[0].is_valid());
        assert!(roster[0].diagnostics[0].message.contains("args"));
    }

    #[test]
    fn wrong_typed_field_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        write(
            &local,
            "bad-cwd.toml",
            "command = \"pi\"\nargs = []\ncwd = 5\n",
        );

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        assert!(!roster[0].is_valid());
        assert!(roster[0].diagnostics[0].message.contains("cwd"));
    }

    #[test]
    fn profile_unknown_field_is_a_warning_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        write(
            &local,
            "extra.toml",
            "command = \"pi\"\nargs = [\"a\"]\nnickname = \"whatever\"\n",
        );

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        assert!(roster[0].is_valid());
        assert_eq!(roster[0].diagnostics.len(), 1);
        assert_eq!(roster[0].diagnostics[0].severity, Severity::Warning);
        assert!(roster[0].diagnostics[0].message.contains("nickname"));
        // The recognized fields still parsed correctly despite the warning.
        let profile = roster[0].value.as_ref().unwrap();
        assert_eq!(profile.command, "pi");
        assert_eq!(profile.args, vec!["a".to_string()]);
    }

    #[test]
    fn profile_env_and_cwd_parse_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        write(
            &local,
            "full.toml",
            "command = \"pi\"\nargs = [\"{{ loti_prompt }}\"]\ncwd = \"{{ project_root }}\"\n\
             [env]\nFOO = \"bar\"\n",
        );

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        let profile = roster[0].value.as_ref().unwrap();
        assert_eq!(profile.cwd, Some("{{ project_root }}".to_string()));
        assert_eq!(
            profile.env.as_ref().unwrap().get("FOO"),
            Some(&"bar".to_string())
        );
    }

    // -- unreadable candidates (where portable) ---------------------------------

    /// chmod a file to remove all permission bits, returning whether the OS
    /// actually enforces that (it never does for the root user, who bypasses
    /// permission bits entirely). Tests that rely on enforcement skip
    /// themselves when it is not in effect, rather than asserting something
    /// that would only be true on a non-root CI runner.
    fn make_unreadable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::read(path).is_err()
    }

    #[test]
    fn unreadable_profile_file_is_invalid_where_portable() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        let path = write(&local, "locked.toml", &valid_profile("pi"));
        if !make_unreadable(&path) {
            eprintln!("skipping: file permissions are not enforced (likely running as root)");
            return;
        }

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        assert_eq!(roster.len(), 1);
        assert!(!roster[0].is_valid());
    }

    #[test]
    fn unreadable_global_root_fails_the_whole_operation_where_portable() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        std::fs::create_dir_all(&global).unwrap();
        write(&global, "solo.toml", &valid_profile("pi"));

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o000)).unwrap();
        let enforced = std::fs::read_dir(&global).is_err();
        if !enforced {
            eprintln!("skipping: directory permissions are not enforced (likely running as root)");
            // Restore permissions so the tempdir can be cleaned up.
            std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let err = list_profiles(&Roots {
            local: None,
            global: Some(global.clone()),
        })
        .unwrap_err();
        assert_eq!(err.root, global);

        // Restore permissions so the tempdir can be cleaned up.
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn missing_global_root_contributes_no_entries() {
        let dir = tempfile::tempdir().unwrap();
        let roster = list_profiles(&Roots {
            local: None,
            global: Some(dir.path().join("does-not-exist")),
        })
        .unwrap();
        assert!(roster.is_empty());
    }

    #[test]
    fn non_directory_global_root_fails() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "not-a-dir", "x");
        let err = list_profiles(&Roots {
            local: None,
            global: Some(file),
        })
        .unwrap_err();
        assert_eq!(err.root, dir.path().join("not-a-dir"));
    }

    // -- workflows: UTF-8 validation and verbatim content -----------------------

    #[test]
    fn workflow_invalid_utf8_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        let path = local.join("bad.md");
        std::fs::write(&path, [0x66, 0x6f, 0xff, 0x6f]).unwrap();

        let roster = list_workflows(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        assert_eq!(roster.len(), 1);
        assert!(!roster[0].is_valid());
        assert_eq!(roster[0].diagnostics[0].severity, Severity::Error);
    }

    #[test]
    fn workflow_valid_utf8_is_retained_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        // Leading blank line, trailing spaces, no final newline: anything a
        // "helpful" normalization pass might be tempted to touch.
        let source = "\n# Title  \n\nBody text.\nNo trailing newline";
        write(&local, "verbatim.md", source);

        let roster = list_workflows(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        assert_eq!(roster[0].value.as_deref(), Some(source));

        let resolved = resolve_workflow(
            &Roots {
                local: Some(dir.path().join("local")),
                global: None,
            },
            "verbatim",
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.value.as_deref(), Some(source));
    }

    // -- stable roster ordering ---------------------------------------------------

    #[test]
    fn roster_is_sorted_bytewise_not_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        // Bytewise ASCII order places every uppercase letter before every
        // lowercase one; a case-insensitive or locale-aware sort would not.
        write(&local, "b.toml", &valid_profile("b"));
        write(&local, "A.toml", &valid_profile("A"));
        write(&local, "a.toml", &valid_profile("a"));

        let roster = list_profiles(&Roots {
            local: Some(local),
            global: None,
        })
        .unwrap();
        let ids: Vec<&str> = roster.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["A", "a", "b"]);
    }

    // -- project config resource-root parsing ------------------------------------

    #[test]
    fn config_relative_root_resolves_against_config_directory() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(base.join("agents")).unwrap();
        let config = base.join(".loti.conf");
        std::fs::write(&config, "agent-root = \"agents\"\n").unwrap();

        let roots = local_roots(&config).unwrap();
        assert_eq!(roots.agents, Some(base.join("agents")));
        assert_eq!(roots.workflows, None);
    }

    #[test]
    fn config_absolute_root_is_taken_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let target = base.join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        let config = base.join(".loti.conf");
        std::fs::write(
            &config,
            format!("workflow-root = \"{}\"\n", target.display()),
        )
        .unwrap();

        let roots = local_roots(&config).unwrap();
        assert_eq!(roots.workflows, Some(target));
    }

    #[test]
    fn config_root_resolves_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let real = base.join("real-agents");
        std::fs::create_dir_all(&real).unwrap();
        let link = base.join("agents-link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let config = base.join(".loti.conf");
        std::fs::write(&config, "agent-root = \"agents-link\"\n").unwrap();

        let roots = local_roots(&config).unwrap();
        assert_eq!(roots.agents, Some(real));
    }

    #[test]
    fn config_root_missing_path_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let config = base.join(".loti.conf");
        std::fs::write(&config, "agent-root = \"does-not-exist\"\n").unwrap();

        let err = local_roots(&config).unwrap_err();
        assert!(matches!(err, ProjectConfigError::InvalidRoot { key, .. } if key == "agent-root"));
    }

    #[test]
    fn config_root_pointing_at_a_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        std::fs::write(base.join("a-file"), "x").unwrap();
        let config = base.join(".loti.conf");
        std::fs::write(&config, "workflow-root = \"a-file\"\n").unwrap();

        let err = local_roots(&config).unwrap_err();
        assert!(matches!(
            err,
            ProjectConfigError::InvalidRoot { key, .. } if key == "workflow-root"
        ));
    }

    #[test]
    fn config_without_either_key_yields_no_local_roots() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let config = base.join(".loti.conf");
        std::fs::write(&config, "loti-root = \".\"\n").unwrap();

        let roots = local_roots(&config).unwrap();
        assert_eq!(roots, LocalRoots::default());
    }
}
