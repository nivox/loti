//! The multi-actor safety primitive: atomic writes bracketed by a
//! deterministic temp-file advisory lock.
//!
//! Every mutation routes through here. The discipline, in one place so it
//! cannot be sidestepped:
//!
//!   * A mutation never writes its target in place. It writes a sibling temp
//!     file in the same directory, flushes it to disk, then atomically renames
//!     it over the target. The rename is the single visible instant of change:
//!     a concurrent reader sees either the whole old file or the whole new one.
//!   * The temp file's name is deterministic, derived from the target
//!     (`7.md` → `.7.md.tmp`, `epic.md` → `.epic.md.tmp`). It is created with
//!     exclusive-create semantics, so its mere existence is the advisory lock
//!     on that target: a second actor cannot create the same temp file while
//!     the first holds it. The final rename consumes the temp, releasing the
//!     lock atomically.
//!   * The lock is acquired *before* the target is read, so the whole
//!     read-modify-write is bracketed. This ordering also lets the version
//!     gate run under the lock: the lock is held, then the store version is
//!     verified, before any read — closing the window where a store could be
//!     migrated out from under an in-flight edit.
//!
//! These guarantees hold among cooperating `loti` operations only. A raw
//! editor writing the target during a live mutation is last-write-wins; that
//! is stated, not prevented.
//!
//! Reads are deliberately lock-free: a single-file read is atomic old-or-new
//! by virtue of the rename above, and multi-file aggregates are explicitly not
//! a consistent global snapshot.
//!
//! This module owns the primitive and the version gate hook; it does not
//! implement store migration. The migration sentinel check is a documented
//! seam: `VersionGate` refuses a store whose major is newer than this binary,
//! and leaves a clear extension point for the in-progress-migration marker.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use thiserror::Error;

/// Recommended liveness threshold: a temp file whose mtime is older than this
/// is treated as abandoned (its owner is presumed dead), not as a live hold.
pub const DEFAULT_STALE_THRESHOLD: Duration = Duration::from_secs(1);

/// Recommended retry interval while waiting on a fresh (live) lock. The
/// invariant `interval` ≪ `threshold` keeps a wait responsive without busy-
/// spinning; a healthy hold is released well inside the threshold.
pub const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// Tunables for the acquire loop. Defaults follow the recommended values;
/// tests shrink them to keep timing deterministic and fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockConfig {
    /// A temp file older than this (by mtime) is stale — its owner is presumed
    /// dead and the lock is recoverable with `force`.
    pub stale_threshold: Duration,
    /// How long to sleep between retries while a fresh lock is held elsewhere.
    pub retry_interval: Duration,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            stale_threshold: DEFAULT_STALE_THRESHOLD,
            retry_interval: DEFAULT_RETRY_INTERVAL,
        }
    }
}

impl LockConfig {
    /// The invariant every configuration must uphold: the retry interval is
    /// strictly smaller than the stale threshold, so a waiter re-checks
    /// liveness several times before declaring a hold abandoned.
    pub fn is_valid(&self) -> bool {
        self.retry_interval < self.stale_threshold && !self.stale_threshold.is_zero()
    }
}

/// Failure to acquire a lock or complete an atomic write.
#[derive(Debug, Error)]
pub enum LockError {
    /// The target is locked by a hold that still looks alive (its temp file is
    /// fresh) and it did not clear within the wait budget.
    #[error("{path} is locked by another operation; retry shortly")]
    Busy {
        /// The target whose lock could not be taken.
        path: PathBuf,
    },
    /// The target's temp file is stale — a previous operation likely died
    /// holding it. Recoverable by re-running with force.
    #[error(
        "{path} has a stale lock from an interrupted operation; \
         re-run with --force to clear it"
    )]
    Stale {
        /// The target whose stale lock blocks acquisition.
        path: PathBuf,
    },
    /// An I/O operation against a lock or target path failed.
    #[error("accessing {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

/// Whether to forcibly clear a stale lock before acquiring.
///
/// `Deny` is the safe default: a stale lock fails fast so an operator decides.
/// `Force` removes an abandoned temp file and re-acquires, and is how a
/// user-facing `--force` flag plumbs in without this module knowing about the
/// CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Force {
    /// Fail fast on a stale lock.
    Deny,
    /// Clear a stale lock and re-acquire.
    Force,
}

/// The deterministic temp-file name for a target: a leading dot, the file
/// name, and a `.tmp` suffix, kept in the same directory as the target so the
/// rename is same-directory (and therefore atomic).
///
/// Returns `None` for a path without a final file-name component.
pub fn temp_path(target: &Path) -> Option<PathBuf> {
    let name = target.file_name()?.to_string_lossy();
    let temp_name = format!(".{name}.tmp");
    Some(match target.parent() {
        Some(dir) => dir.join(temp_name),
        None => PathBuf::from(temp_name),
    })
}

/// A held advisory lock on a target: the exclusively-created temp file that is
/// also the staging file for the pending write.
///
/// Dropping the guard removes the temp file, releasing the lock — unless the
/// guard was consumed by [`TempLock::commit`], which renames the temp over the
/// target (an atomic release-and-publish). This RAII release is what makes a
/// failed or panicking mutation leave no lingering lock.
#[derive(Debug)]
pub struct TempLock {
    target: PathBuf,
    temp: PathBuf,
    /// Cleared once the temp has been renamed onto the target, so `Drop` does
    /// not try to remove a file that no longer exists under this name.
    active: bool,
}

impl TempLock {
    /// The target this lock guards.
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// The staging temp-file path (the lock file).
    pub fn temp(&self) -> &Path {
        &self.temp
    }

    /// Stage bytes into the temp file, flush them to durable storage, then
    /// atomically rename the temp over the target. The rename releases the
    /// lock and publishes the new contents in one indivisible step.
    ///
    /// Durability: the temp file is flushed and fsynced before the rename, so
    /// a crash cannot leave a renamed-but-empty target.
    pub fn commit(mut self, bytes: &[u8]) -> Result<(), LockError> {
        // The temp file was created empty at acquire time; open it for writing
        // its staged contents. Truncate defensively in case of a re-commit.
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.temp)
            .map_err(|source| LockError::Io {
                path: self.temp.clone(),
                source,
            })?;
        file.write_all(bytes).map_err(|source| LockError::Io {
            path: self.temp.clone(),
            source,
        })?;
        // fsync-before-rename: the staged bytes must be on disk before the
        // rename makes them the target, or a crash could publish a torn file.
        file.sync_all().map_err(|source| LockError::Io {
            path: self.temp.clone(),
            source,
        })?;
        drop(file);

        fs::rename(&self.temp, &self.target).map_err(|source| LockError::Io {
            path: self.target.clone(),
            source,
        })?;
        // The temp no longer exists under its own name; suppress Drop cleanup.
        self.active = false;
        Ok(())
    }

    /// Release the lock without publishing anything, removing the temp file.
    /// Equivalent to dropping the guard, but surfaces an I/O error.
    pub fn abort(mut self) -> Result<(), LockError> {
        self.active = false;
        fs::remove_file(&self.temp).map_err(|source| LockError::Io {
            path: self.temp.clone(),
            source,
        })
    }
}

impl Drop for TempLock {
    fn drop(&mut self) {
        // A lock that was neither committed nor explicitly aborted is being
        // released by scope exit (including a panic). Remove the temp so the
        // lock does not outlive the operation that took it. Errors here cannot
        // be propagated from Drop and are intentionally ignored.
        if self.active {
            let _ = fs::remove_file(&self.temp);
        }
    }
}

/// Try to create the temp file exclusively. `Ok(Some(lock))` on success,
/// `Ok(None)` if it already exists (someone else holds the lock), `Err` on any
/// other I/O failure.
fn try_create(target: &Path, temp: &Path) -> Result<Option<TempLock>, LockError> {
    match OpenOptions::new().write(true).create_new(true).open(temp) {
        Ok(_) => Ok(Some(TempLock {
            target: target.to_path_buf(),
            temp: temp.to_path_buf(),
            active: true,
        })),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(source) => Err(LockError::Io {
            path: temp.to_path_buf(),
            source,
        }),
    }
}

/// Attempt to acquire the advisory lock on `target` in a single non-blocking
/// step: create the temp file exclusively and return the lock, or return
/// `Ok(None)` if it is already held. Unlike [`acquire`], this never retries and
/// never treats a stale hold specially — it is one exclusive-create attempt.
///
/// This is the acquire discipline for a hint-only mutation that must never
/// block: a caller that fails to take the lock skips its update silently,
/// because the value it would write is only an optimisation that self-heals.
pub fn try_acquire(target: &Path) -> Result<Option<TempLock>, LockError> {
    let temp = temp_path(target).ok_or_else(|| LockError::Io {
        path: target.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target has no file name to derive a lock from",
        ),
    })?;
    try_create(target, &temp)
}

/// Whether an existing temp file is stale: its mtime is older than the stale
/// threshold, so its owner is presumed dead. A temp file we cannot stat (it
/// vanished mid-check) is treated as not-stale so the caller retries cleanly.
fn is_stale(temp: &Path, config: &LockConfig) -> bool {
    let Ok(meta) = fs::metadata(temp) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    modified
        .elapsed()
        .map(|age| age > config.stale_threshold)
        .unwrap_or(false)
}

/// Acquire the advisory lock on `target`, bracketing a read-modify-write.
///
/// The acquire loop:
///   * temp absent → create it exclusively and return the lock;
///   * temp present and stale → fail fast (or clear it under `Force`);
///   * temp present and fresh → wait a retry interval and re-check, until the
///     hold clears (then acquire) or ages into staleness (then fail),
///     bounded by the stale threshold so a waiter never blocks forever.
///
/// mtime is the liveness heartbeat; a live owner is expected to complete and
/// rename well inside the threshold.
pub fn acquire(target: &Path, config: &LockConfig, force: Force) -> Result<TempLock, LockError> {
    debug_assert!(
        config.is_valid(),
        "lock config must keep retry interval below the stale threshold"
    );
    let temp = temp_path(target).ok_or_else(|| LockError::Io {
        path: target.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target has no file name to derive a lock from",
        ),
    })?;

    // Bound the whole wait so a fresh-but-never-released hold cannot hang us:
    // a lock still held past the staleness threshold is declared stale.
    let deadline = Instant::now() + config.stale_threshold;
    loop {
        if let Some(lock) = try_create(target, &temp)? {
            return Ok(lock);
        }

        // The temp exists: decide whether it is a live hold or debris.
        if is_stale(&temp, config) {
            match force {
                Force::Force => {
                    // Clear the abandoned temp and re-acquire exclusively. A
                    // race to clear is benign: whoever wins the re-create wins
                    // the lock; the loser retries.
                    match fs::remove_file(&temp) {
                        Ok(()) => continue,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(source) => {
                            return Err(LockError::Io {
                                path: temp.clone(),
                                source,
                            })
                        }
                    }
                }
                Force::Deny => {
                    return Err(LockError::Stale {
                        path: target.to_path_buf(),
                    })
                }
            }
        }

        // A fresh hold: wait and re-check. If it never clears within the
        // threshold it will read as stale on a later pass (Force) or the
        // deadline trips (Deny).
        if Instant::now() >= deadline {
            return Err(LockError::Busy {
                path: target.to_path_buf(),
            });
        }
        std::thread::sleep(config.retry_interval);
    }
}

/// Verifies the store is safe to mutate before any read happens under a lock.
///
/// The gate is consulted while the lock is held and before the target is read,
/// so a store cannot be migrated out from under an in-flight edit. It refuses a
/// store whose major version is newer than this binary understands, a store
/// whose major is older (read-only until migrated), and a store carrying the
/// mid-migration sentinel (read-only for everyone but the migrator).
pub trait VersionGate {
    /// Return `Ok(())` if mutation may proceed, or a reason to refuse.
    fn verify(&self) -> Result<(), VersionRefusal>;
}

/// The version a store records, as the gate sees it: a settled `(major, minor)`
/// or a mid-migration sentinel toward `(major, minor)`. Mirrors the metadata
/// layer's own reading so the gate never has to know the on-disk encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedVersion {
    /// A settled store at this `(major, minor)`.
    Clean(u32, u32),
    /// A store mid-migration toward this `(major, minor)`: read-only for
    /// everyone but the migrator.
    Migrating(u32, u32),
}

/// Apply the full mismatch matrix to an observed store version against this
/// binary's version. Shared by both the mutation gate and the read-time check
/// so the two can never drift:
///   * a mid-migration sentinel refuses mutation for everyone but the migrator;
///   * a store major newer than the binary is refused outright;
///   * a store major older than the binary is read-only until migrated;
///   * within a major, minor differences are compatible either direction.
fn classify_mutation(
    observed: Option<ObservedVersion>,
    binary_major: u32,
) -> Result<(), VersionRefusal> {
    let (store_major, migrating) = match observed {
        Some(ObservedVersion::Clean(major, _)) => (major, false),
        Some(ObservedVersion::Migrating(major, _)) => (major, true),
        None => return Err(VersionRefusal::Unreadable),
    };
    if migrating {
        // The sentinel doubles as a dirty-marker: any binary that sees it,
        // including a matching-major one, treats the store as read-only until
        // the migration commits or is re-run.
        return Err(VersionRefusal::MigrationInProgress);
    }
    if store_major > binary_major {
        Err(VersionRefusal::StoreTooNew)
    } else if store_major < binary_major {
        Err(VersionRefusal::NeedsMigration)
    } else {
        Ok(())
    }
}

/// The subset of the matrix that gates *reads*. Reads are otherwise lock-free,
/// but a store whose major is newer than this binary must never be read (the
/// binary cannot be trusted to interpret it). An older major and a
/// mid-migration sentinel are both still readable — only mutation is refused
/// for those — so this returns `Ok` for them.
pub fn classify_read(
    observed: Option<ObservedVersion>,
    binary_major: u32,
) -> Result<(), VersionRefusal> {
    match observed {
        // A newer major is never safe to read.
        Some(ObservedVersion::Clean(major, _)) | Some(ObservedVersion::Migrating(major, _))
            if major > binary_major =>
        {
            Err(VersionRefusal::StoreTooNew)
        }
        None => Err(VersionRefusal::Unreadable),
        _ => Ok(()),
    }
}

/// Why a mutation was refused by the version gate.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VersionRefusal {
    /// The store was written by a newer major format than this binary supports.
    #[error("this store needs a newer loti; upgrade loti to work with it")]
    StoreTooNew,
    /// The store's major is older than this binary; it is read-only until
    /// migrated.
    #[error("this store uses an older format; run 'loti migrate-store' to update it")]
    NeedsMigration,
    /// A migration is in progress (or was interrupted): the store is read-only
    /// for everyone but the migrator until it completes.
    #[error("this store is mid-migration and read-only; finish 'loti migrate-store'")]
    MigrationInProgress,
    /// The store's version string could not be parsed.
    #[error("this store's format version is unreadable")]
    Unreadable,
}

/// A version gate that enforces the major-version rule against a fixed binary
/// version. Reads the store version once via the supplied closure.
///
/// Minor differences within a major are compatible in both directions and do
/// not gate. An equal or newer-minor-same-major store mutates normally.
pub struct MajorVersionGate<F> {
    /// This binary's `(major, minor)`.
    pub binary: (u32, u32),
    /// Reads the store's `(major, minor)`, or `None` if unreadable.
    pub read_store: F,
}

impl<F> VersionGate for MajorVersionGate<F>
where
    F: Fn() -> Option<(u32, u32)>,
{
    fn verify(&self) -> Result<(), VersionRefusal> {
        let observed =
            (self.read_store)().map(|(major, minor)| ObservedVersion::Clean(major, minor));
        classify_mutation(observed, self.binary.0)
    }
}

/// The full sentinel-aware mutation gate. Reads the store's observed version
/// (settled or mid-migration) via the supplied closure and applies the whole
/// mismatch matrix, including refusing mutation while the migration sentinel is
/// set. This is the gate every real store mutation is bracketed by.
pub struct StoreVersionGate<F> {
    /// This binary's `(major, minor)`.
    pub binary: (u32, u32),
    /// Reads the store's observed version, or `None` when it is unreadable.
    pub read_store: F,
}

impl<F> VersionGate for StoreVersionGate<F>
where
    F: Fn() -> Option<ObservedVersion>,
{
    fn verify(&self) -> Result<(), VersionRefusal> {
        classify_mutation((self.read_store)(), self.binary.0)
    }
}

/// Error from a bracketed read-modify-write: a lock/IO failure, a version
/// refusal, or an error from the caller's own read/transform step.
#[derive(Debug, Error)]
pub enum RmwError<E> {
    /// The lock could not be taken or the write could not be committed.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// The store version gate refused the mutation.
    #[error(transparent)]
    Version(#[from] VersionRefusal),
    /// The caller's read/transform step failed.
    #[error(transparent)]
    Op(E),
}

/// Perform a bracketed read-modify-write against a single target.
///
/// The ordering is fixed by this function's shape and cannot be reordered by a
/// caller: (1) acquire the lock, (2) verify the store version under the lock,
/// (3) only then read the target and let `transform` produce the new bytes,
/// (4) atomically commit them. The caller supplies a reader and a transform but
/// can never read before the lock is held and the version verified.
///
/// A `transform` that produces no bytes declines to publish: the lock is
/// released and the target is left exactly as it was found. A read-modify-write
/// that discovers there is nothing to change must not rewrite its target.
///
/// `read` receives the current file contents as `None` when the target does
/// not yet exist (a create), otherwise `Some(bytes)`.
pub fn rmw<T, E, R, X>(
    target: &Path,
    config: &LockConfig,
    force: Force,
    gate: &dyn VersionGate,
    read: R,
    transform: X,
) -> Result<T, RmwError<E>>
where
    R: FnOnce(Option<Vec<u8>>) -> Result<T, E>,
    X: FnOnce(&T) -> Result<Option<Vec<u8>>, E>,
{
    let lock = acquire(target, config, force)?;
    // Verify-after-lock: the gate runs while the lock is held and before the
    // read, so the store cannot change format under this operation.
    gate.verify()?;

    let current = match fs::read(target) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(RmwError::Lock(LockError::Io {
                path: target.to_path_buf(),
                source,
            }))
        }
    };

    let value = read(current).map_err(RmwError::Op)?;
    match transform(&value).map_err(RmwError::Op)? {
        Some(new_bytes) => lock.commit(new_bytes.as_slice())?,
        // Nothing to publish: release the lock without a rename, leaving the
        // target's bytes exactly as they were read.
        None => lock.abort()?,
    }
    Ok(value)
}

/// Atomically write `bytes` to `target` under the advisory lock, replacing any
/// existing contents. A convenience over [`rmw`] for a blind write that does
/// not depend on the current contents. Still lock-bracketed and version-gated.
pub fn atomic_write(
    target: &Path,
    bytes: &[u8],
    config: &LockConfig,
    force: Force,
    gate: &dyn VersionGate,
) -> Result<(), LockError> {
    let lock = acquire(target, config, force)?;
    // A blind write still respects the lock; the gate is the caller's concern
    // for reads, but a blind overwrite is refused on a too-new store too.
    if let Err(refusal) = gate.verify() {
        // Release the lock (via Drop) and surface the refusal as an I/O-shaped
        // error so this convenience keeps a single error type.
        return Err(LockError::Io {
            path: target.to_path_buf(),
            source: std::io::Error::other(refusal.to_string()),
        });
    }
    lock.commit(bytes)
}

/// The outcome of a cascade over an ordered set of targets: which committed and
/// which failed. A cascade is not globally atomic — it is a sequence of
/// independent single-file mutations — so partial progress is reported rather
/// than rolled back. Re-running a cascade is safe because each step is
/// idempotent by construction (the caller's transform must be).
#[derive(Debug, Default)]
pub struct CascadeReport {
    /// Targets whose mutation committed, in the order attempted.
    pub committed: Vec<PathBuf>,
    /// The first target that failed and why; a cascade stops at the first
    /// failure so the operator can inspect and re-run.
    pub failed: Option<(PathBuf, String)>,
}

impl CascadeReport {
    /// Whether every attempted target committed with no failure.
    pub fn is_complete(&self) -> bool {
        self.failed.is_none()
    }
}

/// Run an independent read-modify-write over each target, in the caller-
/// supplied order, stopping at the first failure and reporting partial
/// progress.
///
/// There is no global lock: each target is locked, mutated, and released on its
/// own. Callers pass targets in ascending node-number order so concurrent
/// cascades take locks in the same order and cannot deadlock. Because steps are
/// independent and idempotent, a cascade that stops partway can simply be
/// re-run to completion.
///
/// `step` is invoked per target; it performs that target's own `rmw` (or blind
/// write) and returns its result. A returned error stops the cascade.
pub fn cascade<I, S>(targets: I, mut step: S) -> CascadeReport
where
    I: IntoIterator<Item = PathBuf>,
    S: FnMut(&Path) -> Result<(), String>,
{
    let mut report = CascadeReport::default();
    for target in targets {
        match step(&target) {
            Ok(()) => report.committed.push(target),
            Err(reason) => {
                report.failed = Some((target, reason));
                break;
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gate that always permits mutation, for tests exercising the lock
    /// mechanics rather than the version rule.
    struct AlwaysOk;
    impl VersionGate for AlwaysOk {
        fn verify(&self) -> Result<(), VersionRefusal> {
            Ok(())
        }
    }

    /// Tiny tunables so timing tests never sleep for real seconds. The
    /// invariant interval ≪ threshold is preserved.
    fn fast_config() -> LockConfig {
        LockConfig {
            stale_threshold: Duration::from_millis(80),
            retry_interval: Duration::from_millis(5),
        }
    }

    #[test]
    fn deterministic_temp_naming() {
        assert_eq!(
            temp_path(Path::new("/data/e/7.md")).unwrap(),
            Path::new("/data/e/.7.md.tmp")
        );
        assert_eq!(
            temp_path(Path::new("/data/e/epic.md")).unwrap(),
            Path::new("/data/e/.epic.md.tmp")
        );
    }

    #[test]
    fn default_config_upholds_the_interval_threshold_invariant() {
        assert!(LockConfig::default().is_valid());
        assert!(fast_config().is_valid());
    }

    #[test]
    fn atomic_write_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("7.md");
        fs::write(&target, b"old").unwrap();
        atomic_write(
            &target,
            b"new contents",
            &fast_config(),
            Force::Deny,
            &AlwaysOk,
        )
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new contents");
        // The temp file is gone (consumed by the rename).
        assert!(!temp_path(&target).unwrap().exists());
    }

    #[test]
    fn atomic_write_creates_a_new_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("epic.md");
        atomic_write(&target, b"fresh", &fast_config(), Force::Deny, &AlwaysOk).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"fresh");
    }

    #[test]
    fn a_held_lock_blocks_a_second_acquire_on_the_same_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("7.md");
        // Hold the lock and keep the owner alive past the stale threshold
        // without releasing. A second acquire cannot take the same temp: it
        // retries while the hold looks fresh, then — mtime being the only
        // liveness signal — the un-refreshed hold ages into staleness and the
        // waiter fails fast (a hold exceeding healthy time is presumed dead).
        let held = acquire(&target, &fast_config(), Force::Deny).unwrap();
        let second = acquire(&target, &fast_config(), Force::Deny);
        assert!(
            matches!(
                second,
                Err(LockError::Stale { .. }) | Err(LockError::Busy { .. })
            ),
            "a second acquire on a held lock must be refused, got {second:?}"
        );
        drop(held);
    }

    #[test]
    fn dropping_the_guard_releases_the_lock_without_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("7.md");
        let temp = temp_path(&target).unwrap();
        {
            let _lock = acquire(&target, &fast_config(), Force::Deny).unwrap();
            assert!(temp.exists(), "temp file is the held lock");
        }
        // Drop removed the temp: the lock is released and nothing was published.
        assert!(!temp.exists());
        assert!(!target.exists());
    }

    #[test]
    fn abort_releases_the_lock_and_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("7.md");
        let lock = acquire(&target, &fast_config(), Force::Deny).unwrap();
        lock.abort().unwrap();
        assert!(!temp_path(&target).unwrap().exists());
        assert!(!target.exists());
    }

    #[test]
    fn commit_publishes_and_releases_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("7.md");
        let lock = acquire(&target, &fast_config(), Force::Deny).unwrap();
        lock.commit(b"published").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"published");
        assert!(!temp_path(&target).unwrap().exists());
    }

    #[test]
    fn a_stale_lock_fails_fast_and_force_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("7.md");
        let temp = temp_path(&target).unwrap();
        // Simulate an abandoned temp from a dead operation, aged past the
        // threshold.
        fs::write(&temp, b"").unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(5);
        filetime_set(&temp, old);

        // Without force, a stale lock fails fast.
        let denied = acquire(&target, &fast_config(), Force::Deny);
        assert!(matches!(denied, Err(LockError::Stale { .. })));

        // With force, the stale temp is cleared and the lock is taken.
        let lock = acquire(&target, &fast_config(), Force::Force).unwrap();
        lock.commit(b"recovered").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"recovered");
    }

    #[test]
    fn a_fresh_lock_is_waited_out_then_acquired_once_released() {
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("7.md");
        let config = fast_config();

        // Hold the lock briefly on another thread, then release it well inside
        // the stale threshold so the waiter should succeed by retrying.
        let held = acquire(&target, &config, Force::Deny).unwrap();
        let (tx, rx) = mpsc::channel();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            drop(held); // release without publishing
            tx.send(()).unwrap();
        });

        // This blocks-and-retries until the holder releases, then acquires.
        let lock = acquire(&target, &config, Force::Deny).unwrap();
        rx.recv().unwrap();
        releaser.join().unwrap();
        lock.commit(b"after wait").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"after wait");
    }

    #[test]
    fn rmw_reads_current_contents_and_commits_the_transform() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("7.md");
        fs::write(&target, b"count=1").unwrap();
        let observed: String = rmw(
            &target,
            &fast_config(),
            Force::Deny,
            &AlwaysOk,
            |current| -> Result<String, std::convert::Infallible> {
                Ok(String::from_utf8(current.unwrap()).unwrap())
            },
            |value| Ok(Some(format!("{value};count=2").into_bytes())),
        )
        .unwrap();
        assert_eq!(observed, "count=1");
        assert_eq!(fs::read(&target).unwrap(), b"count=1;count=2");
    }

    #[test]
    fn rmw_declining_to_publish_leaves_the_target_and_releases_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("7.md");
        fs::write(&target, b"as found").unwrap();
        rmw(
            &target,
            &fast_config(),
            Force::Deny,
            &AlwaysOk,
            |current| -> Result<(), std::convert::Infallible> {
                assert_eq!(current.unwrap(), b"as found");
                Ok(())
            },
            // A transform with nothing to change produces no bytes.
            |_| Ok(None),
        )
        .unwrap();
        assert_eq!(
            fs::read(&target).unwrap(),
            b"as found",
            "a declined transform must not rewrite the target"
        );
        assert!(
            !temp_path(&target).unwrap().exists(),
            "the lock is released even when nothing is published"
        );
    }

    #[test]
    fn rmw_sees_none_for_a_missing_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("new.md");
        rmw(
            &target,
            &fast_config(),
            Force::Deny,
            &AlwaysOk,
            |current| -> Result<(), std::convert::Infallible> {
                assert!(current.is_none(), "a missing target reads as None");
                Ok(())
            },
            |_| Ok(Some(b"created".to_vec())),
        )
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"created");
    }

    #[test]
    fn rmw_acquires_the_lock_before_reading() {
        // With the lock held elsewhere, rmw must not read/transform at all: it
        // blocks on acquire first. We assert the read closure never runs.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("7.md");
        fs::write(&target, b"data").unwrap();
        let _held = acquire(&target, &fast_config(), Force::Deny).unwrap();

        let mut read_ran = false;
        let result = rmw(
            &target,
            &fast_config(),
            Force::Deny,
            &AlwaysOk,
            |_| -> Result<(), std::convert::Infallible> {
                read_ran = true;
                Ok(())
            },
            |_| Ok(Some(Vec::new())),
        );
        assert!(
            matches!(
                result,
                Err(RmwError::Lock(LockError::Busy { .. }))
                    | Err(RmwError::Lock(LockError::Stale { .. }))
            ),
            "a held lock must block the rmw, got {result:?}"
        );
        assert!(
            !read_ran,
            "the read must not run until the lock is acquired"
        );
    }

    #[test]
    fn rmw_refuses_a_too_new_store_before_reading() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("7.md");
        fs::write(&target, b"data").unwrap();
        let gate = MajorVersionGate {
            binary: (1, 0),
            read_store: || Some((2, 0)),
        };
        let mut read_ran = false;
        let result = rmw(
            &target,
            &fast_config(),
            Force::Deny,
            &gate,
            |_| -> Result<(), std::convert::Infallible> {
                read_ran = true;
                Ok(())
            },
            |_| Ok(Some(Vec::new())),
        );
        assert!(matches!(
            result,
            Err(RmwError::Version(VersionRefusal::StoreTooNew))
        ));
        assert!(!read_ran, "the version gate runs before the read");
        // The target is untouched.
        assert_eq!(fs::read(&target).unwrap(), b"data");
    }

    #[test]
    fn major_version_gate_rules() {
        let older = MajorVersionGate {
            binary: (2, 3),
            read_store: || Some((1, 9)),
        };
        assert_eq!(older.verify(), Err(VersionRefusal::NeedsMigration));

        let newer = MajorVersionGate {
            binary: (1, 0),
            read_store: || Some((2, 0)),
        };
        assert_eq!(newer.verify(), Err(VersionRefusal::StoreTooNew));

        // Minor differences within a major are compatible either direction.
        let minor_ahead = MajorVersionGate {
            binary: (1, 0),
            read_store: || Some((1, 5)),
        };
        assert_eq!(minor_ahead.verify(), Ok(()));
        let minor_behind = MajorVersionGate {
            binary: (1, 5),
            read_store: || Some((1, 0)),
        };
        assert_eq!(minor_behind.verify(), Ok(()));

        let unreadable = MajorVersionGate {
            binary: (1, 0),
            read_store: || None,
        };
        assert_eq!(unreadable.verify(), Err(VersionRefusal::Unreadable));
    }

    #[test]
    fn store_version_gate_covers_the_full_matrix_including_sentinel() {
        // Newer major: refuse outright.
        let newer = StoreVersionGate {
            binary: (1, 0),
            read_store: || Some(ObservedVersion::Clean(2, 0)),
        };
        assert_eq!(newer.verify(), Err(VersionRefusal::StoreTooNew));

        // Older major: read-only until migrated.
        let older = StoreVersionGate {
            binary: (2, 0),
            read_store: || Some(ObservedVersion::Clean(1, 5)),
        };
        assert_eq!(older.verify(), Err(VersionRefusal::NeedsMigration));

        // Equal and minor-diff within a major: compatible.
        let equal = StoreVersionGate {
            binary: (1, 3),
            read_store: || Some(ObservedVersion::Clean(1, 3)),
        };
        assert_eq!(equal.verify(), Ok(()));
        let minor = StoreVersionGate {
            binary: (1, 3),
            read_store: || Some(ObservedVersion::Clean(1, 1)),
        };
        assert_eq!(minor.verify(), Ok(()));

        // A mid-migration sentinel refuses mutation even for a matching major.
        let migrating = StoreVersionGate {
            binary: (1, 3),
            read_store: || Some(ObservedVersion::Migrating(1, 3)),
        };
        assert_eq!(migrating.verify(), Err(VersionRefusal::MigrationInProgress));

        let unreadable = StoreVersionGate {
            binary: (1, 0),
            read_store: || None,
        };
        assert_eq!(unreadable.verify(), Err(VersionRefusal::Unreadable));
    }

    #[test]
    fn read_classification_only_refuses_a_newer_major() {
        // Newer major is never safe to read.
        assert_eq!(
            classify_read(Some(ObservedVersion::Clean(2, 0)), 1),
            Err(VersionRefusal::StoreTooNew)
        );
        assert_eq!(
            classify_read(Some(ObservedVersion::Migrating(2, 0)), 1),
            Err(VersionRefusal::StoreTooNew)
        );
        // Older major, equal, and a same-or-older mid-migration store all read.
        assert_eq!(classify_read(Some(ObservedVersion::Clean(1, 0)), 2), Ok(()));
        assert_eq!(classify_read(Some(ObservedVersion::Clean(1, 0)), 1), Ok(()));
        assert_eq!(
            classify_read(Some(ObservedVersion::Migrating(1, 0)), 1),
            Ok(())
        );
        // Unreadable is refused.
        assert_eq!(classify_read(None, 1), Err(VersionRefusal::Unreadable));
    }

    #[test]
    fn cascade_runs_in_order_and_reports_full_completion() {
        let dir = tempfile::tempdir().unwrap();
        let targets: Vec<PathBuf> = (1..=3)
            .map(|n| dir.path().join(format!("{n}.md")))
            .collect();
        let order = std::cell::RefCell::new(Vec::new());
        let report = cascade(targets.clone(), |t| {
            order.borrow_mut().push(t.to_path_buf());
            atomic_write(t, b"x", &fast_config(), Force::Deny, &AlwaysOk).map_err(|e| e.to_string())
        });
        assert!(report.is_complete());
        assert_eq!(report.committed, targets);
        assert_eq!(*order.borrow(), targets);
    }

    #[test]
    fn cascade_stops_at_the_first_failure_and_reports_partial_progress() {
        let dir = tempfile::tempdir().unwrap();
        let targets: Vec<PathBuf> = (1..=4)
            .map(|n| dir.path().join(format!("{n}.md")))
            .collect();
        let report = cascade(targets.clone(), |t| {
            // Fail on the third target; the fourth must never be attempted.
            if t.ends_with("3.md") {
                return Err("boom".to_string());
            }
            atomic_write(t, b"x", &fast_config(), Force::Deny, &AlwaysOk).map_err(|e| e.to_string())
        });
        assert!(!report.is_complete());
        assert_eq!(
            report.committed,
            vec![targets[0].clone(), targets[1].clone()]
        );
        let (failed, reason) = report.failed.unwrap();
        assert_eq!(failed, targets[2]);
        assert_eq!(reason, "boom");
        // The target after the failure was not touched.
        assert!(!targets[3].exists());
    }

    /// Backdate a file's mtime so a staleness test does not have to sleep past
    /// the threshold in real time.
    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        let f = OpenOptions::new().write(true).open(path).unwrap();
        let times = std::fs::FileTimes::new()
            .set_accessed(when)
            .set_modified(when);
        f.set_times(times).unwrap();
    }
}
