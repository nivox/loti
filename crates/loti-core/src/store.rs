//! The physical store: layout, path helpers, file read/write, and init.
//!
//! Layout is one flat directory per epic under the data root:
//!   * `<epic-id>/epic.md` holds the epic;
//!   * `<epic-id>/<n>.md` holds each node.
//!
//! There are no nested folders — a node's identity is decoupled from its
//! location, so reparenting is a single frontmatter edit, never a move. The
//! tree is encoded solely by the `parent` field.
//!
//! Attachments live in lazily-created companion directories beside their file:
//! `<epic-id>/epic/` for the epic and `<epic-id>/<n>/` for a node.
//!
//! This module provides a plain read/write seam. It does not take the
//! same-directory temp-file lock or perform the atomic rename that guards
//! concurrent edits: those bracket every mutation and are layered on top here.
//! Number allocation is likewise performed elsewhere.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::discovery::CONFIG_FILE;
use crate::meta::{self, Meta, MARKER_DIR};
use crate::model::{EpicFile, ModelError, NodeFile};

/// The epic file name within an epic directory.
pub const EPIC_FILE: &str = "epic.md";

/// The epic's companion directory name for attachments.
pub const EPIC_ASSET_DIR: &str = "epic";

/// A handle to a store rooted at a data-root directory. Cheap to clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
}

/// Failure to read or write through the store.
#[derive(Debug, Error)]
pub enum StoreError {
    /// An I/O operation against a store path failed.
    #[error("accessing {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// A file's frontmatter/body could not be parsed or rendered.
    #[error(transparent)]
    Model(#[from] ModelError),
    /// A store already exists where init was asked to create one.
    #[error("a store already exists at {path}")]
    AlreadyInitialised {
        /// The existing marker directory.
        path: PathBuf,
    },
}

impl Store {
    /// Open a store at an already-resolved data root. The root is taken as-is;
    /// discovery and version gating happen before this.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The data-root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory holding one epic and its nodes.
    pub fn epic_dir(&self, epic_id: &str) -> PathBuf {
        self.root.join(epic_id)
    }

    /// The epic file path.
    pub fn epic_path(&self, epic_id: &str) -> PathBuf {
        self.epic_dir(epic_id).join(EPIC_FILE)
    }

    /// A node file path within an epic.
    pub fn node_path(&self, epic_id: &str, number: u64) -> PathBuf {
        self.epic_dir(epic_id).join(format!("{number}.md"))
    }

    /// The epic's companion attachment directory.
    pub fn epic_asset_dir(&self, epic_id: &str) -> PathBuf {
        self.epic_dir(epic_id).join(EPIC_ASSET_DIR)
    }

    /// A node's companion attachment directory.
    pub fn node_asset_dir(&self, epic_id: &str, number: u64) -> PathBuf {
        self.epic_dir(epic_id).join(number.to_string())
    }

    /// Read and parse an epic file.
    pub fn read_epic(&self, epic_id: &str) -> Result<EpicFile, StoreError> {
        let path = self.epic_path(epic_id);
        let text = read_to_string(&path)?;
        Ok(EpicFile::parse(&text)?)
    }

    /// Read and parse a node file.
    pub fn read_node(&self, epic_id: &str, number: u64) -> Result<NodeFile, StoreError> {
        let path = self.node_path(epic_id, number);
        let text = read_to_string(&path)?;
        Ok(NodeFile::parse(&text)?)
    }

    /// Render and write an epic file, creating its directory if needed.
    ///
    /// This is a plain write. Callers that must guard against concurrent edits
    /// wrap it with the temp-file lock and atomic rename layered on top.
    pub fn write_epic(&self, epic_id: &str, epic: &EpicFile) -> Result<(), StoreError> {
        let dir = self.epic_dir(epic_id);
        create_dir_all(&dir)?;
        let path = self.epic_path(epic_id);
        write_string(&path, &epic.to_text()?)
    }

    /// Render and write a node file, creating its epic directory if needed.
    pub fn write_node(
        &self,
        epic_id: &str,
        number: u64,
        node: &NodeFile,
    ) -> Result<(), StoreError> {
        let dir = self.epic_dir(epic_id);
        create_dir_all(&dir)?;
        let path = self.node_path(epic_id, number);
        write_string(&path, &node.to_text()?)
    }

    /// Copy an asset's bytes into the epic's companion directory verbatim,
    /// creating the directory lazily. Returns the written path. Index upkeep is
    /// the caller's; this only lands the bytes.
    pub fn copy_epic_asset(
        &self,
        epic_id: &str,
        name: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, StoreError> {
        let dir = self.epic_asset_dir(epic_id);
        create_dir_all(&dir)?;
        let path = dir.join(name);
        write_bytes(&path, bytes)?;
        Ok(path)
    }

    /// Copy an asset's bytes into a node's companion directory verbatim.
    pub fn copy_node_asset(
        &self,
        epic_id: &str,
        number: u64,
        name: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, StoreError> {
        let dir = self.node_asset_dir(epic_id, number);
        create_dir_all(&dir)?;
        let path = dir.join(name);
        write_bytes(&path, bytes)?;
        Ok(path)
    }

    /// Hard-remove an epic asset's bytes. Missing is reported so a caller can
    /// keep the index consistent. Index upkeep is the caller's.
    pub fn remove_epic_asset(&self, epic_id: &str, name: &str) -> Result<(), StoreError> {
        remove_file(&self.epic_asset_dir(epic_id).join(name))
    }

    /// Hard-remove a node asset's bytes.
    pub fn remove_node_asset(
        &self,
        epic_id: &str,
        number: u64,
        name: &str,
    ) -> Result<(), StoreError> {
        remove_file(&self.node_asset_dir(epic_id, number).join(name))
    }

    /// Read the store's format metadata.
    pub fn read_meta(&self) -> Result<Meta, meta::MetaError> {
        meta::read(&self.root)
    }
}

/// Where init placed its markers, so a caller can report precisely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    /// The resolved data root that now holds a marker directory and metadata.
    pub root: PathBuf,
    /// The config-file pointer written in the invocation directory, when the
    /// data root is elsewhere; absent for an in-place init.
    pub config_pointer: Option<PathBuf>,
}

/// Initialise a store.
///
/// With no `data_dir`, the marker directory and metadata are created directly
/// under `here`. With a `data_dir`, the marker and metadata are created there
/// and a config-file pointer is written in `here` naming that root.
///
/// Refuses to clobber an existing store: an existing marker directory at the
/// chosen root is an error.
pub fn init(here: &Path, data_dir: Option<&Path>) -> Result<InitOutcome, StoreError> {
    let root = match data_dir {
        Some(d) if d.is_absolute() => d.to_path_buf(),
        Some(d) => here.join(d),
        None => here.to_path_buf(),
    };

    let marker = root.join(MARKER_DIR);
    if marker.exists() {
        return Err(StoreError::AlreadyInitialised { path: marker });
    }

    meta::write(&root, &Meta::current()).map_err(|e| match e {
        meta::MetaError::Io { path, source } => StoreError::Io { path, source },
        // Encoding a freshly-built Meta cannot realistically fail; surface it
        // as an I/O-shaped error against the metadata path rather than panic.
        other => StoreError::Io {
            path: marker.clone(),
            source: std::io::Error::other(other.to_string()),
        },
    })?;

    // Point the invocation directory at a data root that lives elsewhere.
    let config_pointer = match data_dir {
        Some(_) => {
            let pointer = here.join(CONFIG_FILE);
            let root_str = data_dir_config_value(here, &root);
            let body = format!("loti-root = {}\n", toml_string(&root_str));
            write_string(&pointer, &body)?;
            Some(pointer)
        }
        None => None,
    };

    Ok(InitOutcome {
        root,
        config_pointer,
    })
}

/// Whether `dir` sits inside a git working tree but is not that tree's root.
///
/// A store is best kept at the repository root so a whole checkout shares one
/// store; init warns (but does not refuse) when created deeper. Detection is by
/// the presence of a `.git` entry at `dir` versus at an ancestor.
pub fn inside_git_repo_but_not_root(dir: &Path) -> bool {
    let at_root = dir.join(".git").exists();
    if at_root {
        return false;
    }
    dir.ancestors()
        .skip(1)
        .any(|ancestor| ancestor.join(".git").exists())
}

/// Prefer a relative `loti-root` when the data root is under the config's
/// directory, so a moved checkout keeps working; fall back to absolute.
fn data_dir_config_value(config_dir: &Path, root: &Path) -> String {
    match root.strip_prefix(config_dir) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
        _ => root.display().to_string(),
    }
}

/// Quote a value as a TOML basic string, escaping backslashes and quotes.
fn toml_string(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn read_to_string(path: &Path) -> Result<String, StoreError> {
    std::fs::read_to_string(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_string(path: &Path, text: &str) -> Result<(), StoreError> {
    std::fs::write(path, text).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    std::fs::write(path, bytes).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn create_dir_all(path: &Path) -> Result<(), StoreError> {
    std::fs::create_dir_all(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn remove_file(path: &Path) -> Result<(), StoreError> {
    std::fs::remove_file(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attachment, BlockedBy, EpicFrontmatter, NodeFrontmatter};
    use crate::NodeState;
    use jiff::Timestamp;
    use serde_yaml::Mapping;

    fn ts() -> Timestamp {
        "2024-01-01T00:00:00Z".parse().unwrap()
    }

    fn node(number: u64) -> NodeFile {
        NodeFile {
            frontmatter: NodeFrontmatter {
                number,
                name: "n".into(),
                summary: "s".into(),
                status: NodeState::ToDo,
                labels: Vec::new(),
                parent: None,
                blocked_by: BlockedBy::default(),
                close_reason: None,
                attachments: Vec::new(),
                comments: Vec::new(),
                created: ts(),
                updated: ts(),
                extra: Mapping::new(),
            },
            body: "the body\n".into(),
        }
    }

    fn epic() -> EpicFile {
        EpicFile {
            frontmatter: EpicFrontmatter {
                id: "my-epic".into(),
                name: "n".into(),
                summary: "s".into(),
                next_number: 1,
                closed: false,
                close_reason: None,
                labels: Vec::new(),
                attachments: Vec::new(),
                comments: Vec::new(),
                created: ts(),
                updated: ts(),
                extra: Mapping::new(),
            },
            body: String::new(),
        }
    }

    #[test]
    fn path_helpers_follow_the_flat_layout() {
        let store = Store::at("/data");
        assert_eq!(store.epic_path("e"), Path::new("/data/e/epic.md"));
        assert_eq!(store.node_path("e", 7), Path::new("/data/e/7.md"));
        assert_eq!(store.epic_asset_dir("e"), Path::new("/data/e/epic"));
        assert_eq!(store.node_asset_dir("e", 7), Path::new("/data/e/7"));
    }

    #[test]
    fn write_then_read_a_node() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store.write_node("my-epic", 7, &node(7)).unwrap();
        let back = store.read_node("my-epic", 7).unwrap();
        assert_eq!(back, node(7));
    }

    #[test]
    fn write_then_read_an_epic() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store.write_epic("my-epic", &epic()).unwrap();
        let back = store.read_epic("my-epic").unwrap();
        assert_eq!(back, epic());
    }

    #[test]
    fn assets_copy_in_verbatim_to_lazy_companion_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let written = store
            .copy_node_asset("my-epic", 7, "proof.bin", &[0u8, 1, 2, 3])
            .unwrap();
        assert!(written.is_file());
        assert_eq!(std::fs::read(&written).unwrap(), vec![0u8, 1, 2, 3]);
        store.remove_node_asset("my-epic", 7, "proof.bin").unwrap();
        assert!(!written.exists());
    }

    #[test]
    fn attachment_index_and_bytes_are_managed_separately() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let mut e = epic();
        store.copy_epic_asset("my-epic", "a.txt", b"hello").unwrap();
        e.frontmatter.attachments.push(Attachment {
            name: "a.txt".into(),
            description: Some("greeting".into()),
        });
        store.write_epic("my-epic", &e).unwrap();
        let back = store.read_epic("my-epic").unwrap();
        assert_eq!(back.frontmatter.attachments.len(), 1);
        assert!(store.epic_asset_dir("my-epic").join("a.txt").is_file());
    }

    #[test]
    fn init_in_place_creates_marker_and_meta() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = init(dir.path(), None).unwrap();
        assert_eq!(outcome.root, dir.path());
        assert!(outcome.config_pointer.is_none());
        assert!(dir.path().join(MARKER_DIR).join("meta").is_file());
        let store = Store::at(&outcome.root);
        assert_eq!(store.read_meta().unwrap(), Meta::current());
    }

    #[test]
    fn init_with_data_dir_writes_a_config_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = init(dir.path(), Some(Path::new("store"))).unwrap();
        assert_eq!(outcome.root, dir.path().join("store"));
        let pointer = outcome.config_pointer.expect("expected a config pointer");
        assert_eq!(pointer, dir.path().join(CONFIG_FILE));
        let body = std::fs::read_to_string(&pointer).unwrap();
        assert!(body.contains("loti-root = \"store\""));
        assert!(dir
            .path()
            .join("store")
            .join(MARKER_DIR)
            .join("meta")
            .is_file());
    }

    #[test]
    fn git_root_detection() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        // No git anywhere: not inside a repo.
        assert!(!inside_git_repo_but_not_root(base));
        // A repo root: at the root, no warning.
        std::fs::create_dir(base.join(".git")).unwrap();
        assert!(!inside_git_repo_but_not_root(base));
        // A subdirectory of the repo: inside but not at the root.
        let sub = base.join("crate-a");
        std::fs::create_dir(&sub).unwrap();
        assert!(inside_git_repo_but_not_root(&sub));
    }

    #[test]
    fn init_refuses_to_clobber_an_existing_store() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path(), None).unwrap();
        assert!(matches!(
            init(dir.path(), None),
            Err(StoreError::AlreadyInitialised { .. })
        ));
    }
}
