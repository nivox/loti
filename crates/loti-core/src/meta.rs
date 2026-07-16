//! Store metadata: the `meta` file inside the data-root marker directory.
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

/// The marker directory holding store metadata, relative to the data root.
pub const MARKER_DIR: &str = ".loti";

/// The metadata file name inside the marker directory.
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

impl Meta {
    /// Metadata carrying the version this binary writes for a fresh store.
    pub fn current() -> Self {
        let (major, minor) = FORMAT_VERSION;
        Self {
            format_version: format!("{major}.{minor}"),
        }
    }

    /// Parse `"major.minor"` into its numeric parts, ignoring surrounding
    /// whitespace. Returns `None` when the shape is not two dotted integers.
    pub fn parsed_version(&self) -> Option<(u32, u32)> {
        let (major, minor) = self.format_version.trim().split_once('.')?;
        Some((major.parse().ok()?, minor.parse().ok()?))
    }
}

/// The metadata file path for a given data root.
pub fn meta_path(root: &Path) -> PathBuf {
    root.join(MARKER_DIR).join(META_FILE)
}

/// Read and parse the store metadata under `root`.
pub fn read(root: &Path) -> Result<Meta, MetaError> {
    let path = meta_path(root);
    let text = std::fs::read_to_string(&path).map_err(|source| MetaError::Io {
        path: path.clone(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| MetaError::Parse { path, source })
}

/// Write store metadata under `root`, creating the marker directory if needed.
pub fn write(root: &Path, meta: &Meta) -> Result<(), MetaError> {
    let dir = root.join(MARKER_DIR);
    std::fs::create_dir_all(&dir).map_err(|source| MetaError::Io {
        path: dir.clone(),
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
}
