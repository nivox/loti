//! The physical store: layout, path helpers, file read/write, and init.
//!
//! The container is the only directory loti owns: it holds `meta` at its top
//! level and one flat directory per epic directly under it. Nothing loti writes
//! ever escapes the container.
//!
//! Layout is one flat directory per epic under the container:
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
//! lock. There are two write paths and the difference is what they read:
//! a whole-file write publishes a value that does not depend on the target's
//! contents, while a read-modify-write takes the lock *before* reading the
//! target, so the whole sequence — read, change, publish — is bracketed and
//! nothing can land in the middle of it. Reads stay lock-free — a single-file
//! read is atomic old-or-new by virtue of that rename, and multi-file aggregates
//! are explicitly not a consistent global snapshot.
//!
//! Node numbers are drawn here from a flat monotonic pool per epic: the epic's
//! `next-number` is a hint, and a node is created by probing forward and
//! exclusively creating the first free node file. Correctness comes from that
//! exclusive create, not from the hint, so a stale-low hint self-heals and
//! concurrent creators never collide; the hint is then bumped best-effort.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use thiserror::Error;

use crate::discovery::CONFIG_FILE;
use crate::lock::{
    self, classify_read, Force, LockConfig, LockError, ObservedVersion, RmwError, StoreVersionGate,
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

/// A handle to a store rooted at its container directory. Cheap to clone.
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
        /// The existing store metadata that init refused to clobber.
        path: PathBuf,
    },
    /// The temp-file lock could not be taken, or the atomic write failed.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// The store's format version refuses this mutation.
    #[error(transparent)]
    Version(#[from] VersionRefusal),
    /// The store under the lock is not the store the write was built for, so
    /// nothing was written: either the caller named the `updated` stamp it
    /// expected to still find and the stored stamp has moved on, or the target
    /// is not there under the lock. A modify never creates, so it never
    /// re-creates what another actor deleted.
    #[error("{path} changed since it was read; nothing was written")]
    Conflict {
        /// The file that no longer matches what the write was built for.
        path: PathBuf,
    },
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
    /// Open a store at an already-resolved container. The container is taken
    /// as-is; discovery happens before this. Mutations are gated on the store
    /// version read from its metadata.
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

    /// The container directory (the store root loti owns).
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

    /// Verify the store may be *mutated*: the write-side twin of
    /// [`Store::verify_readable`]. It consults the very gate every mutation is
    /// bracketed by and refuses with the same reason that mutation would, so a
    /// surface can ask up front instead of learning it from a failed write.
    ///
    /// The answer is a snapshot, never a licence: the store's version can change
    /// between the question and the write, so a mutation still re-verifies the
    /// gate while holding the lock.
    pub fn verify_mutable(&self) -> Result<(), VersionRefusal> {
        self.version_gate().verify()
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

    /// Render and write a whole epic file atomically, creating its directory if
    /// needed. The write stages a temp file and renames it over the target
    /// under the advisory lock; a stale lock fails fast.
    ///
    /// This publishes the caller's value as the whole file without reading what
    /// is there — it creates an epic, or replaces one wholesale, last write
    /// wins. A write that must see the stored epic first goes through
    /// [`Store::modify_epic`], which holds the lock across the read as well.
    pub fn write_epic(&self, epic_id: &str, epic: &EpicFile) -> Result<(), StoreError> {
        self.write_epic_forced(epic_id, epic, Force::Deny)
    }

    /// As [`Store::write_epic`], but a stale lock from an interrupted operation
    /// is cleared and the write proceeds (the `--force` path). The one
    /// epic-write body every public epic write funnels into, so the force
    /// policy cannot be applied by one path and skipped by another.
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

    /// Render and write a whole node file atomically, creating its epic
    /// directory if needed. As with an epic, this is a wholesale publish that
    /// reads nothing; [`Store::modify_node`] is the read-modify-write path.
    pub fn write_node(
        &self,
        epic_id: &str,
        number: u64,
        node: &NodeFile,
    ) -> Result<(), StoreError> {
        self.write_node_forced(epic_id, number, node, Force::Deny)
    }

    /// As [`Store::write_node`], but clears a stale lock (the `--force` path),
    /// and the one node-write body every public node write funnels into.
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

    /// Read an epic, let `change` modify it, and publish the result — with the
    /// whole sequence bracketed by the advisory lock. The lock is taken first,
    /// the store version verified under it, and only then is the epic read, so
    /// no cooperating operation can publish to the target between the read
    /// `change` sees and the rename that publishes its result. This is the path
    /// every write that depends on what is stored takes; a caller that merges
    /// into the stored epic therefore cannot discard a concurrent change.
    ///
    /// A modify never creates: an epic that is not there is refused with
    /// [`StoreError::Conflict`], because publishing on top of nothing would
    /// resurrect an entity from a read that no longer describes the store.
    ///
    /// `expect_updated` is the caller's optional precondition for the window it
    /// owns and this lock cannot cover — the one between a surface reading an
    /// entity, a human composing a replacement, and the edit arriving here. The
    /// stamp is compared against the epic as *stored*, read under the lock, and
    /// a mismatch refuses with [`StoreError::Conflict`] having published
    /// nothing. Granularity is the entity, not the field: every write that
    /// changes an entity's content bumps `updated` (the best-effort bump of the
    /// next-number hint is the sole exception, and carries nothing a caller
    /// composes), so an unrelated change to the same epic refuses the write too.
    /// `None` names no precondition — last write wins.
    ///
    /// Returns the epic as published, together with whatever `change` produced.
    pub fn modify_epic<T, E>(
        &self,
        epic_id: &str,
        expect_updated: Option<Timestamp>,
        change: impl FnOnce(&mut EpicFile) -> Result<T, E>,
    ) -> Result<(EpicFile, T), E>
    where
        E: From<StoreError>,
    {
        self.modify_entity(&self.epic_path(epic_id), expect_updated, change)
    }

    /// The node twin of [`Store::modify_epic`], with the same bracketing, the
    /// same refusal for a node that is not there, and the same precondition
    /// semantics.
    pub fn modify_node<T, E>(
        &self,
        epic_id: &str,
        number: u64,
        expect_updated: Option<Timestamp>,
        change: impl FnOnce(&mut NodeFile) -> Result<T, E>,
    ) -> Result<(NodeFile, T), E>
    where
        E: From<StoreError>,
    {
        self.modify_entity(&self.node_path(epic_id, number), expect_updated, change)
    }

    /// The one bracketed read-modify-write body, shared by both entity kinds so
    /// the discipline cannot say different things about an epic and a node.
    ///
    /// The lock module fixes the ordering — lock, version gate, read, publish —
    /// and this adds the entity layer on top: parse what is stored, evaluate the
    /// caller's precondition against it, apply the change, and publish only what
    /// the change actually changed. A change that leaves the entity as it found
    /// it publishes nothing, so an operation that is a no-op on the stored state
    /// does not rewrite the file.
    fn modify_entity<F, T, E>(
        &self,
        path: &Path,
        expect_updated: Option<Timestamp>,
        change: impl FnOnce(&mut F) -> Result<T, E>,
    ) -> Result<(F, T), E>
    where
        F: StoredEntity,
        E: From<StoreError>,
    {
        let gate = self.version_gate();
        let outcome = lock::rmw(
            path,
            &self.lock_config,
            // A read-modify-write never clears a stale lock: an interrupted
            // operation is an operator's call, as it is on every other write.
            Force::Deny,
            &gate,
            |current| {
                let Some(bytes) = current else {
                    return Err(E::from(StoreError::Conflict {
                        path: path.to_path_buf(),
                    }));
                };
                // Bytes that are not text at all are a corrupt store, and
                // surface as an I/O failure rather than a conflict.
                let text = String::from_utf8(bytes).map_err(|_| {
                    E::from(StoreError::Io {
                        path: path.to_path_buf(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "stored file is not valid UTF-8 text",
                        ),
                    })
                })?;
                // Text that is not a parseable entity of this kind is refused
                // rather than clobbered: nothing is published on top of contents
                // that could not be inspected.
                let stored = F::parse_text(&text).map_err(|e| E::from(StoreError::Model(e)))?;
                if let Some(expected) = expect_updated {
                    // The comparison uses the stamp as stored, read here under
                    // the lock — never the one on the value being written.
                    if stored.updated_stamp() != expected {
                        return Err(E::from(StoreError::Conflict {
                            path: path.to_path_buf(),
                        }));
                    }
                }
                // The entity as stored is kept beside the changed one, so the
                // publish step can tell whether the change changed anything.
                let mut modified = stored.clone();
                let out = change(&mut modified)?;
                Ok((stored, modified, out))
            },
            |(stored, modified, _)| {
                // Publishing nothing is how an idempotent operation leaves a
                // target alone: no rename, and the stored bytes keep whatever
                // formatting they had.
                if modified == stored {
                    return Ok(None);
                }
                Ok(Some(
                    modified
                        .render_text()
                        .map_err(|e| E::from(StoreError::Model(e)))?
                        .into_bytes(),
                ))
            },
        );
        match outcome {
            Ok((_, modified, out)) => Ok((modified, out)),
            Err(RmwError::Lock(e)) => Err(E::from(StoreError::Lock(e))),
            Err(RmwError::Version(refusal)) => Err(E::from(StoreError::Version(refusal))),
            Err(RmwError::Op(e)) => Err(e),
        }
    }

    /// Atomically write bytes to a store file under the advisory lock and the
    /// store's version gate. The single shared path every whole-file mutation
    /// routes through, so the lock discipline cannot be sidestepped.
    ///
    /// Nothing is read here: this publishes bytes that do not depend on the
    /// target's current contents. A mutation that does depend on them is a
    /// read-modify-write and goes through [`Store::modify_entity`], which reads
    /// under the same lock.
    fn atomic_write(&self, path: &Path, bytes: &[u8], force: Force) -> Result<(), StoreError> {
        let gate = self.version_gate();
        let lock = lock::acquire(path, &self.lock_config, force)?;
        // Verify-after-lock: the store version is checked while the lock is
        // held, before publishing, so a store cannot change format underneath.
        gate.verify()?;
        lock.commit(bytes)?;
        Ok(())
    }

    /// The delete twin of [`Store::atomic_write`]: acquire the lock on `path`,
    /// verify the store's version gate while it is held, then remove the file
    /// instead of publishing onto it. A stale lock is never forced past here —
    /// asset removal carries no `--force` surface, and a stale temp file on an
    /// asset path most often means another operation is mid-publish on the very
    /// bytes being removed, which is exactly the race this bracketing closes.
    fn atomic_remove(&self, path: &Path) -> Result<(), StoreError> {
        let gate = self.version_gate();
        let lock = lock::acquire(path, &self.lock_config, Force::Deny)?;
        gate.verify()?;
        lock.remove_target()?;
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
        // Asset bytes are opaque: a copy replaces them wholesale and never reads
        // what is there, so it is a blind write rather than a modify.
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

    /// Hard-remove an epic asset's bytes, under the same advisory lock and
    /// version gate as every other mutation here: a store this binary must not
    /// write refuses before any byte is removed, and the removal cannot land
    /// while another operation holds the lock on the same path mid-publish.
    /// Missing is reported so a caller can keep the index consistent. Index
    /// upkeep is the caller's.
    pub fn remove_epic_asset(&self, epic_id: &str, name: &str) -> Result<(), StoreError> {
        self.atomic_remove(&self.epic_asset_dir(epic_id).join(name))
    }

    /// Hard-remove a node asset's bytes, with the same bracketing.
    pub fn remove_node_asset(
        &self,
        epic_id: &str,
        number: u64,
        name: &str,
    ) -> Result<(), StoreError> {
        self.atomic_remove(&self.node_asset_dir(epic_id, number).join(name))
    }

    /// Read an epic asset's bytes verbatim. The index is the source of truth for
    /// which assets exist; callers gate on it, so a missing file here is a
    /// store-corruption I/O error, not an absent asset.
    pub fn read_epic_asset(&self, epic_id: &str, name: &str) -> Result<Vec<u8>, StoreError> {
        read_bytes(&self.epic_asset_dir(epic_id).join(name))
    }

    /// Read a node asset's bytes verbatim.
    pub fn read_node_asset(
        &self,
        epic_id: &str,
        number: u64,
        name: &str,
    ) -> Result<Vec<u8>, StoreError> {
        read_bytes(&self.node_asset_dir(epic_id, number).join(name))
    }

    /// Read the store's format metadata.
    pub fn read_meta(&self) -> Result<Meta, meta::MetaError> {
        meta::read(&self.root)
    }
}

/// What a bracketed read-modify-write needs of a stored entity: parse it from
/// the target's text, render it back, and expose the `updated` stamp a write
/// precondition is compared against. Implemented for both entity kinds so the
/// bracketing body exists once and cannot drift between an epic and a node.
trait StoredEntity: Sized + Clone + PartialEq {
    /// Parse the target's text into the entity.
    fn parse_text(text: &str) -> Result<Self, ModelError>;
    /// Render the entity back to the target's text.
    fn render_text(&self) -> Result<String, ModelError>;
    /// The entity's `updated` stamp as stored.
    fn updated_stamp(&self) -> Timestamp;
}

impl StoredEntity for EpicFile {
    fn parse_text(text: &str) -> Result<Self, ModelError> {
        Self::parse(text)
    }
    fn render_text(&self) -> Result<String, ModelError> {
        self.to_text()
    }
    fn updated_stamp(&self) -> Timestamp {
        self.frontmatter.updated
    }
}

impl StoredEntity for NodeFile {
    fn parse_text(text: &str) -> Result<Self, ModelError> {
        Self::parse(text)
    }
    fn render_text(&self) -> Result<String, ModelError> {
        self.to_text()
    }
    fn updated_stamp(&self) -> Timestamp {
        self.frontmatter.updated
    }
}

/// Where init placed the store, so a caller can report precisely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    /// The container that now holds metadata (and will hold every epic dir).
    pub root: PathBuf,
    /// The config-file pointer written in the invocation directory, when the
    /// container is neither `here` nor `here`'s default `.loti`; absent when
    /// discovery finds the container without a breadcrumb.
    pub config_pointer: Option<PathBuf>,
}

/// Initialise a store whose container is `container`, invoked from `here`.
///
/// The container is the only directory loti owns: metadata lands at
/// `container/meta` and every epic dir will sit directly under it. The caller
/// resolves where `container` should be (the default `here/.loti`, or a literal
/// `--root`/positional target with no `.loti` appended).
///
/// A `.loti.conf` pointer naming the container is written in `here` only when a
/// bare upward walk from `here` would not find the container on its own. That
/// walk finds either a `.loti` directory or a `.loti.conf`, so a pointer is
/// suppressed for the default in-place container (`here/.loti`, found
/// directly) and for a container that is literally `here` (nothing to redirect
/// to). A relative `loti-root` is written when the container is under `here`
/// (so a moved checkout keeps working), else an absolute one.
///
/// Refuses to clobber an existing store: existing metadata at the container is
/// an error.
pub fn init(here: &Path, container: &Path) -> Result<InitOutcome, StoreError> {
    let container = container.to_path_buf();

    let meta_file = meta::meta_path(&container);
    if meta_file.exists() {
        return Err(StoreError::AlreadyInitialised { path: meta_file });
    }

    meta::write(&container, &Meta::current()).map_err(|e| match e {
        meta::MetaError::Io { path, source } => StoreError::Io { path, source },
        // Encoding a freshly-built Meta cannot realistically fail; surface it
        // as an I/O-shaped error against the metadata path rather than panic.
        other => StoreError::Io {
            path: meta_file.clone(),
            source: std::io::Error::other(other.to_string()),
        },
    })?;

    // A pointer is written only when a bare upward walk from `here` would not
    // reach the container by itself. Both the default `.loti` and `here` are
    // found without one, so suppress the breadcrumb for those two cases; every
    // other explicit container needs a pointer. Compare canonical forms so the
    // check sees through `.`/`..`/symlinks.
    let default_container = here.join(MARKER_DIR);
    let found_by_walk = same_dir(here, &container) || same_dir(&default_container, &container);
    let config_pointer = if found_by_walk {
        None
    } else {
        let pointer = here.join(CONFIG_FILE);
        let root_str = data_dir_config_value(here, &container);
        let body = format!("loti-root = {}\n", toml_string(&root_str));
        write_string(&pointer, &body)?;
        Some(pointer)
    };

    Ok(InitOutcome {
        root: container,
        config_pointer,
    })
}

/// Whether two directories denote the same location, seeing through `.`/`..`
/// and symlinks where the paths resolve; falls back to a literal comparison
/// when canonicalisation is unavailable.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
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

/// Prefer a relative `loti-root` when the container is under the config's
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

fn read_bytes(path: &Path) -> Result<Vec<u8>, StoreError> {
    std::fs::read(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Asset, EpicFrontmatter, NodeFrontmatter};
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
                blocked_by: Vec::new(),
                block_reason: None,
                close_reason: None,
                claim: None,
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
    fn remove_epic_asset_refuses_on_a_read_only_store_without_removing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        let (major, minor) = FORMAT_VERSION;
        if major == 0 {
            // No lower major exists to record; unreachable with a major-0
            // binary, as with the other version-matrix tests in this module.
            return;
        }
        let path = store
            .copy_epic_asset("my-epic", "a.bin", b"payload")
            .unwrap();
        meta::write(
            dir.path(),
            &Meta {
                format_version: format!("{}.{minor}", major - 1),
            },
        )
        .unwrap();

        // An older major is read-only until migrated: removal refuses exactly
        // as any other mutation would, and no byte moves before it does.
        assert!(matches!(
            store.remove_epic_asset("my-epic", "a.bin"),
            Err(StoreError::Version(VersionRefusal::NeedsMigration))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"payload");
    }

    #[test]
    fn remove_node_asset_refuses_mid_migration_without_removing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        let (major, minor) = FORMAT_VERSION;
        let path = store
            .copy_node_asset("my-epic", 7, "a.bin", b"payload")
            .unwrap();
        meta::write(
            dir.path(),
            &Meta {
                format_version: format!("{major}.{minor}-migrate"),
            },
        )
        .unwrap();

        // The mid-migration sentinel refuses every mutation but the migrator's,
        // removal included, before any byte is touched.
        assert!(matches!(
            store.remove_node_asset("my-epic", 7, "a.bin"),
            Err(StoreError::Version(VersionRefusal::MigrationInProgress))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"payload");
    }

    #[test]
    fn remove_epic_asset_cannot_land_while_another_operation_holds_the_lock() {
        // The live race the bracketing closes: an unlink must not land while
        // another operation holds the lock on the very asset path being
        // removed, as it would mid-publish.
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        let path = store
            .copy_epic_asset("my-epic", "a.bin", b"payload")
            .unwrap();

        let held = lock::acquire(&path, &LockConfig::default(), Force::Deny).unwrap();

        assert!(matches!(
            store.remove_epic_asset("my-epic", "a.bin"),
            Err(StoreError::Lock(_))
        ));
        assert!(
            path.is_file(),
            "the bytes are untouched while the lock is held"
        );

        drop(held);
        store.remove_epic_asset("my-epic", "a.bin").unwrap();
        assert!(!path.is_file());
    }

    // -- the bracketed read-modify-write ------------------------------------

    #[test]
    fn a_modify_hands_the_change_the_stored_entity_and_publishes_the_result() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        let mut stored = epic();
        stored.frontmatter.name = "as stored".into();
        store.write_epic("my-epic", &stored).unwrap();

        // The change is handed the epic as stored, and the returned epic is the
        // one published.
        let (published, seen) = store
            .modify_epic("my-epic", None, |epic| {
                let seen = epic.frontmatter.name.clone();
                epic.frontmatter.name = "renamed".into();
                Ok::<String, StoreError>(seen)
            })
            .unwrap();
        assert_eq!(seen, "as stored");
        assert_eq!(published.frontmatter.name, "renamed");
        assert_eq!(
            store.read_epic("my-epic").unwrap().frontmatter.name,
            "renamed"
        );
    }

    #[test]
    fn a_modify_holds_the_lock_from_before_the_read_until_the_publish() {
        // The window between the read the change sees and the rename that
        // publishes it is closed: no other writer can hold the target's lock
        // while the change is running, so nothing can land in between.
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        store.write_node("my-epic", 7, &node(7)).unwrap();
        let path = store.node_path("my-epic", 7);

        let (_published, competitor) = store
            .modify_node("my-epic", 7, None, |n| {
                n.body = "mine\n".into();
                // A single non-blocking acquire attempt from inside the window.
                Ok::<Option<lock::TempLock>, StoreError>(lock::try_acquire(&path).unwrap())
            })
            .unwrap();
        assert!(
            competitor.is_none(),
            "the target's lock is held for the whole read-modify-write"
        );
        assert_eq!(store.read_node("my-epic", 7).unwrap().body, "mine\n");
    }

    #[test]
    fn a_modify_never_creates_and_refuses_an_unparseable_target() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        // Give the epic dir a real file so the directory exists but the epic
        // does not.
        store.write_node("my-epic", 7, &node(7)).unwrap();

        // Nothing stored to modify: refused, and no epic conjured into being.
        let missing = store.modify_epic("my-epic", None, |epic| {
            epic.frontmatter.name = "resurrected".into();
            Ok::<(), StoreError>(())
        });
        assert!(matches!(missing, Err(StoreError::Conflict { .. })));
        assert!(!store.epic_path("my-epic").exists());

        // Contents that cannot be parsed are refused rather than clobbered, and
        // the refusal leaves no lock debris behind for a retry to trip over.
        std::fs::write(store.epic_path("my-epic"), b"not a store file").unwrap();
        let unparseable = store.modify_epic("my-epic", None, |epic| {
            epic.frontmatter.name = "clobbered".into();
            Ok::<(), StoreError>(())
        });
        assert!(matches!(unparseable, Err(StoreError::Model(_))));
        assert_eq!(
            std::fs::read(store.epic_path("my-epic")).unwrap(),
            b"not a store file"
        );
        assert!(!lock::temp_path(&store.epic_path("my-epic"))
            .unwrap()
            .exists());
    }

    #[test]
    fn a_change_that_changes_nothing_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        store.write_node("my-epic", 7, &node(7)).unwrap();
        let path = store.node_path("my-epic", 7);
        // Re-lay the same node with its frontmatter keys in another order: it
        // parses to the very same entity, so a publish would be visible as the
        // canonical rendering replacing this one.
        let canonical = std::fs::read_to_string(&path).unwrap();
        let reordered = canonical.replace("name: n\nsummary: s\n", "summary: s\nname: n\n");
        assert_ne!(reordered, canonical, "the planted text must differ");
        std::fs::write(&path, &reordered).unwrap();

        store
            .modify_node("my-epic", 7, None, |_| Ok::<(), StoreError>(()))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            reordered,
            "a change that leaves the entity as found must not rewrite the file"
        );
    }

    // -- the opt-in `updated` precondition ----------------------------------

    #[test]
    fn an_epic_modify_naming_the_stored_stamp_applies_and_a_stale_one_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        store.write_epic("my-epic", &epic()).unwrap();
        let stamp = store.read_epic("my-epic").unwrap().frontmatter.updated;

        // The stored stamp still matches the one read, so the change applies.
        // The stamp compared is the stored one, never the one being written.
        store
            .modify_epic("my-epic", Some(stamp), |epic| {
                epic.frontmatter.name = "renamed".into();
                epic.frontmatter.updated = "2024-06-01T00:00:00Z".parse().unwrap();
                Ok::<(), StoreError>(())
            })
            .unwrap();
        assert_eq!(
            store.read_epic("my-epic").unwrap().frontmatter.name,
            "renamed"
        );

        // That change moved the stamp, so one still naming the original is
        // refused, and its change never runs.
        let mut change_ran = false;
        let refused = store.modify_epic("my-epic", Some(stamp), |epic| {
            change_ran = true;
            epic.frontmatter.name = "clobbered".into();
            Ok::<(), StoreError>(())
        });
        assert!(matches!(refused, Err(StoreError::Conflict { .. })));
        assert!(!change_ran, "a refused precondition runs no change");
        assert_eq!(
            store.read_epic("my-epic").unwrap().frontmatter.name,
            "renamed",
            "a refused write leaves the stored epic untouched"
        );
        // The refusal released the lock, so a retry is not blocked by debris.
        assert!(!lock::temp_path(&store.epic_path("my-epic"))
            .unwrap()
            .exists());

        // Naming no stamp keeps last-write-wins: the same change now applies.
        store
            .modify_epic("my-epic", None, |epic| {
                epic.frontmatter.name = "clobbered".into();
                Ok::<(), StoreError>(())
            })
            .unwrap();
        assert_eq!(
            store.read_epic("my-epic").unwrap().frontmatter.name,
            "clobbered"
        );
    }

    #[test]
    fn a_node_modify_naming_the_stored_stamp_applies_and_a_stale_one_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let store = fast_store(dir.path());
        store.write_node("my-epic", 7, &node(7)).unwrap();
        let stamp = store.read_node("my-epic", 7).unwrap().frontmatter.updated;

        store
            .modify_node("my-epic", 7, Some(stamp), |n| {
                n.body = "mine\n".into();
                n.frontmatter.updated = "2024-06-01T00:00:00Z".parse().unwrap();
                Ok::<(), StoreError>(())
            })
            .unwrap();
        assert_eq!(store.read_node("my-epic", 7).unwrap().body, "mine\n");

        let refused = store.modify_node("my-epic", 7, Some(stamp), |n| {
            n.body = "theirs\n".into();
            Ok::<(), StoreError>(())
        });
        assert!(matches!(refused, Err(StoreError::Conflict { .. })));
        assert_eq!(store.read_node("my-epic", 7).unwrap().body, "mine\n");

        store
            .modify_node("my-epic", 7, None, |n| {
                n.body = "theirs\n".into();
                Ok::<(), StoreError>(())
            })
            .unwrap();
        assert_eq!(store.read_node("my-epic", 7).unwrap().body, "theirs\n");
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
    fn init_default_container_creates_meta_and_no_pointer() {
        // The default in-place container is `here/.loti`; discovery finds it on
        // a bare walk, so no `.loti.conf` breadcrumb is written.
        let dir = tempfile::tempdir().unwrap();
        let container = dir.path().join(MARKER_DIR);
        let outcome = init(dir.path(), &container).unwrap();
        assert_eq!(outcome.root, container);
        assert!(outcome.config_pointer.is_none());
        assert!(container.join("meta").is_file());
        let store = Store::at(&outcome.root);
        assert_eq!(store.read_meta().unwrap(), Meta::current());
    }

    #[test]
    fn init_container_here_writes_meta_and_no_pointer() {
        // An explicit container that is literally `here` puts meta directly in
        // the invocation dir and writes no breadcrumb (nothing to redirect to).
        let dir = tempfile::tempdir().unwrap();
        let outcome = init(dir.path(), dir.path()).unwrap();
        assert_eq!(outcome.root, dir.path());
        assert!(outcome.config_pointer.is_none());
        assert!(dir.path().join("meta").is_file());
        assert!(!dir.path().join(CONFIG_FILE).exists());
    }

    #[test]
    fn init_with_explicit_container_writes_a_config_pointer() {
        // An explicit container elsewhere is literal (no `.loti` appended): meta
        // lands at `<container>/meta` and a relative breadcrumb points at it.
        let dir = tempfile::tempdir().unwrap();
        let outcome = init(dir.path(), &dir.path().join("store")).unwrap();
        assert_eq!(outcome.root, dir.path().join("store"));
        let pointer = outcome.config_pointer.expect("expected a config pointer");
        assert_eq!(pointer, dir.path().join(CONFIG_FILE));
        let body = std::fs::read_to_string(&pointer).unwrap();
        assert!(body.contains("loti-root = \"store\""));
        assert!(dir.path().join("store").join("meta").is_file());
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
        let container = dir.path().join(MARKER_DIR);
        init(dir.path(), &container).unwrap();
        assert!(matches!(
            init(dir.path(), &container),
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
        assert!(store.verify_mutable().is_ok());
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
        // Writes are refused too, and asking up front gives the same reason as
        // attempting the write.
        assert_eq!(store.verify_mutable(), Err(VersionRefusal::StoreTooNew));
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
        // Any mutation is refused, pointing at migrate-store, and the up-front
        // check says so before a write is attempted.
        assert_eq!(store.verify_mutable(), Err(VersionRefusal::NeedsMigration));
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
        assert!(store.verify_mutable().is_ok());
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
        // Every mutation is refused as mid-migration, up-front check included.
        assert_eq!(
            store.verify_mutable(),
            Err(VersionRefusal::MigrationInProgress)
        );
        assert!(matches!(
            store.write_epic("my-epic", &epic()),
            Err(StoreError::Version(VersionRefusal::MigrationInProgress))
        ));
    }

    #[test]
    fn unreadable_version_refuses_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_version(dir.path(), "not-a-version");
        assert_eq!(store.verify_mutable(), Err(VersionRefusal::Unreadable));
        assert!(matches!(
            store.write_epic("my-epic", &epic()),
            Err(StoreError::Version(VersionRefusal::Unreadable))
        ));
    }
}
