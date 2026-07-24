//! Store metadata: the `meta` file inside the store container.
//!
//! The container `S` is the only directory loti owns: it holds `meta` at
//! `S/meta` and every epic directory directly under it. Metadata is at the
//! container's top level, not inside a nested marker directory.
//!
//! The store carries exactly one format version, at store granularity, written
//! when the store is created. The version is `major.minor`:
//!   * a store major newer than the binary is refused outright;
//!   * a store major older than the binary is read-only until migrated;
//!   * minor differences within a major stay compatible in both directions.
//!
//! This module only reads and writes the version. Enforcing the mismatch gates
//! and running migration are done where a store is opened for mutation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::FORMAT_VERSION;

/// The default container directory name used by discovery and `init` when no
/// explicit container is chosen. The container itself is the store root; this
/// name is what an in-place store is called (`<here>/.loti`).
pub const MARKER_DIR: &str = ".loti";

/// The metadata file name at the container's top level.
pub const META_FILE: &str = "meta";

/// On-disk store metadata (TOML).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// The store format version as a `"major.minor"` string.
    #[serde(rename = "format-version")]
    pub format_version: String,
}

/// Failure to read or write store metadata.
#[derive(Debug, Error)]
pub enum MetaError {
    /// The metadata file could not be read or written.
    #[error("accessing store metadata at {path}: {source}")]
    Io {
        /// The metadata path involved.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The metadata file was present but not parseable TOML.
    #[error("store metadata at {path} is malformed: {source}")]
    Parse {
        /// The metadata path involved.
        path: PathBuf,
        /// The underlying parse error.
        source: toml::de::Error,
    },
    /// Serialising the metadata to TOML failed.
    #[error("encoding store metadata: {0}")]
    Encode(#[from] toml::ser::Error),
}

/// The suffix that marks a version string as a mid-migration sentinel. A store
/// whose recorded version carries this suffix is being migrated (or a migration
/// died holding it): it is read-only for every binary except the migrator, and
/// the suffix doubles as a crash dirty-marker until the migration commits.
const SENTINEL_SUFFIX: &str = "-migrate";

/// The version a store records, as understood by the version rules.
///
/// A store either records a clean `major.minor` it is settled at, or a
/// `major.minor-migrate` sentinel meaning a migration to that version is in
/// flight. The sentinel is written first and cleared last, so observing it
/// always means "not settled" — either live or crashed mid-migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreVersion {
    /// A settled store at this `(major, minor)`.
    Clean {
        /// Major component.
        major: u32,
        /// Minor component.
        minor: u32,
    },
    /// A store mid-migration toward this `(major, minor)` (the sentinel).
    Migrating {
        /// Target major once the migration commits.
        major: u32,
        /// Target minor once the migration commits.
        minor: u32,
    },
}

impl StoreVersion {
    /// The target `(major, minor)` regardless of settled/migrating state.
    pub fn version(self) -> (u32, u32) {
        match self {
            StoreVersion::Clean { major, minor } | StoreVersion::Migrating { major, minor } => {
                (major, minor)
            }
        }
    }

    /// Whether this is the mid-migration sentinel (also the crash dirty-marker).
    pub fn is_migrating(self) -> bool {
        matches!(self, StoreVersion::Migrating { .. })
    }
}

impl Meta {
    /// Metadata carrying the version this binary writes for a fresh store.
    pub fn current() -> Self {
        let (major, minor) = FORMAT_VERSION;
        Self {
            format_version: format!("{major}.{minor}"),
        }
    }

    /// Metadata carrying a settled `major.minor` version.
    pub fn clean(major: u32, minor: u32) -> Self {
        Self {
            format_version: format!("{major}.{minor}"),
        }
    }

    /// Metadata carrying the mid-migration sentinel for target `major.minor`.
    /// Written as the first step of a migration and cleared last; its presence
    /// is the dirty-marker that keeps the store read-only until the migration
    /// commits.
    pub fn migrating(major: u32, minor: u32) -> Self {
        Self {
            format_version: format!("{major}.{minor}{SENTINEL_SUFFIX}"),
        }
    }

    /// Parse `"major.minor"` into its numeric parts, ignoring surrounding
    /// whitespace. Returns `None` when the shape is not two dotted integers, or
    /// when the string is the mid-migration sentinel (use [`Meta::store_version`]
    /// to distinguish the two).
    pub fn parsed_version(&self) -> Option<(u32, u32)> {
        match self.store_version()? {
            StoreVersion::Clean { major, minor } => Some((major, minor)),
            StoreVersion::Migrating { .. } => None,
        }
    }

    /// Parse the recorded version into a [`StoreVersion`], distinguishing a
    /// settled store from one carrying the mid-migration sentinel. Returns
    /// `None` when the string is neither a clean `major.minor` nor a
    /// `major.minor-migrate` sentinel.
    pub fn store_version(&self) -> Option<StoreVersion> {
        let raw = self.format_version.trim();
        let (core, migrating) = match raw.strip_suffix(SENTINEL_SUFFIX) {
            Some(core) => (core, true),
            None => (raw, false),
        };
        let (major, minor) = core.split_once('.')?;
        let major = major.parse().ok()?;
        let minor = minor.parse().ok()?;
        Some(if migrating {
            StoreVersion::Migrating { major, minor }
        } else {
            StoreVersion::Clean { major, minor }
        })
    }
}

/// The metadata file path for a store container: `meta` at the container's top
/// level (the container is the store root loti owns).
pub fn meta_path(root: &Path) -> PathBuf {
    root.join(META_FILE)
}

/// Read and parse the store metadata under the container `root`.
pub fn read(root: &Path) -> Result<Meta, MetaError> {
    let path = meta_path(root);
    let text = std::fs::read_to_string(&path).map_err(|source| MetaError::Io {
        path: path.clone(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| MetaError::Parse { path, source })
}

/// Write store metadata under the container `root`, creating the container if
/// needed.
pub fn write(root: &Path, meta: &Meta) -> Result<(), MetaError> {
    std::fs::create_dir_all(root).map_err(|source| MetaError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let path = meta_path(root);
    let text = toml::to_string(meta)?;
    std::fs::write(&path, text).map_err(|source| MetaError::Io { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_matches_the_pinned_version() {
        let (major, minor) = FORMAT_VERSION;
        assert_eq!(Meta::current().parsed_version(), Some((major, minor)));
    }

    #[test]
    fn parses_dotted_version() {
        let m = Meta {
            format_version: "3.14".into(),
        };
        assert_eq!(m.parsed_version(), Some((3, 14)));
    }

    #[test]
    fn rejects_non_numeric_version() {
        let m = Meta {
            format_version: "not-a-version".into(),
        };
        assert_eq!(m.parsed_version(), None);
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &Meta::current()).unwrap();
        assert!(meta_path(dir.path()).is_file());
        let back = read(dir.path()).unwrap();
        assert_eq!(back, Meta::current());
    }

    #[test]
    fn read_missing_meta_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(read(dir.path()), Err(MetaError::Io { .. })));
    }

    #[test]
    fn clean_version_parses_as_clean() {
        let m = Meta::clean(1, 4);
        assert_eq!(
            m.store_version(),
            Some(StoreVersion::Clean { major: 1, minor: 4 })
        );
        assert!(!m.store_version().unwrap().is_migrating());
        assert_eq!(m.parsed_version(), Some((1, 4)));
    }

    #[test]
    fn sentinel_version_parses_as_migrating_and_hides_from_parsed_version() {
        let m = Meta::migrating(2, 0);
        assert_eq!(m.format_version, "2.0-migrate");
        assert_eq!(
            m.store_version(),
            Some(StoreVersion::Migrating { major: 2, minor: 0 })
        );
        assert!(m.store_version().unwrap().is_migrating());
        assert_eq!(m.store_version().unwrap().version(), (2, 0));
        // parsed_version deliberately refuses the sentinel so callers using the
        // plain numeric path cannot mistake a mid-migration store for settled.
        assert_eq!(m.parsed_version(), None);
    }

    #[test]
    fn garbage_version_parses_as_none_both_ways() {
        let m = Meta {
            format_version: "garbage-migrate".into(),
        };
        assert_eq!(m.store_version(), None);
        assert_eq!(m.parsed_version(), None);
    }
}
