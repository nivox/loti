//! Locating the data root.
//!
//! A store is found by walking upward from a starting directory to the nearest
//! ancestor that carries either a marker directory (whose parent is the data
//! root) or a config file naming the root. Rules:
//!
//!   * an explicit override wins outright — no walk, no environment variable;
//!   * at a single level, the config file wins over the marker directory; if
//!     the two name different roots, the disagreement is reported so a caller
//!     can warn;
//!   * the first level that resolves a root ends the walk.
//!
//! The config file is TOML with a `loti-root` key (absolute, or relative to the
//! config file). It may also carry `[match-impl.<name>]` tables; those are
//! parsed and preserved but not interpreted here.

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

/// A resolved data root, plus whether discovery saw the two markers disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// The resolved data root directory.
    pub root: PathBuf,
    /// Set when a single level held both a config file and a marker directory
    /// that named different roots; the config file's root was taken. A caller
    /// should warn the user.
    pub disagreement: Option<Disagreement>,
}

/// The two roots that a single level disagreed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disagreement {
    /// The root taken, from the config file.
    pub config_root: PathBuf,
    /// The root implied by the sibling marker directory.
    pub marker_root: PathBuf,
}

/// Failure to locate a data root.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// No marker directory or config file was found walking upward.
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

/// Resolve the data root, preferring an explicit override over discovery.
///
/// When `override_root` is given it is taken as-is (no walk, no validation of
/// its contents here). Otherwise discovery walks upward from `start`.
pub fn resolve(start: &Path, override_root: Option<&Path>) -> Result<Discovered, DiscoveryError> {
    if let Some(root) = override_root {
        return Ok(Discovered {
            root: root.to_path_buf(),
            disagreement: None,
        });
    }
    discover(start)
}

/// Walk upward from `start`, returning the first level that resolves a root.
pub fn discover(start: &Path) -> Result<Discovered, DiscoveryError> {
    for dir in start.ancestors() {
        let config = dir.join(CONFIG_FILE);
        let marker = dir.join(MARKER_DIR);
        let has_config = config.is_file();
        let has_marker = marker.is_dir();

        if has_config {
            // The config file wins at this level; the marker root (if any) is
            // only consulted to surface a disagreement.
            let config_root = read_config_root(&config)?;
            let disagreement = if has_marker && config_root != dir {
                Some(Disagreement {
                    config_root: config_root.clone(),
                    marker_root: dir.to_path_buf(),
                })
            } else {
                None
            };
            return Ok(Discovered {
                root: config_root,
                disagreement,
            });
        }

        if has_marker {
            // The marker directory sits inside the data root; the root is its
            // parent — here, the level we are standing on.
            return Ok(Discovered {
                root: dir.to_path_buf(),
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

/// Read a config file and resolve its `loti-root` (absolute, or relative to the
/// config file's directory).
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

    let root = PathBuf::from(&raw);
    let resolved = if root.is_absolute() {
        root
    } else {
        // A relative root is anchored at the config file's own directory.
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(root)
    };
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_marker(root: &Path) {
        std::fs::create_dir_all(root.join(MARKER_DIR)).unwrap();
    }

    #[test]
    fn override_wins_without_walking() {
        let dir = tempfile::tempdir().unwrap();
        let forced = dir.path().join("elsewhere");
        let found = resolve(dir.path(), Some(&forced)).unwrap();
        assert_eq!(found.root, forced);
        assert!(found.disagreement.is_none());
    }

    #[test]
    fn finds_marker_directory_upward() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        touch_marker(&root);
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let found = discover(&nested).unwrap();
        assert_eq!(found.root, root);
    }

    #[test]
    fn config_names_a_relative_root() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        std::fs::write(base.join(CONFIG_FILE), "loti-root = \"data\"\n").unwrap();
        let found = discover(&base).unwrap();
        assert_eq!(found.root, base.join("data"));
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
    }

    #[test]
    fn config_wins_over_marker_at_the_same_level_and_flags_disagreement() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        touch_marker(&base);
        std::fs::write(base.join(CONFIG_FILE), "loti-root = \"data\"\n").unwrap();
        let found = discover(&base).unwrap();
        // The config file's root is taken.
        assert_eq!(found.root, base.join("data"));
        // And the disagreement with the sibling marker is reported.
        let disagreement = found.disagreement.expect("expected a disagreement");
        assert_eq!(disagreement.config_root, base.join("data"));
        assert_eq!(disagreement.marker_root, base);
    }

    #[test]
    fn config_pointing_at_its_own_level_does_not_disagree_with_marker() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        touch_marker(&base);
        std::fs::write(
            base.join(CONFIG_FILE),
            format!("loti-root = \"{}\"\n", base.display()),
        )
        .unwrap();
        let found = discover(&base).unwrap();
        assert_eq!(found.root, base);
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
