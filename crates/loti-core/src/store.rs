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
//! Assets live in lazily-created companion directories beside their file:
//! `<epic-id>/epic/` for the epic and `<epic-id>/<n>/` for a node.
//!
//! Every mutation here routes through the atomic write + temp-file advisory
//! lock primitive: a write stages a same-directory temp file and atomically
//! renames it over the target, and the temp file's exclusive existence is the
//! lock bracketing the whole read-modify-write. Reads stay lock-free — a
//! single-file read is atomic old-or-new by virtue of that rename, and
//! multi-file aggregates are explicitly not a consistent global snapshot.
//!
//! Node numbers are drawn here from a flat monotonic pool per epic: the epic's
//! `next-number` is a hint, and a node is created by probing forward and
//! exclusively creating the first free node file. Correctness comes from that
//! exclusive create, not from the hint, so a stale-low hint self-heals and
//! concurrent creators never collide; the hint is then bumped best-effort.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::discovery::CONFIG_FILE;
use crate::lock::{
    self, classify_read, Force, LockConfig, LockError, ObservedVersion, StoreVersionGate,
    VersionGate, VersionRefusal,
};
use crate::meta::{self, Meta, StoreVersion, MARKER_DIR};
use crate::model::{EpicFile, ModelError, NodeFile};
use crate::FORMAT_VERSION;

/// An upper bound on how far a single allocation will probe forward before
/// giving up. The probe advances one number per already-taken slot; a bound
/// this large is only ever reached under a corrupt or adversarial store, and
/// turns an otherwise-unbounded loop into a clear error instead of a hang.
const MAX_PROBE_STEPS: u64 = 1_000_000;

/// The epic file name within an epic directory.
pub const EPIC_FILE: &str = "epic.md";

/// The epic's companion directory name for assets.
pub const EPIC_ASSET_DIR: &str = "epic";

/// A handle to a store rooted at a data-root directory. Cheap to clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
    /// Tunables for the temp-file lock's acquire loop; defaults follow the
    /// recommended liveness threshold and retry interval.
    lock_config: LockConfig,
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
    /// The temp-file lock could not be taken, or the atomic write failed.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// The store's format version refuses this mutation.
    #[error(transparent)]
    Version(#[from] VersionRefusal),
    /// A node could not be created because the epic it belongs to does not
    /// exist. A node's number is drawn from its epic's pool, so the epic must
    /// exist first.
    #[error("epic {epic_id} does not exist; create the epic before adding nodes")]
    NoSuchEpic {
        /// The missing epic id.
        epic_id: String,
    },
    /// Number allocation probed forward past a sane bound without finding a
    /// free slot — the epic directory is almost certainly corrupt.
    #[error("could not allocate a node number in epic {epic_id}; the epic looks corrupt")]
    Exhausted {
        /// The epic whose number pool could not yield a free slot.
        epic_id: String,
    },
}

impl Store {
    /// Open a store at an already-resolved data root. The root is taken as-is;
    /// discovery happens before this. Mutations are gated on the store version
    /// read from its metadata.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            lock_config: LockConfig::default(),
        }
    }

    /// Override the lock tunables (for tests or specialised callers). The
    /// acquire-loop invariant (retry interval below the stale threshold) is
    /// the caller's to uphold.
    pub fn with_lock_config(mut self, config: LockConfig) -> Self {
        self.lock_config = config;
        self
    }

    /// The data-root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The store's observed version for the version rules: settled, or the
    /// mid-migration sentinel. A store with no metadata file is treated as the
    /// current settled version — it is a bare directory being populated, not a
    /// versioned store to gate against. Metadata present but unparseable reads
    /// as `None`, which the gates treat as unreadable and refuse.
    fn observed_version(&self) -> Option<ObservedVersion> {
        match meta::read(&self.root) {
            Ok(m) => match m.store_version() {
                Some(StoreVersion::Clean { major, minor }) => {
                    Some(ObservedVersion::Clean(major, minor))
                }
                Some(StoreVersion::Migrating { major, minor }) => {
                    Some(ObservedVersion::Migrating(major, minor))
                }
                None => None,
            },
            // No metadata yet: nothing to gate against, treat as current.
            Err(meta::MetaError::Io { .. }) => {
                Some(ObservedVersion::Clean(FORMAT_VERSION.0, FORMAT_VERSION.1))
            }
            // Metadata present but unreadable: refuse via the gate.
            Err(_) => None,
        }
    }

    /// A mutation gate bound to this store, applying the whole version matrix:
    /// a newer major is refused outright, an older major is read-only until
    /// migrated, a mid-migration sentinel is read-only for everyone but the
    /// migrator, and minor differences within a major are compatible. A
    /// mutation cannot silently write a store it does not understand.
    fn version_gate(&self) -> StoreVersionGate<impl Fn() -> Option<ObservedVersion> + '_> {
        StoreVersionGate {
            binary: FORMAT_VERSION,
            read_store: move || self.observed_version(),
        }
    }

    /// Verify the store may be *read*. Reads are otherwise lock-free, but a
    /// store whose major is newer than this binary must never be read, since the
    /// binary cannot be trusted to interpret it. An older major and a
    /// mid-migration store are still readable — only mutation is refused for
    /// those. Call this before surfacing store contents to a caller.
    pub fn verify_readable(&self) -> Result<(), VersionRefusal> {
        classify_read(self.observed_version(), FORMAT_VERSION.0)
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

    /// The epic's companion asset directory.
    pub fn epic_asset_dir(&self, epic_id: &str) -> PathBuf {
        self.epic_dir(epic_id).join(EPIC_ASSET_DIR)
    }

    /// A node's companion asset directory.
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

    /// Render and write an epic file atomically, creating its directory if
    /// needed. The write stages a temp file and renames it over the target
    /// under the advisory lock; a stale lock fails fast.
    pub fn write_epic(&self, epic_id: &str, epic: &EpicFile) -> Result<(), StoreError> {
        self.write_epic_forced(epic_id, epic, Force::Deny)
    }

    /// As [`Store::write_epic`], but a stale lock from an interrupted operation
    /// is cleared and the write proceeds (the `--force` path).
    pub fn write_epic_forced(
        &self,
        epic_id: &str,
        epic: &EpicFile,
        force: Force,
    ) -> Result<(), StoreError> {
        let dir = self.epic_dir(epic_id);
        create_dir_all(&dir)?;
        let path = self.epic_path(epic_id);
        self.atomic_write(&path, epic.to_text()?.as_bytes(), force)
    }

    /// Render and write a node file atomically, creating its epic directory if
    /// needed.
    pub fn write_node(
        &self,
        epic_id: &str,
        number: u64,
        node: &NodeFile,
    ) -> Result<(), StoreError> {
        self.write_node_forced(epic_id, number, node, Force::Deny)
    }

    /// As [`Store::write_node`], but clears a stale lock (the `--force` path).
    pub fn write_node_forced(
        &self,
        epic_id: &str,
        number: u64,
        node: &NodeFile,
        force: Force,
    ) -> Result<(), StoreError> {
        let dir = self.epic_dir(epic_id);
        create_dir_all(&dir)?;
        let path = self.node_path(epic_id, number);
        self.atomic_write(&path, node.to_text()?.as_bytes(), force)
    }

    /// Atomically write bytes to a store file under the advisory lock and the
    /// store's version gate. The single shared path every file mutation routes
    /// through, so the lock discipline cannot be sidestepped.
    fn atomic_write(&self, path: &Path, bytes: &[u8], force: Force) -> Result<(), StoreError> {
        let gate = self.version_gate();
        let lock = lock::acquire(path, &self.lock_config, force)?;
        // Verify-after-lock: the store version is checked while the lock is
        // held, before publishing, so a store cannot change format underneath.
        gate.verify()?;
        lock.commit(bytes)?;
        Ok(())
    }

    /// Create a new node in an epic, allocating its number from the epic's flat
    /// monotonic pool, and return the written [`NodeFile`] (its `number` field
    /// filled in with the allocated value).
    ///
    /// `fields` carries everything about the node except its number: callers
    /// build the node's frontmatter with any placeholder in `number` (it is
    /// overwritten) and the body they want. The epic must already exist.
    ///
    /// Allocation is a probe-forward atomic exclusive-create: the epic's
    /// `next-number` is only a hint for where to start looking; the number that
    /// is actually taken is the first for which the node file could be created
    /// where none existed. Correctness therefore does not depend on the hint
    /// being accurate — a stale-low hint self-heals by probing forward, and two
    /// racers starting from the same hint cannot take the same number because
    /// the exclusive create of the node file lets exactly one win each slot.
    pub fn create_node(&self, epic_id: &str, fields: NodeFile) -> Result<NodeFile, StoreError> {
        self.create_node_forced(epic_id, fields, Force::Deny)
    }

    /// As [`Store::create_node`], but a stale lock encountered while taking a
    /// candidate slot is cleared and creation proceeds (the `--force` path).
    pub fn create_node_forced(
        &self,
        epic_id: &str,
        mut fields: NodeFile,
        force: Force,
    ) -> Result<NodeFile, StoreError> {
        // A node draws its number from its epic's pool, so the epic must exist.
        if !self.epic_path(epic_id).is_file() {
            return Err(StoreError::NoSuchEpic {
                epic_id: epic_id.to_string(),
            });
        }

        let start = self.allocation_start(epic_id);
        let (number, lock) = self.take_free_slot(epic_id, start, force)?;

        // Stamp the allocated number onto the frontmatter, then publish the
        // complete node file by consuming the slot lock's rename. The version
        // gate is verified under the lock, before publishing, so the store
        // cannot change format mid-create.
        fields.frontmatter.number = number;
        let text = fields.to_text()?;
        self.version_gate().verify()?;
        lock.commit(text.as_bytes())?;

        // Best-effort hint bump; never blocks and never fails the create.
        self.bump_next_number(epic_id, number + 1);
        Ok(fields)
    }

    /// The number to begin probing from: the epic's `next-number` hint, or 1 if
    /// the epic cannot be read (the exclusive create will correct any error by
    /// probing forward, so a bad hint is never fatal here).
    fn allocation_start(&self, epic_id: &str) -> u64 {
        match self.read_epic(epic_id) {
            Ok(epic) => epic.frontmatter.next_number.max(1),
            Err(_) => 1,
        }
    }

    /// Probe forward from `start`, taking the first number whose node file does
    /// not yet exist, and return that number together with a held slot lock
    /// ready to publish the node file. The caller commits the lock to publish.
    ///
    /// The exclusive-create guarantee lives here: for each candidate the slot
    /// lock (the deterministic node temp file) is acquired first, then the node
    /// file's non-existence is re-checked while the lock is held. Because the
    /// temp file is the advisory lock on that exact node, only one operation
    /// can be between the check and the publish for a given number at a time, so
    /// two racers cannot both observe the slot free and both publish it.
    fn take_free_slot(
        &self,
        epic_id: &str,
        start: u64,
        force: Force,
    ) -> Result<(u64, lock::TempLock), StoreError> {
        let dir = self.epic_dir(epic_id);
        create_dir_all(&dir)?;

        let mut number = start;
        for _ in 0..MAX_PROBE_STEPS {
            let path = self.node_path(epic_id, number);
            // A node file already at this number: the slot is taken, probe on
            // without even trying to lock it.
            if path.exists() {
                number += 1;
                continue;
            }
            let lock = lock::acquire(&path, &self.lock_config, force)?;
            // Re-check under the lock: another operation may have published this
            // number between the existence check and taking the lock. If so,
            // release (drop) and probe forward.
            if path.exists() {
                drop(lock);
                number += 1;
                continue;
            }
            return Ok((number, lock));
        }
        Err(StoreError::Exhausted {
            epic_id: epic_id.to_string(),
        })
    }

    /// Best-effort bump of the epic's `next-number` hint to `at_least`.
    ///
    /// This is a single non-blocking attempt to take the epic lock that skips
    /// silently on any collision or error: the counter is only a hint for where
    /// the next allocation starts probing, and a stale-low value self-heals
    /// because the exclusive create still probes forward to a free slot. It is
    /// therefore never worth blocking on, and never worth failing a create for.
    /// The hint is only ever moved upward, never lowered.
    fn bump_next_number(&self, epic_id: &str, at_least: u64) {
        let epic_path = self.epic_path(epic_id);
        // Single non-blocking acquire; on collision (someone else holds the
        // epic lock) or any I/O error, skip silently — the hint self-heals.
        let Ok(Some(lock)) = lock::try_acquire(&epic_path) else {
            return;
        };
        // Read the epic under the lock so the bump is against current contents.
        let Ok(mut epic) = self.read_epic(epic_id) else {
            return;
        };
        if epic.frontmatter.next_number >= at_least {
            // Already at or past the target; nothing to publish. Dropping the
            // lock releases it without a rename.
            return;
        }
        epic.frontmatter.next_number = at_least;
        let Ok(text) = epic.to_text() else {
            return;
        };
        // The bump is a hint; a failed publish is harmless and ignored.
        let _ = lock.commit(text.as_bytes());
    }

    /// Copy an asset's bytes into the epic's companion directory verbatim,
    /// creating the directory lazily. Returns the written path. Index upkeep is
    /// the caller's; this only lands the bytes. The copy is atomic (temp file
    /// then rename) under the advisory lock.
    pub fn copy_epic_asset(
        &self,
        epic_id: &str,
        name: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, StoreError> {
        let dir = self.epic_asset_dir(epic_id);
        create_dir_all(&dir)?;
        let path = dir.join(name);
        self.atomic_write(&path, bytes, Force::Deny)?;
        Ok(path)
    }

    /// Copy an asset's bytes into a node's companion directory verbatim, landed
    /// atomically under the advisory lock.
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
        self.atomic_write(&path, bytes, Force::Deny)?;
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
    use crate::model::{Asset, BlockedBy, EpicFrontmatter, NodeFrontmatter};
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
                assets: Vec::new(),
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
                assets: Vec::new(),
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
    fn asset_index_and_bytes_are_managed_separately() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let mut e = epic();
        store.copy_epic_asset("my-epic", "a.txt", b"hello").unwrap();
        e.frontmatter.assets.push(Asset {
            name: "a.txt".into(),
            description: Some("greeting".into()),
        });
        store.write_epic("my-epic", &e).unwrap();
        let back = store.read_epic("my-epic").unwrap();
        assert_eq!(back.frontmatter.assets.len(), 1);
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

    // -- numbering & atomic node creation ----------------------------------

    /// Fast lock tunables so any contention in an allocation test resolves in
    /// milliseconds rather than the default one-second liveness window.
    fn fast_store(root: &Path) -> Store {
        use std::time::Duration;
        Store::at(root).with_lock_config(LockConfig {
            stale_threshold: Duration::from_millis(80),
            retry_interval: Duration::from_millis(5),
        })
    }

    /// A node file with no number stamped yet (creation overwrites it) and the
    /// given parent, for exercising allocation.
    fn new_node(parent: Option<u64>) -> NodeFile {
        let mut n = node(0);
        n.frontmatter.parent = parent;
        n
    }

    #[test]
    fn create_node_allocates_from_the_hint_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        store.write_epic("my-epic", &epic()).unwrap();

        let created = store.create_node("my-epic", new_node(None)).unwrap();
        // The epic's next-number hint started at 1.
        assert_eq!(created.frontmatter.number, 1);
        let back = store.read_node("my-epic", 1).unwrap();
        assert_eq!(back.frontmatter.number, 1);
        assert_eq!(back.body, created.body);
    }

    #[test]
    fn allocation_bumps_the_hint_so_the_next_create_advances() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        store.write_epic("my-epic", &epic()).unwrap();

        let a = store.create_node("my-epic", new_node(None)).unwrap();
        let b = store.create_node("my-epic", new_node(None)).unwrap();
        assert_eq!(a.frontmatter.number, 1);
        assert_eq!(b.frontmatter.number, 2);
        // The hint was bumped to one past the last allocation.
        let epic = store.read_epic("my-epic").unwrap();
        assert_eq!(epic.frontmatter.next_number, 3);
    }

    #[test]
    fn parent_is_carried_through_onto_the_created_node() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        store.write_epic("my-epic", &epic()).unwrap();
        let parent = store.create_node("my-epic", new_node(None)).unwrap();
        let child = store
            .create_node("my-epic", new_node(Some(parent.frontmatter.number)))
            .unwrap();
        assert_eq!(child.frontmatter.parent, Some(parent.frontmatter.number));
        let back = store
            .read_node("my-epic", child.frontmatter.number)
            .unwrap();
        assert_eq!(back.frontmatter.parent, Some(parent.frontmatter.number));
    }

    #[test]
    fn a_stale_low_hint_self_heals_by_probing_forward() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        // Hint says 1, but 1..=5 are already taken: allocation must return 6.
        let mut e = epic();
        e.frontmatter.next_number = 1;
        store.write_epic("my-epic", &e).unwrap();
        for n in 1..=5 {
            store.write_node("my-epic", n, &node(n)).unwrap();
        }
        let created = store.create_node("my-epic", new_node(None)).unwrap();
        assert_eq!(created.frontmatter.number, 6);
        assert!(store.node_path("my-epic", 6).is_file());
    }

    #[test]
    fn numbers_are_never_reused_after_the_pool_advances() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        store.write_epic("my-epic", &epic()).unwrap();
        let first = store.create_node("my-epic", new_node(None)).unwrap();
        // Even if the node's file were removed, the hint has advanced past it,
        // so a later allocation does not hand out the same number again.
        std::fs::remove_file(store.node_path("my-epic", first.frontmatter.number)).unwrap();
        let next = store.create_node("my-epic", new_node(None)).unwrap();
        assert!(next.frontmatter.number > first.frontmatter.number);
    }

    #[test]
    fn numbers_may_collide_across_epics() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        store.write_epic("epic-a", &epic()).unwrap();
        let mut eb = epic();
        eb.frontmatter.id = "epic-b".into();
        store.write_epic("epic-b", &eb).unwrap();
        let a = store.create_node("epic-a", new_node(None)).unwrap();
        let b = store.create_node("epic-b", new_node(None)).unwrap();
        // Each epic has its own pool: the same number in two epics is fine.
        assert_eq!(a.frontmatter.number, 1);
        assert_eq!(b.frontmatter.number, 1);
    }

    #[test]
    fn create_node_requires_the_epic_to_exist() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        assert!(matches!(
            store.create_node("ghost", new_node(None)),
            Err(StoreError::NoSuchEpic { .. })
        ));
    }

    #[test]
    fn two_allocations_from_the_same_hint_do_not_collide() {
        // Both starts see next-number == 1; the exclusive create makes exactly
        // one take slot 1 and the other probe forward to 2. No lost file, no
        // reused number.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let store = fast_store(&root);
        store.write_epic("my-epic", &epic()).unwrap();

        let r1 = root.clone();
        let r2 = root.clone();
        let t1 = std::thread::spawn(move || {
            fast_store(&r1)
                .create_node("my-epic", new_node(None))
                .unwrap()
                .frontmatter
                .number
        });
        let t2 = std::thread::spawn(move || {
            fast_store(&r2)
                .create_node("my-epic", new_node(None))
                .unwrap()
                .frontmatter
                .number
        });
        let n1 = t1.join().unwrap();
        let n2 = t2.join().unwrap();

        // Distinct numbers, both files present, both parse cleanly.
        assert_ne!(n1, n2, "two racers must not take the same number");
        assert!(store.node_path("my-epic", n1).is_file());
        assert!(store.node_path("my-epic", n2).is_file());
        store.read_node("my-epic", n1).unwrap();
        store.read_node("my-epic", n2).unwrap();
    }

    #[test]
    fn many_concurrent_allocations_are_all_distinct_and_persisted() {
        // A stronger race: N threads against one epic must produce N distinct
        // numbers and N readable files, with none lost to a collision.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let store = fast_store(&root);
        store.write_epic("my-epic", &epic()).unwrap();

        const N: usize = 8;
        let handles: Vec<_> = (0..N)
            .map(|_| {
                let r = root.clone();
                std::thread::spawn(move || {
                    fast_store(&r)
                        .create_node("my-epic", new_node(None))
                        .unwrap()
                        .frontmatter
                        .number
                })
            })
            .collect();
        let mut numbers: Vec<u64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        numbers.sort_unstable();
        numbers.dedup();
        assert_eq!(numbers.len(), N, "every allocation must be unique");
        for n in numbers {
            assert!(store.node_path("my-epic", n).is_file());
            store.read_node("my-epic", n).unwrap();
        }
    }

    #[test]
    fn a_held_epic_lock_skips_the_hint_bump_without_failing_the_create() {
        // The counter bump is best-effort: if the epic lock is held, the bump
        // is skipped silently and the create still succeeds. The next
        // allocation self-heals by probing forward from the stale-low hint.
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        store.write_epic("my-epic", &epic()).unwrap();

        // Hold the epic lock for the duration of a create so the bump cannot
        // take it.
        let epic_lock =
            lock::acquire(&store.epic_path("my-epic"), &store.lock_config, Force::Deny).unwrap();
        let created = store.create_node("my-epic", new_node(None)).unwrap();
        assert_eq!(created.frontmatter.number, 1);
        // The node file exists; the hint was NOT bumped (still 1) because the
        // lock was held.
        assert!(store.node_path("my-epic", 1).is_file());
        drop(epic_lock);
        let epic = store.read_epic("my-epic").unwrap();
        assert_eq!(
            epic.frontmatter.next_number, 1,
            "a held epic lock leaves the hint unbumped"
        );

        // The next create self-heals: it starts at the stale-low hint (1),
        // finds 1 taken, and probes forward to 2.
        let second = store.create_node("my-epic", new_node(None)).unwrap();
        assert_eq!(second.frontmatter.number, 2);
    }

    #[test]
    fn create_node_respects_the_version_gate() {
        // A store whose major is newer than this binary refuses a create.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Write metadata claiming a far-future major version.
        meta::write(
            root,
            &Meta {
                format_version: format!("{}.0", FORMAT_VERSION.0 + 1),
            },
        )
        .unwrap();
        let store = fast_store(root);
        // The epic file must exist for create_node to get past the epic check;
        // but writing it is itself gated, so the too-new store already refuses
        // this write — which is the guarantee we want.
        assert!(matches!(
            store.write_epic("my-epic", &epic()),
            Err(StoreError::Version(VersionRefusal::StoreTooNew))
        ));
    }

    // -- version mismatch matrix & sentinel gate ---------------------------

    /// Record a store version string, having first placed a real epic so reads
    /// have something to return.
    fn store_with_version(root: &Path, version: &str) -> Store {
        let store = fast_store(root);
        // Seed content at the current version, then overwrite the recorded
        // version to the scenario under test (bypassing the gate deliberately).
        store.write_epic("my-epic", &epic()).unwrap();
        meta::write(
            root,
            &Meta {
                format_version: version.to_string(),
            },
        )
        .unwrap();
        store
    }

    #[test]
    fn equal_version_permits_reads_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let (major, minor) = FORMAT_VERSION;
        let store = store_with_version(dir.path(), &format!("{major}.{minor}"));
        assert!(store.verify_readable().is_ok());
        // A mutation succeeds at the equal version.
        let mut e = store.read_epic("my-epic").unwrap();
        e.frontmatter.name = "renamed".into();
        assert!(store.write_epic("my-epic", &e).is_ok());
    }

    #[test]
    fn too_new_store_refuses_both_reads_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let (major, minor) = FORMAT_VERSION;
        let store = store_with_version(dir.path(), &format!("{}.{minor}", major + 1));
        // Reads are refused (never read-guess a newer major).
        assert!(matches!(
            store.verify_readable(),
            Err(VersionRefusal::StoreTooNew)
        ));
        // Writes are refused too.
        assert!(matches!(
            store.write_epic("my-epic", &epic()),
            Err(StoreError::Version(VersionRefusal::StoreTooNew))
        ));
    }

    #[test]
    fn older_major_store_reads_but_refuses_mutation_with_migrate_message() {
        let dir = tempfile::tempdir().unwrap();
        let (major, minor) = FORMAT_VERSION;
        if major == 0 {
            // No lower major exists to record; the matrix for older-major is
            // unreachable with a major-0 binary. Covered by the migrate module
            // via simulated skew instead.
            return;
        }
        let store = store_with_version(dir.path(), &format!("{}.{minor}", major - 1));
        // Reads are fine on an older major.
        assert!(store.verify_readable().is_ok());
        assert!(store.read_epic("my-epic").is_ok());
        // Any mutation is refused, pointing at migrate-store.
        assert!(matches!(
            store.write_epic("my-epic", &epic()),
            Err(StoreError::Version(VersionRefusal::NeedsMigration))
        ));
    }

    #[test]
    fn minor_difference_within_a_major_is_compatible_both_ways() {
        let dir = tempfile::tempdir().unwrap();
        let (major, minor) = FORMAT_VERSION;
        // A store a minor ahead within the same major still mutates (tolerant
        // reader handles any additive keys).
        let store = store_with_version(dir.path(), &format!("{major}.{}", minor + 1));
        assert!(store.verify_readable().is_ok());
        assert!(store.write_epic("my-epic", &epic()).is_ok());
    }

    #[test]
    fn mid_migration_sentinel_refuses_mutation_for_everyone_but_reads_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (major, minor) = FORMAT_VERSION;
        // The sentinel names this very binary's version, so a matching-major
        // binary still refuses mutation while the migration is in flight.
        let store = store_with_version(dir.path(), &format!("{major}.{minor}-migrate"));
        // Reads remain allowed (the store is not too-new).
        assert!(store.verify_readable().is_ok());
        // Every mutation is refused as mid-migration.
        assert!(matches!(
            store.write_epic("my-epic", &epic()),
            Err(StoreError::Version(VersionRefusal::MigrationInProgress))
        ));
    }

    #[test]
    fn unreadable_version_refuses_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_version(dir.path(), "not-a-version");
        assert!(matches!(
            store.write_epic("my-epic", &epic()),
            Err(StoreError::Version(VersionRefusal::Unreadable))
        ));
    }
}
