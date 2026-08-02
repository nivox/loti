//! Locating the store container.
//!
//! The container `S` is the only directory loti owns — it holds `meta` and all
//! epic dirs. It is found by walking upward from a starting directory to the
//! nearest ancestor that carries either a `.loti` directory (that directory
//! *is* the container) or a config file naming the container. Rules:
//!
//!   * an explicit override wins outright — no walk, no environment variable;
//!   * at a single level, the config file wins over the `.loti` directory; if
//!     the two name different containers, the disagreement is reported so a
//!     caller can warn;
//!   * the first level that resolves a container ends the walk.
//!
//! The config file is TOML with a `loti-root` key naming the container
//! directly (absolute, or relative to the config file). It may also carry
//! `[match-impl.<name>]` tables; those are parsed and preserved but not
//! interpreted here.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::meta::MARKER_DIR;

/// The config-file name that points at (and configures) a data root.
pub const CONFIG_FILE: &str = ".loti.conf";

/// The `loti-root` key inside the config file.
#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(rename = "loti-root")]
    loti_root: Option<String>,
    /// External matcher command templates, keyed by impl name. Preserved for a
    /// later consumer; not interpreted during discovery.
    #[serde(rename = "match-impl", default)]
    #[allow(dead_code)]
    match_impl: toml::Table,
}

/// A resolved store container and its project directory, plus whether discovery
/// saw the two markers disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// The resolved store container directory.
    pub root: PathBuf,
    /// The directory whose marker selected this store. Agent launches begin here
    /// by default; it remains distinct from the store container for `.loti` and
    /// external `.loti.conf` stores.
    pub project_root: PathBuf,
    /// Set when a single level held both a config file and a `.loti` directory
    /// that named different containers; the config file's container was taken.
    /// A caller should warn the user.
    pub disagreement: Option<Disagreement>,
}

/// The two containers that a single level disagreed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disagreement {
    /// The container taken, from the config file.
    pub config_root: PathBuf,
    /// The container implied by the sibling `.loti` directory.
    pub marker_root: PathBuf,
}

/// Failure to locate a data root.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// No `.loti` directory or config file was found walking upward.
    #[error(
        "no store found here or in any parent directory; \
         run 'loti init' to create one, or pass --root"
    )]
    NotFound,
    /// A config file was found but could not be read or parsed.
    #[error("config file at {path} is malformed: {message}")]
    BadConfig {
        /// The config file path.
        path: PathBuf,
        /// A human-readable explanation.
        message: String,
    },
    /// A config file was found but did not name a root.
    #[error("config file at {path} is missing the 'loti-root' key")]
    MissingRoot {
        /// The config file path.
        path: PathBuf,
    },
}

/// Resolve the store container, preferring an explicit override over discovery.
///
/// When `override_root` is given it is taken as-is (no walk, no validation of
/// its contents here). Otherwise discovery walks upward from `start`.
pub fn resolve(start: &Path, override_root: Option<&Path>) -> Result<Discovered, DiscoveryError> {
    if let Some(root) = override_root {
        return Ok(Discovered {
            root: root.to_path_buf(),
            project_root: root.to_path_buf(),
            disagreement: None,
        });
    }
    discover(start)
}

/// Walk upward from `start`, returning the first level that resolves a
/// container.
pub fn discover(start: &Path) -> Result<Discovered, DiscoveryError> {
    for dir in start.ancestors() {
        let config = dir.join(CONFIG_FILE);
        let marker = dir.join(MARKER_DIR);
        let has_config = config.is_file();
        let has_marker = marker.is_dir();

        if has_config {
            // The config file wins at this level; the `.loti` container (if any)
            // is only consulted to surface a disagreement. Both branches name a
            // container, so the comparison is container-to-container.
            let config_root = read_config_root(&config)?;
            let marker_root = dir.join(MARKER_DIR);
            let disagreement = if has_marker && config_root != marker_root {
                Some(Disagreement {
                    config_root: config_root.clone(),
                    marker_root,
                })
            } else {
                None
            };
            return Ok(Discovered {
                root: config_root,
                project_root: dir.to_path_buf(),
                disagreement,
            });
        }

        if has_marker {
            // The `.loti` directory *is* the container: it holds `meta` and all
            // epic dirs. Return it directly, not its parent.
            return Ok(Discovered {
                root: marker,
                project_root: dir.to_path_buf(),
                disagreement: None,
            });
        }
    }
    Err(DiscoveryError::NotFound)
}

/// Locate the nearest project config file walking upward from `start`, if any.
/// This is the `.loti.conf` that configures the project (including its external
/// matcher tables); it may sit above the data root. Returns `None` when no
/// config file is found on the walk.
pub fn find_project_config(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let config = dir.join(CONFIG_FILE);
        if config.is_file() {
            return Some(config);
        }
    }
    None
}

/// Read a config file and resolve its `loti-root` — the container path
/// (absolute, or relative to the config file's directory).
fn read_config_root(config_path: &Path) -> Result<PathBuf, DiscoveryError> {
    let text = std::fs::read_to_string(config_path).map_err(|e| DiscoveryError::BadConfig {
        path: config_path.to_path_buf(),
        message: e.to_string(),
    })?;
    let config: Config = toml::from_str(&text).map_err(|e| DiscoveryError::BadConfig {
        path: config_path.to_path_buf(),
        message: e.to_string(),
    })?;
    let raw = config
        .loti_root
        .ok_or_else(|| DiscoveryError::MissingRoot {
            path: config_path.to_path_buf(),
        })?;

    Ok(resolve_relative_to_config(config_path, &raw))
}

/// Resolve a config-file string value against the config file's own directory:
/// an absolute value is taken as-is, a relative one is anchored at the config
/// file's parent directory. No shell expansion is performed. Shared by every
/// config key that names a path this way (`loti-root`, and the agent/workflow
/// resource roots), so they cannot disagree on what "relative" means.
pub(crate) fn resolve_relative_to_config(config_path: &Path, raw: &str) -> PathBuf {
    let value = PathBuf::from(raw);
    if value.is_absolute() {
        value
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_marker(root: &Path) {
        std::fs::create_dir_all(root.join(MARKER_DIR)).unwrap();
    }

    #[test]
    fn explicit_root_is_both_container_and_project_directory() {
        let dir = tempfile::tempdir().unwrap();
        let forced = dir.path().join("elsewhere");
        let found = resolve(dir.path(), Some(&forced)).unwrap();
        assert_eq!(found.root, forced);
        assert_eq!(found.project_root, forced);
        assert!(found.disagreement.is_none());
    }

    #[test]
    fn finds_marker_directory_upward_and_returns_the_container() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        touch_marker(&base);
        let nested = base.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let found = discover(&nested).unwrap();
        // The `.loti` directory *is* the container, not its parent.
        assert_eq!(found.root, base.join(MARKER_DIR));
        assert_eq!(found.project_root, base);
    }

    #[test]
    fn config_names_a_relative_root() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        std::fs::write(base.join(CONFIG_FILE), "loti-root = \"data\"\n").unwrap();
        let found = discover(&base).unwrap();
        assert_eq!(found.root, base.join("data"));
        assert_eq!(found.project_root, base);
    }

    #[test]
    fn config_names_an_absolute_root() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let target = base.join("abs-root");
        std::fs::write(
            base.join(CONFIG_FILE),
            format!("loti-root = \"{}\"\n", target.display()),
        )
        .unwrap();
        let found = discover(&base).unwrap();
        assert_eq!(found.root, target);
        assert_eq!(found.project_root, base);
    }

    #[test]
    fn config_wins_over_marker_at_the_same_level_and_flags_disagreement() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        touch_marker(&base);
        std::fs::write(base.join(CONFIG_FILE), "loti-root = \"data\"\n").unwrap();
        let found = discover(&base).unwrap();
        // The config file's container is taken.
        assert_eq!(found.root, base.join("data"));
        assert_eq!(found.project_root, base);
        // And the disagreement with the sibling `.loti` container is reported;
        // the marker container is the `.loti` directory itself.
        let disagreement = found.disagreement.expect("expected a disagreement");
        assert_eq!(disagreement.config_root, base.join("data"));
        assert_eq!(disagreement.marker_root, base.join(MARKER_DIR));
    }

    #[test]
    fn config_pointing_at_the_marker_container_does_not_disagree() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        touch_marker(&base);
        // The config names the same `.loti` container the marker branch implies.
        std::fs::write(
            base.join(CONFIG_FILE),
            format!("loti-root = \"{}\"\n", base.join(MARKER_DIR).display()),
        )
        .unwrap();
        let found = discover(&base).unwrap();
        assert_eq!(found.root, base.join(MARKER_DIR));
        assert!(found.disagreement.is_none());
    }

    #[test]
    fn match_impl_tables_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        std::fs::write(
            base.join(CONFIG_FILE),
            "loti-root = \"data\"\n\
             [match-impl.ripgrep]\n\
             command = [\"rg\", \"<QUERY>\", \"<CANDIDATES>\"]\n",
        )
        .unwrap();
        let found = discover(&base).unwrap();
        assert_eq!(found.root, base.join("data"));
    }

    #[test]
    fn missing_root_key_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        std::fs::write(base.join(CONFIG_FILE), "other = 1\n").unwrap();
        assert!(matches!(
            discover(&base),
            Err(DiscoveryError::MissingRoot { .. })
        ));
    }

    #[test]
    fn nothing_found_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        assert!(matches!(discover(&base), Err(DiscoveryError::NotFound)));
    }
}
