# `loti` — Core specification

**`loti`** (LOcal TIckets) is a markdown-backed local ticketing system driven by
a CLI, usable autonomously by AI agents and legible to a human monitoring them.

This document is **normative** (MUST/SHOULD/MAY per RFC 2119). It specifies the
architecture the CLI operates on: the domain model, the on-disk storage format,
concurrency and multi-actor safety, and format versioning/migration. The
command-line interface itself — grammar, read/output formats, filtering, and the
skill/help system — is specified in [`cli-spec.md`](cli-spec.md).

---

## 1. Overview & scope

- **Goal.** A local, markdown-backed ticket tracker operated entirely through a
  `loti` CLI. Two audiences: **AI agents** operating autonomously, and **one
  human** monitoring/steering them. The store is plain, greppable markdown so a
  human can inspect it directly.
- **Non-goals.**
  - **Implementation language.** The spec is language-agnostic: it constrains
    behaviour and on-disk format, not the implementing language or toolchain.
  - **Security / access control.** Attribution is *cooperative*, not enforced;
    `loti` protects hygiene and audit trails, not against a hostile actor.
  - **Networked / multi-host use.** Semantics are for a single local POSIX
    filesystem (see *Concurrency & multi-actor safety*, precondition C10).

---

## 2. Domain model

- **Entities.** An **epic** is the top-level unit of work (epics do not nest,
  addressed by an **id**). A **ticket** belongs to an epic. A **subticket** breaks
  a ticket down and may nest recursively. **Node** = any ticket/subticket at any
  depth.
- **Numbering.** Every node draws a number from a **single flat monotonic pool per
  epic**. Numbers are unique within an epic, MAY collide across epics, and MUST
  NOT be reused.
- **Reference.** `<epic-id>/<n>` addresses exactly one node regardless of depth.
  Parent/child is metadata, never encoded in the number.
- **Node states** (exactly one): `to-do`, `in-progress`, `blocked`, `done`,
  `closed`. `done` and `closed` are **terminal** ("resolved").
  - A node MAY become `done` only when **all** descendants are terminal (a
    `closed` descendant counts as resolved).
  - `closed` carries a **reason**. Closing a node resolves only that node by
    default and leaves any non-terminal descendants untouched (so it can be
    reopened without having rewritten its subtree); cascade MAY be requested to
    close the descendants too.
  - `blocked` carries a non-empty free-form **block-reason** (an empty reason is
    refused), exactly as `closed` carries a close-reason. `loti` never
    sets/clears `blocked` automatically. Leaving `blocked` clears the
    block-reason; likewise leaving `closed` clears the close-reason (no
    non-blocked/non-closed node carries the respective reason).
- **Blocked-by (dependency list).** A node MAY carry **blocked-by**: an ordered,
  deduplicated list of canonical `<epic-id>/<n>` references to the tickets that
  block it. It is an advisory dependency annotation **independent of status** —
  it never gates or is changed by a state transition, and a node in any state
  may carry it. Each blocker MUST reference an existing node (its own state is
  irrelevant; cross-epic blockers are allowed) and MUST NOT be the node itself;
  no cycle check is performed. It exists on nodes only, never on epics.
- **Epic states.** `closed` is an explicit stored flag (optional reason,
  reversible) and takes precedence. `completed` (computed) = ≥1 node and all nodes
  terminal. `open` (computed) = any other case.
- **Standard fields.** name, summary, body, assets, labels, status,
  comments; epics also have an id. Labels carry no intrinsic semantics.
- **Attribution.** The **actor** is either *the human* (exactly one, unnamed) or a
  *named agent*. Attribution is required **only for comment operations**; every
  other operation is actor-agnostic. **Comments are the sole attribution
  channel** — attribute an asset or status change by adding a comment.
- **Comments.** Appended to an epic or node; carry an author; editable/soft-
  deletable **only by their own author**; deleted ones hidden unless explicitly
  requested.

---

## 3. On-disk storage format

- **Layout.** One flat directory per epic under the data root:
  `<epic-id>/epic.md` (the epic) and `<epic-id>/<n>.md` (each node). **No nested
  folders** — identity is decoupled from location; reparenting is a one-field
  edit. The tree is encoded by a `parent:` frontmatter field (absent = top-level).
- **File structure.** **All** structured data lives in **YAML frontmatter**:
  scalars (`number`, `name`, `summary`, `status`, `labels`, `parent`,
  `created`/`updated` timestamps), `blocked-by` (a list of canonical
  `<epic-id>/<n>` refs), the assets index, and the comments list (comment text
  as `|` literal block scalars). The entire region **below** the frontmatter is
  the free-form **body**, with **no managed sections**. Timestamps are ISO-8601
  UTC. A `blocked` node carries a `block-reason`; a terminally-closed node/epic
  carries a `close-reason`.
- **Numbering.** A monotonic **`next-number`** counter in `epic.md`. Allocation
  MUST use **probe-forward atomic exclusive-create** (`O_CREAT|O_EXCL`) of the
  complete node file, followed by a **best-effort** counter bump. Correctness
  comes from the exclusive create; the counter is a hint (stale-low self-heals by
  probing forward). This pre-resolves the node-creation race.
- **Comment ids.** Per node, `max(existing)+1`. No stored counter — safe via
  soft-delete (comments are never hard-deleted) plus single-file
  read-modify-write; ids are stable and monotonic.
- **Attachments.** Copied in **verbatim** to lazily-created companion dirs
  `<n>/` (nodes) and `epic/` (epic); indexed in frontmatter. URL/soft references
  are expressed as comments, not assets.
- **Store metadata.** `<root>/.loti/meta` (TOML) carries the store `format-version`
  (see *Format versioning & migration*). Written by `loti init`.
- **Root discovery.** Git-like upward walk to the nearest ancestor with a
  `.loti/` directory (the data root) or a `.loti.conf` file (TOML; `loti-root` key,
  absolute or relative-to-conf; also carries `[match-impl.<name>]` matcher config,
  specified in [`cli-spec.md`](cli-spec.md) → *Filtering model for `list`*).
  `.loti.conf` wins if both are present at one level; warn if they disagree.
  Override via `--root` only (no env var). Every verb resolves its store this
  way, from any depth.
- **Initialisation.** `init` creates the store *for the current directory*. Its
  files land in place (`.loti/` here) by default, or at a target named by
  `--root` or the positional `<dir>` (equivalent; naming both is an error), in
  which case a `.loti.conf` pointer is written here so this scope discovers it.
  `init` refuses when the current scope already resolves a store by the upward
  walk (marker or pointer, here or in any ancestor), so a nested `init` can
  never shadow or strand the enclosing store.

---

## 4. Concurrency & multi-actor safety

Scope: concurrent *edits* to existing files (node-creation races are pre-resolved
by the *On-disk storage format* numbering rules). Posture is **hybrid** — the
spec mandates the cheap universal primitive and the coordination *algorithm*, and
mandates only outcomes/invariants (with required defaults) for higher-level
policy.

- **Atomic writes (MUST).** Every file mutation writes to a **same-directory temp
  file** then atomically `rename`s over the target. `fsync`-before-rename is
  SHOULD.
- **Temp file = deterministic advisory lock.** The temp name is **deterministic**
  (e.g. `.<n>.md.tmp`), created via `O_CREAT|O_EXCL`, so it is both lock and
  staging file. It MUST be acquired **before the read**, bracketing the whole
  read-modify-write, and is released atomically by the `rename`.
  **Ordering (MUST, day-one — see *Format versioning & migration*):** acquire the
  lock **first**, *then* read and verify `format-version`, aborting (and
  releasing) if the major is unknown or the migration sentinel is set.
- **Acquire loop.** temp absent → acquire; temp present & **stale** (mtime past
  threshold) → fail fast, suggest `--force`; temp present & **fresh** → retry at a
  fixed interval, re-checking mtime, failing if it ages past threshold mid-wait.
  `--force` removes the stale temp and re-acquires via `O_CREAT|O_EXCL`. mtime is
  the liveness heartbeat.
- **Cascade / multi-file ops.** **No global lock.** A cascade is per-file
  independent RMW in **ascending node-number order** (deadlock-free), **not**
  globally atomic, **idempotent/re-runnable**, reporting partial progress on
  failure.
- **Counter-bump asymmetry.** `next-number` bumps do a **single non-blocking
  acquire and skip silently** on collision (self-healing per the *On-disk storage
  format* numbering rules); epic *edits* use the full retry loop.
- **Direct hand-edits.** Guarantees hold **among `loti` operations only.**
  Concurrent raw-editor writes during a live `loti` mutation are **last-write-
  wins** (stated, not prevented).
- **Reads are lock-free.** Single-file reads are atomic old-or-new; multi-file
  reads/aggregates are **not** a consistent global snapshot.
- **Tunables.** The algorithm + invariant are mandated
  (`interval` ≪ `threshold`; `threshold` > healthy hold time); values are
  implementation-defined (recommended: threshold **1s**, retry interval
  **~50–100ms**).
- **Precondition C10 (MUST).** POSIX-compatible local-filesystem semantics
  (atomic same-dir `rename`, atomic `O_CREAT|O_EXCL`). **NFS/SMB are unsupported**
  for concurrent multi-actor use. Windows is an implementation mapping obligation
  (OS-native atomic-replace), outside this spec's semantic guarantees.

---

## 5. Format versioning & migration

- **Version.** The store carries **exactly one** `format-version` at **store
  granularity**, in `<root>/.loti/meta` (TOML), written by `loti init`.
- **Scheme: `major.minor`.** **major** = breaking (rename/remove/restructure
  fields, layout change); **minor** = additive-only (new *optional* fields),
  compatible both directions within a major.
  - **Tolerant-reader (MUST):** unknown optional frontmatter keys are
    **preserved-and-ignored**, never an error. A writer MUST **round-trip
    (preserve)** keys it does not understand.
  - **Prefer-additive:** evolve via minor changes; reserve major bumps + migration
    for genuinely breaking change.
- **Mismatch behaviour on open** (fail-safe both directions):
  - **store major > binary major** → **refuse everything** (hard error, "upgrade
    `loti`"); never read-guess, never write.
  - **store major < binary major** → **reads OK** (optional warning); **any
    mutation refused** with a message to **run `loti migrate-store`**. **No
    auto-migrate on read.**
  - **minor difference within a major** → compatible (tolerant reader); no gate, no
    migration.
  - **equal** → normal.
- **`loti migrate-store` — sentinel-barrier protocol** (quiescence via the format,
  **no global lock**):
  - **Minor migration:** meta-only version bump; **no store rewrite**.
  - **Major migration**, in order: (1) set `.loti/meta` to the intermediate
    **`M+1.m-migrate` sentinel**; (2) **drain** — wait until no `*.tmp` (see
    *Concurrency & multi-actor safety*) exists; (3) **snapshot → transform →
    replace** the store directory (chained ordered steps); (4) write the clean
    **`M+1.m`** version — the **commit point**.
  - **Sentinel semantics (MUST):** any binary observing the `-migrate` sentinel —
    including a matching-major one — refuses mutations ("mid-migration, read-only
    for everyone but the migrator").
  - **Lock-then-verify ordering (MUST, see *Concurrency & multi-actor safety*):**
    makes `*.tmp` existence a complete signal of in-flight edits, so the drain is
    airtight — an edit either fully preceded the flip (drain waits; snapshot
    captures it) or re-reads the flipped meta and aborts. Closes the
    read-meta→create-tmp TOCTOU.
  - **Crash recovery:** the sentinel doubles as a **dirty marker** — a
    dead migration leaves the store read-only (all binaries refuse mutations); re-
    running `migrate-store` resumes/redoes from the preserved old copy.
- **Implementation-defined:** the concrete snapshot/replace technique
  (copy-and-swap vs in-place+backup vs journaled), old-copy retention/naming,
  `*.tmp` drain timeout/`--force` policy, progress reporting.
