---
name: loti
description: Drive the loti local ticket tracker from its CLI — create and manage epics, tickets, and subtickets; set status; add comments, labels, and assets; and read work back. Use whenever you need to record, track, or report on units of work in this repository.
---

# loti — local ticket tracker

## What this is and when to use it

`loti` is a local, markdown-backed ticket tracker you operate entirely through
its command line. Use it to break work into epics and tickets, track their
state, attach evidence, and leave an attributed audit trail a human can follow.

Two audiences share one store: **agents** driving the work autonomously, and
**one human** monitoring and steering. Reach for `loti` whenever a task is worth
recording — planning a feature, tracking sub-tasks, or reporting progress — so
the human always has a legible picture of what happened and why.

## The one hard rule (read this first)

**Never touch the store files directly — for reading or writing. Every
operation goes through the `loti` CLI.**

The store is plain markdown so a **human can read it** — that legibility is for
human inspection only, never a path for a tool or agent to read or write. Every
correctness guarantee (numbering, state transitions, attribution, locking,
format versioning) is enforced by the CLI, **not** by the files. Editing a file
by hand — or writing one directly instead of running a command — silently breaks
those guarantees: duplicate or reused numbers, illegal state changes, corrupted
concurrent writes, lost audit trail.

Reading directly is banned too, not just as a courtesy: file paths, layout, and
on-disk format are internal and may change, and a read that sidesteps the CLI
teaches the wrong habit and invites the next step of writing directly. This
covers **assets** as well — never open, copy, or search an asset's file on
disk; read it back with `asset show` and change it with `asset update`.

So: **drive every operation, read and write alike, through a `loti` command.**
If a command to do what you need does not seem to exist, re-read the help — do
not reach for the files.

## Core concepts

Everything you need to work is here; you do not need any other document.

- **Epic** — the top-level unit of work (for example, one feature). Epics do not
  nest. Each is named by a human-chosen **epic id** (for example, `my-feature`).
- **Ticket** — a slice of work belonging to an epic.
- **Subticket** — a finer breakdown of a ticket; it has a parent and may itself
  have subtickets, to any depth. Create one with `ticket create <epic-id>
  --parent <ref>`.
- **Node** — any ticket or subticket, at any depth. Rules that say "node" apply
  regardless of level.
- **Number & reference.** Every node draws a number from a single flat, growing
  pool **per epic** — the next free number, never reused, unique within its epic
  (numbers may repeat *across* epics). A node is addressed as
  **`<epic-id>/<number>`** (for example, `my-feature/7`), which points to exactly
  one node whatever its depth. **Parent/child is metadata, never encoded in the
  number** — reparenting is a one-field edit, not a renumber.

### Node states

A node is in exactly one state:

- **to-do** — created, not started.
- **in-progress** — actively being worked.
- **blocked** — cannot proceed. Carries a structured **blocked-by** that must
  not be empty: at least one node reference and/or a free-form reason (for a
  non-ticket blocker, such as waiting on an external key), so a blocked node
  always says why. `loti` never sets or clears `blocked` for you — it is always
  an explicit choice.
- **done** — delivered successfully. Terminal.
- **closed** — resolved *without* completing (won't-do, cancelled, obsolete,
  duplicate, superseded). Carries a **reason**. Terminal.

**Terminal** means `done` or `closed` — both count as "resolved". Two rules
follow:

- A node may become **done** only when **all** its descendants are terminal (a
  `closed` descendant counts as resolved).
- **Closing** a node that still has non-terminal descendants is **refused**
  unless you explicitly cascade the close, which then closes those descendants
  too.

### Epic states

- **closed** — an explicit, stored, reversible flag (with an optional reason). It
  takes precedence over the computed states.
- **completed** — computed: the epic has at least one node and every node is
  terminal.
- **open** — computed: anything else, including an epic with no nodes yet.

### Actor & attribution

The **actor** is either **the human** (there is exactly one, unnamed) or a
**named agent**. Attribution is **cooperative**, not access control.

Attribution is required on **comment operations only** — every other operation
is actor-agnostic. **Comments are the sole attribution channel:** to record who
made a status change or added an asset, add a comment saying so. Identify the
actor with `-u`/`--user` (the human) or `-a`/`--agent <name>` (a named agent).

### Comments

A comment is appended to an epic or node and carries its author. It is editable
and (soft-)deletable **only by its own author**. A deleted comment is hidden by
default and never truly removed, so its id is stable and never reused.

### Standard fields

Epics and nodes both carry: **name** (one-liner), **summary** (scope), **body**
(free-form markdown), **labels** (free-form, no built-in meaning — use them to
orchestrate), **status**, **comments**, and **assets** (attached files as proof
of work). Epics also have their **id**.

**Content input rule.** Free-form or binary payloads — an epic or ticket
**body**, **comment** text, **asset** data — are read from **stdin** or
**`--file <path>`**, never from an inline flag. One-liners (`--name`,
`--summary`, `--reason`, and so on) stay inline flags.

## Lifecycle & workflow

A typical path:

1. **Set up once.** `loti init` creates the store in the current directory; the
   whole checkout then shares one store, found from any depth by an upward walk
   (like git). To keep the data elsewhere, `loti init --root <path>` (or a
   positional `<dir>`) creates the store there and leaves a `.loti.conf` pointer
   here. Init refuses if this scope is already inside a store.
2. **Create an epic.** `loti epic create <epic-id> --name "..." --summary "..."`
   (pipe or `--file` a longer body if you want one).
3. **Add tickets.** `loti ticket create <epic-id> --name "..." --summary "..."`.
   For a subticket, add `--parent <epic-id>/<n>`.
4. **Drive status as work moves.** `loti ticket status <ref> --in-progress`,
   then `--blocked` (with `--blocked-by` and/or `--reason`) if stuck, and finally
   `--done` when every descendant is resolved. Use `--closed --reason "..."`
   (add `--cascade` to close open descendants too) when work is dropped rather
   than finished. Status is set-only — read it back with `show`.
5. **Attribute and evidence.** Add a `comment` (with `-u` or `-a <name>`) to
   explain a decision or a status change; `asset add` to attach proof. Read an
   asset back with `asset show <ref> <name>` (bytes to stdout, verbatim) and
   change it with `asset update <ref> <name>` (new data via stdin/`--file`
   and/or a new `--description`). Organise with `label add`. When you change
   something that already exists — a body, a comment, an asset — read its
   current version back first and edit from that, so a concurrent change by
   another agent or the human is not overwritten (see Gotchas).
6. **Read and report.** `loti ticket show <ref>` for one node,
   `loti ticket list <epic-id>` for a scope, and `loti epic list` for the
   roster. A list defaults to the full tree rooted at the scope; add `--shallow`
   for just the immediate level (an epic's top-level nodes, or a node's direct
   children). The plain list closes with a per-status progress footer (totals
   per status over what's listed) — handy for a status readout. Add `--json` to
   any read for the canonical machine-readable form.

Keep the human oriented: prefer small, frequent status changes and short
comments over silent work.

## Gotchas

- **Numbers are per epic and never reused.** Deleting nothing gives numbers
  back; a reference always points at the same node for the life of the epic.
  Numbers count on past deleted nodes, so a mistyped ref can still be valid and
  hit the wrong node — every `edit`/`status` confirmation echoes the target's
  **name** alongside the ref, so glance at it to catch a wrong number.
- **`status` is set-only.** There is no status *reader* — use `show`.
- **`done` is gated by descendants.** It is refused while any descendant is
  non-terminal; resolve or close them first.
- **`close` needs a reason, and refuses open descendants** unless you pass
  cascade, which closes them too.
- **Terminal is a resolution class, not a lock.** `done`/`closed` only gate the
  two rules above; a node is never frozen. You can reclassify `done`→`closed`
  (or the reverse), or move a terminal node back to an active state — the state
  machine allows it.
- **`blocked` is never automatic, and never empty.** You set it and you clear
  it; moving to another state clears the blocked-by. `--blocked` requires at
  least one of `--blocked-by`/`--reason` (an empty blocker is refused). The
  `--blocked-by` refs are a free annotation: `loti` records them but does not
  check the blockers exist or are still unresolved.
- **An asset's name defaults to the file's basename with `--file`.** From stdin
  there is nothing to infer, so `--name` is required there.
- **Never read an asset off disk — use `asset show`.** Its bytes come back on
  stdout exactly as stored (no trailing newline), so binary assets round-trip
  through a pipe. `asset add` is **create-only** — a name already present is
  refused, mirroring `epic create` refusing a duplicate id; `asset update`
  replaces the data and/or description in place and refuses an unknown name.
  Create with `add`, change with `update` — neither silently clobbers. "I was
  only reading it" is not an exception to the hard rule: reads go through the
  CLI too.
- **Edit any resource from a freshly read copy — never a stale one.** Two
  audiences share one store (agents and the human), and every free-form edit
  **replaces the whole field**: `edit --file` overwrites a body, `comment edit`
  overwrites the text, `asset update` overwrites the payload. Basing an edit on
  an old copy — a temp file kept from an earlier `asset add`, a body you cached,
  your own memory of the text — silently discards whatever changed in between.
  Always **read the current version first** (`show` for a body, `comment list`
  for comment text, `asset show` for asset bytes), apply your change to *that*,
  then write it back. Treat every edit as **read-fresh → modify → write**, and
  discard scratch files afterward rather than reusing them next time.
- **Bodies, comment text, and asset data never take an inline flag.** Pipe them
  on stdin or pass `--file`. An interactive terminal with no input is treated as
  empty (it never hangs waiting).
- **Attribution lives in comments.** A status change or asset carries no author
  by itself; add a comment to say who and why.
- **`ticket list` always needs a scope** (an epic id, or a node reference to
  list under). There is no bare cross-epic listing; aggregate with `epic list`
  plus per-epic `ticket list`.
- **`--json` is the source of truth** on every read; the plain form is a
  convenience rendering of it (and carries the progress footer, which the
  machine formats omit).
- **`--fields` drops the identifier unless you ask for it.** A projection like
  `--fields name,status` has no `ref` column, so rows are hard to tell apart —
  add `ref` yourself, or use `--json`/`--ndjson`/`--raw`, which always carry
  `ref`/`number`/`parent`. Any single unknown leaf aborts the whole projection.

## The whole command surface

For the complete, annotated command tree in one pass — every noun, verb, and
collection, with each flag's input rule and the actor requirement on comment
operations — run:

```
loti --help-full
```

That one page is the authoritative reference for exact flags and forms. Start
here for concepts and workflow, then go there for the surface.
