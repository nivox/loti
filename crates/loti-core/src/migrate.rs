//! Aligning an older on-disk store to the version this binary writes.
//!
//! Migration moves a store from the version it recorded to the version this
//! binary understands. Two shapes, chosen by how far apart they are:
//!
//!   * **Minor behind, same major** — only the recorded version is stale; the
//!     store bytes are already compatible in both directions. Migration is a
//!     metadata-only version bump, no store rewrite.
//!   * **Major behind** — the layout/fields changed across the gap, so the
//!     store bytes must be transformed. This runs a sentinel-barrier protocol
//!     that needs no global lock:
//!       1. record a mid-migration sentinel as the store's version. From this
//!          instant every binary (including this one) treats the store as
//!          read-only, and the sentinel doubles as a crash dirty-marker;
//!       2. drain — wait until no staging temp files remain anywhere under the
//!          store, so no edit is mid-flight. The lock-then-verify ordering used
//!          by every mutation guarantees a temp file's absence means quiescence:
//!          an edit either fully preceded the sentinel flip (drain waits for it)
//!          or re-reads the flipped version and aborts;
//!       3. snapshot, transform, replace — copy the live store aside as a
//!          preserved backup, apply the per-major transforms into a fresh
//!          directory, then swap the transformed directory into place;
//!       4. record the clean target version — the commit point. Only now is the
//!          store settled and writable again.
//!
//! Crash recovery falls out of the sentinel: a migration that dies leaves the
//! sentinel (and the preserved backup) in place, so the store stays read-only
//! for everyone. Re-running migration restarts from the preserved backup, so a
//! half-applied transform is discarded and redone rather than resumed in place.
//!
//! Concrete choices this module makes (the rules, stated so they can be checked
//! against reality):
//!   * **Snapshot/replace technique: copy-aside then swap.** The live store is
//!     copied to a sibling backup directory; the transform builds a fresh
//!     directory; the old store directory is moved to a discard name and the
//!     fresh one moved into place. A crash between those two moves is recovered
//!     because the backup, not the possibly-half-swapped live tree, is the
//!     source of truth on re-run.
//!   * **Backup retention/naming.** The preserved copy is a sibling of the data
//!     root named with a fixed suffix; it is kept after a successful migration
//!     so a human can inspect or roll back, and it is the resume source if a
//!     migration is re-run. Re-running removes a leftover discard directory and
//!     rebuilds from the backup.
//!   * **Drain timeout.** The drain waits a bounded time for staging temp files
//!     to clear; if any persists past the bound it fails rather than hanging,
//!     and a caller may force past a drain that is stuck on abandoned temps.
//!   * **Progress.** Each ordered step reports a short line through a caller
//!     sink so a human sees the sentinel set, the drain, the transform and the
//!     commit as they happen.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::meta::{self, Meta, StoreVersion, MARKER_DIR};
use crate::FORMAT_VERSION;

/// The sibling-directory suffix for the preserved pre-migration copy. Kept
/// after a successful migration for inspection/rollback and used as the resume
/// source when a migration is re-run.
const BACKUP_SUFFIX: &str = ".loti-migrate-backup";

/// The sibling-directory suffix for the old store while the transformed store
/// is swapped into place. A leftover of this name means a crash mid-swap; it is
/// removed and the swap redone from the backup on re-run.
const DISCARD_SUFFIX: &str = ".loti-migrate-old";

/// The sibling-directory suffix the transform builds the new store into before
/// it is swapped in. A leftover means a crash before the swap; it is rebuilt.
const STAGE_SUFFIX: &str = ".loti-migrate-new";

/// Default bound on the drain wait for staging temp files to clear. Chosen
/// comfortably above a healthy edit's hold time so an ordinary in-flight
/// mutation always completes within it, while a truly abandoned temp fails the
/// drain rather than hanging a migration forever.
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// The staging temp-file marker every mutation uses: a leading dot and a
/// `.tmp` suffix. The drain treats any file matching this shape under the store
/// as an in-flight edit.
const TEMP_PREFIX: &str = ".";
const TEMP_SUFFIX: &str = ".tmp";

/// Whether to force past a drain stuck on staging temp files that never clear
/// (presumed abandoned by dead operations).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Force {
    /// Fail the drain if temp files persist past the timeout.
    Deny,
    /// Proceed once the timeout elapses, treating lingering temps as abandoned.
    Force,
}

/// Tunables for a migration run. Defaults follow the documented rules; tests
/// shrink the drain timeout to stay fast and hermetic.
#[derive(Debug, Clone, Copy)]
pub struct MigrateConfig {
    /// How long the drain waits for staging temp files to clear.
    pub drain_timeout: Duration,
    /// How often the drain re-checks for staging temp files.
    pub drain_poll: Duration,
    /// Whether to force past a drain that will not clear.
    pub force: Force,
}

impl Default for MigrateConfig {
    fn default() -> Self {
        Self {
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            drain_poll: Duration::from_millis(50),
            force: Force::Deny,
        }
    }
}

/// What a migration run did, for reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The store was already at this binary's version; nothing to do.
    AlreadyCurrent,
    /// A metadata-only minor bump; the store bytes were untouched.
    MinorBumped {
        /// Version before the bump.
        from: (u32, u32),
        /// Version after the bump.
        to: (u32, u32),
    },
    /// A major migration ran the full transform and committed the new version.
    Migrated {
        /// Version before migration.
        from: (u32, u32),
        /// Version after migration (this binary's).
        to: (u32, u32),
        /// How many major steps were applied.
        steps: usize,
    },
}

/// Why a migration could not run or complete.
#[derive(Debug, Error)]
pub enum MigrateError {
    /// The store metadata could not be read or written.
    #[error(transparent)]
    Meta(#[from] meta::MetaError),
    /// The store's recorded version could not be parsed.
    #[error("the store's format version is unreadable, so it cannot be migrated")]
    Unreadable,
    /// The store is newer than this binary; migration cannot downgrade it.
    #[error("this store is newer than this loti; upgrade loti instead of migrating")]
    StoreTooNew,
    /// No transform is registered for a major step the migration needs.
    #[error("no migration is available from format major {from} to {to}")]
    MissingTransform {
        /// The major the missing step starts from.
        from: u32,
        /// The major the missing step produces.
        to: u32,
    },
    /// The drain timed out with staging temp files still present.
    #[error(
        "the store is still being edited (staging files remain after waiting); \
         retry when idle, or force past if an operation was interrupted"
    )]
    DrainTimedOut,
    /// A migration is marked in progress but its preserved copy is gone, so it
    /// cannot be safely resumed.
    #[error(
        "this store is marked mid-migration but its preserved copy is missing, \
         so the migration cannot be safely resumed"
    )]
    NoBackupToResume,
    /// A filesystem step failed.
    #[error("migration step failed accessing {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

/// A single major-to-major transform: given the staged copy of the store
/// (already populated with the previous major's bytes), rewrite it in place to
/// the next major's layout. It operates on a directory tree, not the live
/// store, so a failure never touches the original.
///
/// Registered by the major it upgrades *from*; running it advances the store to
/// `from + 1`.
pub trait MajorTransform {
    /// The major version this transform reads.
    fn source_major(&self) -> u32;
    /// Rewrite the staged store directory from `source_major` to
    /// `source_major + 1`.
    fn apply(&self, staged_store: &Path) -> Result<(), MigrateError>;
}

/// The set of known major transforms, keyed by the major they upgrade from.
/// A migration across several majors chains them in order; a gap with no
/// registered transform is a hard error rather than a silent skip.
#[derive(Default)]
pub struct TransformRegistry {
    transforms: Vec<Box<dyn MajorTransform>>,
}

impl TransformRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a transform. Later registration wins for the same `from` major.
    pub fn register(&mut self, transform: Box<dyn MajorTransform>) -> &mut Self {
        self.transforms.push(transform);
        self
    }

    /// The transform that upgrades from `major`, if any.
    fn for_major(&self, major: u32) -> Option<&dyn MajorTransform> {
        self.transforms
            .iter()
            .rev()
            .find(|t| t.source_major() == major)
            .map(|b| b.as_ref())
    }
}

/// A sink for short human-facing progress lines. The default discards them.
pub trait Progress {
    /// Report one step line.
    fn step(&mut self, message: &str);
}

/// A progress sink that discards everything, for callers that do not report.
pub struct SilentProgress;
impl Progress for SilentProgress {
    fn step(&mut self, _message: &str) {}
}

/// Migrate the store under `root` to this binary's version, using `registry`
/// for any major steps and `config` for drain policy, reporting through
/// `progress`.
///
/// The version rules decide the shape: equal is a no-op, minor-behind is a
/// metadata-only bump, and major-behind runs the sentinel-barrier protocol. A
/// store newer than this binary, or with an unreadable version, cannot be
/// migrated.
pub fn migrate_store(
    root: &Path,
    registry: &TransformRegistry,
    config: &MigrateConfig,
    progress: &mut dyn Progress,
) -> Result<Outcome, MigrateError> {
    migrate_store_to(root, FORMAT_VERSION, registry, config, progress)
}

/// Migrate the store under `root` up to an explicit `target` version, rather
/// than this binary's. The public [`migrate_store`] is exactly this with the
/// binary's own version. Threading the target explicitly keeps the whole
/// protocol independent of the pinned version, so the machinery can be driven
/// across a fabricated major gap regardless of what version this binary happens
/// to carry.
pub fn migrate_store_to(
    root: &Path,
    target: (u32, u32),
    registry: &TransformRegistry,
    config: &MigrateConfig,
    progress: &mut dyn Progress,
) -> Result<Outcome, MigrateError> {
    let meta = meta::read(root)?;
    let observed = meta.store_version().ok_or(MigrateError::Unreadable)?;

    let (target_major, target_minor) = target;

    // A mid-migration sentinel means either a live migration by another process
    // or a crashed one. Either way this run restarts the major migration from
    // the preserved backup — resuming in place is never safe.
    if let StoreVersion::Migrating { major, minor } = observed {
        progress.step("resuming an interrupted migration from the preserved copy");
        return resume_or_run_major(root, (major, minor), target, registry, config, progress);
    }

    let (store_major, store_minor) = observed.version();

    if store_major > target_major {
        return Err(MigrateError::StoreTooNew);
    }
    if store_major == target_major {
        // Same major: settled or a minor bump. A store at or ahead on minor is
        // already current for the target.
        if store_minor >= target_minor {
            return Ok(Outcome::AlreadyCurrent);
        }
        progress.step("bumping the recorded format version (no store rewrite needed)");
        meta::write(root, &Meta::clean(target_major, target_minor))?;
        return Ok(Outcome::MinorBumped {
            from: (store_major, store_minor),
            to: (target_major, target_minor),
        });
    }

    // store_major < target_major: a major migration.
    run_major(
        root,
        (store_major, store_minor),
        target,
        registry,
        config,
        progress,
    )
}

/// Run the major migration from a clean older store: set the sentinel, then run
/// the ordered sentinel-barrier steps. The source version is `from`, recorded
/// into the preserved backup so a later resume is self-describing.
fn run_major(
    root: &Path,
    from: (u32, u32),
    target: (u32, u32),
    registry: &TransformRegistry,
    config: &MigrateConfig,
    progress: &mut dyn Progress,
) -> Result<Outcome, MigrateError> {
    let (target_major, target_minor) = target;

    // Step 1: record the sentinel. From here the store is read-only for all,
    // and the sentinel doubles as the crash dirty-marker. The live tree is not
    // touched by any later step until the single atomic swap, so it stays the
    // pristine source until the migration commits.
    progress.step("marking the store mid-migration (read-only until it finishes)");
    meta::write(root, &Meta::migrating(target_major, target_minor))?;

    // A fresh run snapshots the live tree; there is no prior backup to trust.
    let steps = apply_major_steps(root, from, target, false, registry, config, progress)?;

    Ok(Outcome::Migrated {
        from,
        to: (target_major, target_minor),
        steps,
    })
}

/// Resume a migration observed via the sentinel (crash recovery). The live tree
/// is only ever replaced by one atomic swap, so on a crash it is either still
/// the pristine original (swap never happened) or already the migrated store
/// (swap committed but meta not yet cleared). A preserved backup, if present,
/// is the authoritative pristine source; otherwise the still-pristine live tree
/// is the source.
fn resume_or_run_major(
    root: &Path,
    _sentinel_target: (u32, u32),
    target: (u32, u32),
    registry: &TransformRegistry,
    config: &MigrateConfig,
    progress: &mut dyn Progress,
) -> Result<Outcome, MigrateError> {
    let backup = sibling(root, BACKUP_SUFFIX);
    // The source version comes from the backup when one survived (it is written
    // clean), else from the live tree — which, absent a committed swap, is still
    // the pristine pre-migration store.
    let source = if backup.is_dir() { &backup } else { root };
    let from = match meta::read(source)?.store_version() {
        // The backup records a clean source version; the live tree records the
        // sentinel, whose target is not the source. If the live tree is the
        // only source and it carries the sentinel, the swap must have already
        // committed the migrated store, so there is nothing left to transform.
        Some(StoreVersion::Clean { major, minor }) => (major, minor),
        Some(StoreVersion::Migrating { .. }) if source == root => {
            // The live tree is already the migrated store; just clear the
            // sentinel to the clean target (the commit that the crash skipped).
            meta::write(root, &Meta::clean(target.0, target.1))?;
            return Ok(Outcome::Migrated {
                from: target,
                to: target,
                steps: 0,
            });
        }
        _ => return Err(MigrateError::NoBackupToResume),
    };
    // A resume trusts the preserved backup as the authoritative pristine source
    // and rebuilds from it, rather than re-snapshotting the live tree — which,
    // after a crash mid-swap, may already be the migrated store.
    let have_backup = backup.is_dir();
    let steps = apply_major_steps(root, from, target, have_backup, registry, config, progress)?;
    Ok(Outcome::Migrated {
        from,
        to: target,
        steps,
    })
}

/// The shared tail of a major migration, in order: drain the store to
/// quiescence, snapshot the now-idle live tree to the preserved backup,
/// transform a staging copy of it, swap the staging copy into place, then
/// commit the clean `target` version. Idempotent and re-runnable: it rebuilds
/// the backup and staging from the live tree each time, and the live tree is
/// only replaced by the single atomic swap.
fn apply_major_steps(
    root: &Path,
    from: (u32, u32),
    target: (u32, u32),
    reuse_backup: bool,
    registry: &TransformRegistry,
    config: &MigrateConfig,
    progress: &mut dyn Progress,
) -> Result<usize, MigrateError> {
    let (target_major, target_minor) = target;
    let backup = sibling(root, BACKUP_SUFFIX);

    if reuse_backup {
        // Resuming: the existing backup is the authoritative pristine source. Do
        // not re-snapshot the live tree, which may already be swapped.
        progress.step("rebuilding from the preserved copy of the store");
    } else {
        // Step 2: drain in-flight edits. With the sentinel set, no new edit can
        // commit, so once staging temps clear the store is quiescent.
        progress.step("waiting for in-flight edits to finish");
        drain(root, config)?;

        // Step 3a: snapshot the now-idle live tree to the preserved backup.
        // Taken after the drain so it never captures an in-flight staging temp,
        // and recorded with a clean source version so a resume is
        // self-describing. This is the resume source that survives the swap.
        remove_dir_if_exists(&backup)?;
        copy_dir(root, &backup)?;
        meta::write(&backup, &Meta::clean(from.0, from.1))?;
    }

    // Step 3b: build the new store in a staging sibling from the pristine
    // backup, chain the per-major transforms, then swap it into place.
    let stage = sibling(root, STAGE_SUFFIX);
    remove_dir_if_exists(&stage)?;
    copy_dir(&backup, &stage)?;

    let mut steps = 0usize;
    let mut major = from.0;
    while major < target_major {
        let transform = registry
            .for_major(major)
            .ok_or(MigrateError::MissingTransform {
                from: major,
                to: major + 1,
            })?;
        progress.step(&format!(
            "transforming store from format major {major} to {}",
            major + 1
        ));
        transform.apply(&stage)?;
        major += 1;
        steps += 1;
    }

    // Record the settled target inside the staged store so the swapped-in tree
    // is already clean before it becomes live.
    meta::write(&stage, &Meta::clean(target_major, target_minor))?;

    progress.step("swapping the migrated store into place");
    swap_in(root, &stage)?;

    // Step 4: the commit point. The live store now carries the clean version and
    // is writable again. (swap_in already placed clean meta; assert-write here
    // makes the commit explicit and covers a store swapped without meta.)
    meta::write(root, &Meta::clean(target_major, target_minor))?;

    progress.step("migration complete");
    Ok(steps)
}

/// Wait until no staging temp files remain under `root`. A staging temp is any
/// file whose name starts with a dot and ends with `.tmp` (the deterministic
/// lock/staging name every mutation uses). With the sentinel set no new one can
/// appear, so this converges; a temp that never clears fails the drain unless
/// forced.
fn drain(root: &Path, config: &MigrateConfig) -> Result<(), MigrateError> {
    let deadline = Instant::now() + config.drain_timeout;
    loop {
        if !any_temp_files(root)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return match config.force {
                // Forcing treats lingering temps as abandoned and proceeds; the
                // transform reads the backup, not the temps, so this is safe.
                Force::Force => Ok(()),
                Force::Deny => Err(MigrateError::DrainTimedOut),
            };
        }
        std::thread::sleep(config.drain_poll);
    }
}

/// Whether any staging temp file exists anywhere under `root`, excluding the
/// marker directory (which holds only metadata, never staging temps for nodes).
fn any_temp_files(root: &Path) -> Result<bool, MigrateError> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(MigrateError::Io { path: dir, source }),
        };
        for entry in entries {
            let entry = entry.map_err(|source| MigrateError::Io {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| MigrateError::Io {
                path: path.clone(),
                source,
            })?;
            if file_type.is_dir() {
                // The marker directory never holds node staging temps; skip it
                // so the drain only watches the store's own edit staging.
                if path.file_name().and_then(|n| n.to_str()) == Some(MARKER_DIR) {
                    continue;
                }
                stack.push(path);
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with(TEMP_PREFIX) && name.ends_with(TEMP_SUFFIX) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Move the staged store into place over the live one. The old live tree is
/// moved aside to a discard name first (so a crash leaves the backup as the
/// source of truth), then the staged tree is moved in, then the discard is
/// removed. Re-running clears a leftover discard.
fn swap_in(root: &Path, stage: &Path) -> Result<(), MigrateError> {
    let discard = sibling(root, DISCARD_SUFFIX);
    remove_dir_if_exists(&discard)?;

    // Move the live store aside. If the live store no longer exists (a crash
    // between the two moves on a previous run), skip straight to placing stage.
    if root.exists() {
        rename(root, &discard)?;
    }
    match rename(stage, root) {
        Ok(()) => {}
        Err(e) => {
            // Restore the live store from the discard so the store is never left
            // missing, then surface the failure.
            if discard.exists() && !root.exists() {
                let _ = rename(&discard, root);
            }
            return Err(e);
        }
    }
    remove_dir_if_exists(&discard)?;
    Ok(())
}

/// A sibling of `root` with the given suffix appended to its file name, so the
/// migration's working directories never live inside the store being migrated.
fn sibling(root: &Path, suffix: &str) -> PathBuf {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "store".to_string());
    let parent = root.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{name}{suffix}"))
}

/// Recursively copy a directory tree from `src` to `dst`. `dst` must not exist.
fn copy_dir(src: &Path, dst: &Path) -> Result<(), MigrateError> {
    std::fs::create_dir_all(dst).map_err(|source| MigrateError::Io {
        path: dst.to_path_buf(),
        source,
    })?;
    let entries = std::fs::read_dir(src).map_err(|source| MigrateError::Io {
        path: src.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| MigrateError::Io {
            path: src.to_path_buf(),
            source,
        })?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let file_type = entry.file_type().map_err(|source| MigrateError::Io {
            path: from.clone(),
            source,
        })?;
        if file_type.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|source| MigrateError::Io {
                path: from.clone(),
                source,
            })?;
        }
    }
    Ok(())
}

fn rename(from: &Path, to: &Path) -> Result<(), MigrateError> {
    std::fs::rename(from, to).map_err(|source| MigrateError::Io {
        path: from.to_path_buf(),
        source,
    })
}

fn remove_dir_if_exists(path: &Path) -> Result<(), MigrateError> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MigrateError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

// ---------------------------------------------------------------------------
// registered transforms
// ---------------------------------------------------------------------------

/// The registry of every known major transform, used by callers that migrate
/// to this binary's version. Registered in `from`-major order.
pub fn default_registry() -> TransformRegistry {
    let mut registry = TransformRegistry::new();
    registry.register(Box::new(BlockedByV0ToV1));
    registry
}

/// Transform node files from format major 0 to 1: split the coupled major-0
/// `blocked-by: {refs, reason}` map into an independent `blocked-by` ref list
/// plus a `block-reason` scalar.
///
/// The rule this enforces: in major 0 a node's blocker was one map tied to the
/// `blocked` state; in major 1 the dependency list (`blocked-by`) and the
/// state's reason (`block-reason`) are separate, status-independent fields. A
/// node that carried the old map keeps its `refs` as the new list and moves its
/// `reason` to `block-reason`; a node without the old map shape is left
/// untouched.
pub struct BlockedByV0ToV1;

impl MajorTransform for BlockedByV0ToV1 {
    fn source_major(&self) -> u32 {
        0
    }

    fn apply(&self, staged_store: &Path) -> Result<(), MigrateError> {
        let epics = match std::fs::read_dir(staged_store) {
            Ok(e) => e,
            Err(source) => {
                return Err(MigrateError::Io {
                    path: staged_store.to_path_buf(),
                    source,
                })
            }
        };
        for epic in epics {
            let epic = epic.map_err(|source| MigrateError::Io {
                path: staged_store.to_path_buf(),
                source,
            })?;
            let epic_dir = epic.path();
            if !epic_dir.is_dir() {
                continue;
            }
            // The store's own marker/working directories are not epics.
            if epic_dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with('.'))
                .unwrap_or(true)
            {
                continue;
            }
            let files = std::fs::read_dir(&epic_dir).map_err(|source| MigrateError::Io {
                path: epic_dir.clone(),
                source,
            })?;
            for file in files {
                let file = file.map_err(|source| MigrateError::Io {
                    path: epic_dir.clone(),
                    source,
                })?;
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    transform_blocked_by_file(&path)?;
                }
            }
        }
        Ok(())
    }
}

/// Rewrite one file's frontmatter if it carries the old `blocked-by` map. A
/// file that is not a frontmatter document, or whose `blocked-by` is absent or
/// already a list, is left byte-for-byte unchanged.
fn transform_blocked_by_file(path: &Path) -> Result<(), MigrateError> {
    use serde_yaml::{Mapping, Value};

    let invalid = |source: std::io::Error| MigrateError::Io {
        path: path.to_path_buf(),
        source,
    };
    let yaml_err =
        |e: serde_yaml::Error| invalid(std::io::Error::new(std::io::ErrorKind::InvalidData, e));

    let text = std::fs::read_to_string(path).map_err(invalid)?;
    let split = match crate::frontmatter::split(&text) {
        Ok(s) => s,
        // Not a frontmatter document: nothing to transform.
        Err(_) => return Ok(()),
    };
    let mut map: Mapping = serde_yaml::from_str(&split.frontmatter).map_err(yaml_err)?;

    let key = Value::from("blocked-by");
    let old = match map.get(&key) {
        Some(Value::Mapping(m)) => m.clone(),
        // Absent, or already the new list form: leave it alone.
        _ => return Ok(()),
    };

    let refs: Vec<Value> = old
        .get(Value::from("refs"))
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    let reason = old
        .get(Value::from("reason"))
        .and_then(Value::as_str)
        .map(str::to_string);

    // blocked-by becomes the plain ref list (dropped when empty).
    if refs.is_empty() {
        map.remove(&key);
    } else {
        map.insert(key, Value::Sequence(refs));
    }
    // The old reason moves to block-reason (the blocked state's reason).
    if let Some(reason) = reason {
        map.insert(Value::from("block-reason"), Value::from(reason));
    }

    let yaml = serde_yaml::to_string(&map).map_err(yaml_err)?;
    let out = crate::frontmatter::join(&yaml, &split.body);
    std::fs::write(path, out).map_err(invalid)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A progress sink that records the step lines for assertions.
    #[derive(Default, Clone)]
    struct RecordProgress {
        lines: Arc<Mutex<Vec<String>>>,
    }
    impl Progress for RecordProgress {
        fn step(&mut self, message: &str) {
            self.lines.lock().unwrap().push(message.to_string());
        }
    }
    impl RecordProgress {
        fn lines(&self) -> Vec<String> {
            self.lines.lock().unwrap().clone()
        }
    }

    /// A transform that upgrades from a given major by writing a breadcrumb file
    /// into the staged store, so a test can assert it actually ran against the
    /// staged copy (never the live store).
    struct MarkerTransform {
        from: u32,
    }
    impl MajorTransform for MarkerTransform {
        fn source_major(&self) -> u32 {
            self.from
        }
        fn apply(&self, staged_store: &Path) -> Result<(), MigrateError> {
            let marker = staged_store.join(format!("transformed-from-{}.marker", self.from));
            std::fs::write(&marker, b"x").map_err(|source| MigrateError::Io {
                path: marker,
                source,
            })?;
            Ok(())
        }
    }

    fn fast_config(force: Force) -> MigrateConfig {
        MigrateConfig {
            drain_timeout: Duration::from_millis(120),
            drain_poll: Duration::from_millis(5),
            force,
        }
    }

    /// A store rooted at a temp dir, recorded at the given version, with one
    /// epic file so a real tree is copied/transformed.
    fn store_at_version(version: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        std::fs::create_dir_all(root.join("proj")).unwrap();
        std::fs::write(root.join("proj").join("epic.md"), "---\nid: proj\n---\n").unwrap();
        meta::write(
            &root,
            &Meta {
                format_version: version.to_string(),
            },
        )
        .unwrap();
        (dir, root)
    }

    #[test]
    fn equal_version_is_a_no_op() {
        // A store already at this binary's version needs nothing done.
        let (major, minor) = FORMAT_VERSION;
        let (_d, root) = store_at_version(&format!("{major}.{minor}"));
        let out = migrate_store(
            &root,
            &TransformRegistry::new(),
            &fast_config(Force::Deny),
            &mut SilentProgress,
        )
        .unwrap();
        assert_eq!(out, Outcome::AlreadyCurrent);
    }

    #[test]
    fn minor_behind_is_a_meta_only_bump() {
        // A fabricated minor gap (store 1.0 -> target 1.3): a meta-only bump
        // that never rewrites the store bytes, driven through migrate_store_to
        // so it runs regardless of this binary's pinned minor.
        let (_d, root) = store_at_version("1.0");
        let before = std::fs::read_to_string(root.join("proj").join("epic.md")).unwrap();
        let out = migrate_store_to(
            &root,
            (1, 3),
            &TransformRegistry::new(),
            &fast_config(Force::Deny),
            &mut SilentProgress,
        )
        .unwrap();
        assert_eq!(
            out,
            Outcome::MinorBumped {
                from: (1, 0),
                to: (1, 3),
            }
        );
        // The store bytes were not rewritten by a minor bump.
        let after = std::fs::read_to_string(root.join("proj").join("epic.md")).unwrap();
        assert_eq!(before, after);
        assert_eq!(meta::read(&root).unwrap(), Meta::clean(1, 3));
    }

    #[test]
    fn store_newer_than_target_cannot_be_migrated() {
        // A store two majors up cannot be migrated down to a lower target.
        let (_d, root) = store_at_version("3.0");
        let err = migrate_store_to(
            &root,
            (1, 0),
            &TransformRegistry::new(),
            &fast_config(Force::Deny),
            &mut SilentProgress,
        )
        .unwrap_err();
        assert!(matches!(err, MigrateError::StoreTooNew));
    }

    #[test]
    fn major_migration_sets_sentinel_transforms_and_commits_clean() {
        // A fabricated one-major gap (store 0.x -> target 1.0), driven through
        // migrate_store_to so the major path runs whatever version this binary
        // is pinned at.
        let (_d, root) = store_at_version("0.0");
        let mut registry = TransformRegistry::new();
        registry.register(Box::new(MarkerTransform { from: 0 }));
        let progress = RecordProgress::default();
        let mut p = progress.clone();
        let out =
            migrate_store_to(&root, (1, 0), &registry, &fast_config(Force::Deny), &mut p).unwrap();
        assert_eq!(
            out,
            Outcome::Migrated {
                from: (0, 0),
                to: (1, 0),
                steps: 1,
            }
        );
        // The transform ran against the staged copy and its breadcrumb is now in
        // the live store.
        assert!(root.join("transformed-from-0.marker").is_file());
        // The committed version is clean (no sentinel).
        assert_eq!(
            meta::read(&root).unwrap().store_version(),
            Some(StoreVersion::Clean { major: 1, minor: 0 })
        );
        // A backup was preserved for inspection/rollback.
        assert!(sibling(&root, BACKUP_SUFFIX).is_dir());
        // Ordering holds: sentinel set -> transform -> commit.
        let lines = progress.lines();
        let sentinel_idx = lines
            .iter()
            .position(|l| l.contains("mid-migration"))
            .unwrap();
        let transform_idx = lines
            .iter()
            .position(|l| l.contains("transforming store"))
            .unwrap();
        let commit_idx = lines.iter().position(|l| l.contains("complete")).unwrap();
        assert!(sentinel_idx < transform_idx, "sentinel precedes transform");
        assert!(transform_idx < commit_idx, "transform precedes commit");
    }

    #[test]
    fn multi_major_migration_chains_transforms_in_order() {
        // A two-major gap (0.x -> target 2.0) must chain both steps in order.
        let (_d, root) = store_at_version("0.5");
        let mut registry = TransformRegistry::new();
        registry.register(Box::new(MarkerTransform { from: 0 }));
        registry.register(Box::new(MarkerTransform { from: 1 }));
        let out = migrate_store_to(
            &root,
            (2, 0),
            &registry,
            &fast_config(Force::Deny),
            &mut SilentProgress,
        )
        .unwrap();
        assert_eq!(
            out,
            Outcome::Migrated {
                from: (0, 5),
                to: (2, 0),
                steps: 2
            }
        );
        assert!(root.join("transformed-from-0.marker").is_file());
        assert!(root.join("transformed-from-1.marker").is_file());
        assert_eq!(
            meta::read(&root).unwrap().store_version(),
            Some(StoreVersion::Clean { major: 2, minor: 0 })
        );
    }

    #[test]
    fn missing_transform_for_a_major_gap_is_an_error() {
        let (_d, root) = store_at_version("0.0");
        // No transform registered for the required 0 -> 1 step.
        let err = migrate_store_to(
            &root,
            (1, 0),
            &TransformRegistry::new(),
            &fast_config(Force::Deny),
            &mut SilentProgress,
        )
        .unwrap_err();
        assert!(matches!(err, MigrateError::MissingTransform { .. }));
        // The sentinel is left set (the store stays read-only until re-run),
        // and the backup is preserved to resume from.
        assert!(meta::read(&root)
            .unwrap()
            .store_version()
            .unwrap()
            .is_migrating());
        assert!(sibling(&root, BACKUP_SUFFIX).is_dir());
    }

    #[test]
    fn drain_waits_for_a_planted_temp_then_proceeds() {
        let (_d, root) = store_at_version("0.0");
        // Plant an in-flight staging temp file under the store.
        let temp = root.join("proj").join(".7.md.tmp");
        std::fs::write(&temp, b"").unwrap();

        // Clear it shortly, on another thread, well inside the drain timeout.
        let temp_clone = temp.clone();
        let remover = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            let _ = std::fs::remove_file(&temp_clone);
        });

        let mut registry = TransformRegistry::new();
        registry.register(Box::new(MarkerTransform { from: 0 }));
        let out = migrate_store_to(
            &root,
            (1, 0),
            &registry,
            &fast_config(Force::Deny),
            &mut SilentProgress,
        )
        .unwrap();
        remover.join().unwrap();
        assert!(matches!(out, Outcome::Migrated { .. }));
        assert!(!temp.exists());
    }

    #[test]
    fn drain_times_out_when_a_temp_never_clears() {
        let (_d, root) = store_at_version("0.0");
        // A temp that is never removed forces the drain to time out.
        std::fs::write(root.join("proj").join(".7.md.tmp"), b"").unwrap();
        let mut registry = TransformRegistry::new();
        registry.register(Box::new(MarkerTransform { from: 0 }));
        let err = migrate_store_to(
            &root,
            (1, 0),
            &registry,
            &fast_config(Force::Deny),
            &mut SilentProgress,
        )
        .unwrap_err();
        assert!(matches!(err, MigrateError::DrainTimedOut));
    }

    #[test]
    fn force_proceeds_past_a_stuck_drain() {
        let (_d, root) = store_at_version("0.0");
        std::fs::write(root.join("proj").join(".7.md.tmp"), b"").unwrap();
        let mut registry = TransformRegistry::new();
        registry.register(Box::new(MarkerTransform { from: 0 }));
        let out = migrate_store_to(
            &root,
            (1, 0),
            &registry,
            &fast_config(Force::Force),
            &mut SilentProgress,
        )
        .unwrap();
        assert!(matches!(out, Outcome::Migrated { .. }));
    }

    #[test]
    fn a_failed_migration_is_rerunnable_to_completion() {
        // First run fails (no transform), leaving the sentinel and a backup.
        let (_d, root) = store_at_version("0.0");
        let err = migrate_store_to(
            &root,
            (1, 0),
            &TransformRegistry::new(),
            &fast_config(Force::Deny),
            &mut SilentProgress,
        )
        .unwrap_err();
        assert!(matches!(err, MigrateError::MissingTransform { .. }));
        assert!(meta::read(&root)
            .unwrap()
            .store_version()
            .unwrap()
            .is_migrating());

        // Re-running with the transform now available resumes from the preserved
        // copy and completes to a clean store.
        let mut registry = TransformRegistry::new();
        registry.register(Box::new(MarkerTransform { from: 0 }));
        let out = migrate_store_to(
            &root,
            (1, 0),
            &registry,
            &fast_config(Force::Deny),
            &mut SilentProgress,
        )
        .unwrap();
        assert!(matches!(out, Outcome::Migrated { steps: 1, .. }));
        assert!(root.join("transformed-from-0.marker").is_file());
        assert_eq!(
            meta::read(&root).unwrap().store_version(),
            Some(StoreVersion::Clean { major: 1, minor: 0 })
        );
    }

    #[test]
    fn blocked_by_v0_to_v1_splits_map_into_list_and_reason() {
        // A staged major-0 node carrying the coupled blocked-by map is rewritten
        // to the decoupled major-1 shape: a plain ref list plus block-reason.
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("store");
        std::fs::create_dir_all(stage.join("e")).unwrap();
        std::fs::write(stage.join("e").join("epic.md"), "---\nid: e\n---\n").unwrap();
        let old = "---\n\
             number: 1\n\
             name: t\n\
             summary: s\n\
             status: blocked\n\
             blocked-by:\n  refs:\n  - e/2\n  - other/3\n  reason: waiting on a key\n\
             created: 2024-01-01T00:00:00Z\n\
             updated: 2024-01-01T00:00:00Z\n\
             ---\nbody\n";
        std::fs::write(stage.join("e").join("1.md"), old).unwrap();

        BlockedByV0ToV1.apply(&stage).unwrap();

        let out = std::fs::read_to_string(stage.join("e").join("1.md")).unwrap();
        let node = crate::model::NodeFile::parse(&out).unwrap();
        assert_eq!(node.frontmatter.blocked_by, vec!["e/2", "other/3"]);
        assert_eq!(
            node.frontmatter.block_reason.as_deref(),
            Some("waiting on a key")
        );
        // The body is preserved verbatim.
        assert_eq!(node.body, "body\n");
    }

    #[test]
    fn blocked_by_v0_to_v1_leaves_files_without_the_old_map_untouched() {
        // A node with no blocked-by (the common case) is byte-for-byte unchanged.
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("store");
        std::fs::create_dir_all(stage.join("e")).unwrap();
        std::fs::write(stage.join("e").join("epic.md"), "---\nid: e\n---\n").unwrap();
        let node_text = "---\nnumber: 1\nname: t\nstatus: to-do\n---\nbody\n";
        std::fs::write(stage.join("e").join("1.md"), node_text).unwrap();

        BlockedByV0ToV1.apply(&stage).unwrap();

        let out = std::fs::read_to_string(stage.join("e").join("1.md")).unwrap();
        assert_eq!(out, node_text);
    }

    #[test]
    fn crash_recovery_resumes_from_a_left_sentinel_and_backup() {
        // Simulate a store that crashed mid-migration: the sentinel is set and a
        // preserved backup at the old version sits beside it, but the live tree
        // was never transformed/committed.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        std::fs::create_dir_all(root.join("proj")).unwrap();
        std::fs::write(root.join("proj").join("epic.md"), "---\nid: proj\n---\n").unwrap();
        // Live store carries the sentinel (dirty marker) toward target 1.0.
        meta::write(&root, &Meta::migrating(1, 0)).unwrap();
        // The preserved backup is a clean old-major (0.x) store.
        let backup = sibling(&root, BACKUP_SUFFIX);
        copy_dir(&root, &backup).unwrap();
        meta::write(&backup, &Meta::clean(0, 0)).unwrap();

        let mut registry = TransformRegistry::new();
        registry.register(Box::new(MarkerTransform { from: 0 }));
        let out = migrate_store_to(
            &root,
            (1, 0),
            &registry,
            &fast_config(Force::Deny),
            &mut SilentProgress,
        )
        .unwrap();
        assert!(matches!(out, Outcome::Migrated { steps: 1, .. }));
        // The resumed run rebuilt from the backup: the transform breadcrumb is
        // present and the committed version is clean.
        assert!(root.join("transformed-from-0.marker").is_file());
        assert_eq!(
            meta::read(&root).unwrap().store_version(),
            Some(StoreVersion::Clean { major: 1, minor: 0 })
        );
    }
}
