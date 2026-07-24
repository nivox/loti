# `loti` — CLI specification

This document is **normative** (MUST/SHOULD/MAY per RFC 2119). It specifies the
**`loti`** command-line interface: the command surface & grammar, read/output
formats, the filtering model for `list`, and the `skill`/help system.

The domain model, on-disk storage format, and cross-cutting requirements
(concurrency, versioning) are specified in [`core-spec.md`](core-spec.md); this
document assumes that model.

---

## 1. CLI command surface & grammar

- **Grammar: noun-verb.** Nouns: `epic`, `ticket` (a subticket is
  `ticket create <epic-id> --parent <ref>`), plus top-level `init`, `skill`,
  `migrate-store`. Collections nest a third level (`ticket comment add`).
- **Operations grouped by nature:** `edit` = plain scalar fields
  (`name`/`summary`/`body`/`parent`); `status` = the state machine (**set-only**;
  read via `show`); `claim` = the node-only single-holder claim
  (`take`/`release`; **set-only**, read via `show`); `label`/`comment`/`asset` =
  collections (`add`/`remove`|`delete`/`list`, plus asset `update`/`show`);
  `show` = the **sole projectable reader** of nodes/epics.
- **Create never overwrites; modify targets what exists.** Every creator refuses
  a duplicate (`epic create` an existing id; `ticket create`/`comment add` cannot
  collide — their keys are machine-allocated, never-reused numbers/ids); every
  modifier (`edit`, `comment edit`, `asset update`) requires its target to exist.
  An **asset** is keyed by a **caller-chosen name**, so `asset add` is
  **create-only** — a name already present is refused (use `asset update` to
  replace). Assets are read back verbatim with `asset show`, never off disk.
- **Content input.** Free-form/binary payloads (epic & ticket `body`, comment
  text, asset data) come from **stdin or `--file <path>`** — never inline; absent
  source → empty for optional body, error for required content; never blocks on a
  TTY. One-liners stay inline flags (`--name`, `--summary`, `--reason`, …).
- **Actor.** `-u|--user` xor `-a|--agent <name>` required **only** on comment
  add/edit/delete. Everything else is actor-agnostic (assets: anyone
  add/update/show/delete/list; asset delete is **hard**).
- **Mutation confirmations.** A successful `edit`/`status` on an epic or ticket
  confirms on stdout **naming the target** — the ref/id **and** its name — since
  refs are numeric and a mistyped but valid ref would otherwise apply silently
  to the wrong node.

```
loti [--root <path>] <init|epic|ticket|skill|migrate-store> ...

loti init [<dir>]        # default: .loti/ here; a data root elsewhere via
                         # --root or the positional <dir> (equivalent, mutually
                         # exclusive) + a .loti.conf pointer here. Refuses if the
                         # scope is already inside a store (upward walk). Warns
                         # if in a git repo but not at its root.
loti --help-full         # entire command tree in one pass (global; see
                         #   The `skill` subcommand & help)
loti migrate-store       # align an older on-disk format to this binary
                         #   (see core-spec.md → Format versioning & migration)

loti epic create <epic-id> --name <s> --summary <s> [--label <l>]...   # body ← stdin|--file
loti epic show   <id> [--field <f> | --fields <f,…>] [--markdown|--json|--raw]
loti epic edit   <id> [--name <s>] [--summary <s>] [--file <path>]
loti epic status <id> (--closed [--reason <s>] | --open)               # set-only
loti epic label   (add|remove|list) <id> [<label>…]
loti epic comment (add|edit|delete|list) <id> …
loti epic asset   (add|update|show|delete|list) <id> …
loti epic list   [filters — see Filtering model for `list`]

loti ticket create <epic-id> [--parent <ref>] --name <s> --summary <s> [--label <l>]...  # body ← stdin|--file
loti ticket show   <ref> [--field <f> | --fields <f,…>] [--markdown|--json|--raw]
loti ticket edit   <ref> [--name <s>] [--summary <s>] [--parent <ref>] [--file <path>]
loti ticket status <ref> (--to-do | --in-progress |
                          --blocked --reason <s> |
                          --done | --closed --reason <s> [--cascade])   # set-only
loti ticket blocked-by (add|remove|set|clear|list) <ref> [<blocker>…]  # node-only dependency list
loti ticket claim (take <ref> --as <s> | release <ref>)               # node-only single-holder claim
loti ticket label   (add|remove|list) <ref> [<label>…]
loti ticket comment (add|edit|delete|list) <ref> …
loti ticket asset   (add|update|show|delete|list) <ref> …
loti ticket list   <epic-id>[/<n>] [--shallow] [filters]              # scope required

# Collections (identical under epic and ticket; <ref> = <id> or <epic-id>/<n>)
loti <e|t> comment add    <ref> (-u | -a <agent>) [--file <path>]      # text ← stdin|--file (required)
loti <e|t> comment edit   <ref> <comment-id> (-u | -a <agent>) [--file <path>]  # own author only
loti <e|t> comment delete <ref> <comment-id> (-u | -a <agent>)        # own author only; soft
loti <e|t> comment list   <ref> [--include-deleted]
loti <e|t> asset add    <ref> --name <name> [--file <path>] [--description <s>]  # data ← stdin|--file; create-only
loti <e|t> asset update <ref> <name> [--file <path>] [--description <s>]  # data ← stdin|--file; ≥1 change
loti <e|t> asset show   <ref> <name>                                 # data → stdout, verbatim
loti <e|t> asset delete <ref> <name>                                 # hard
loti <e|t> asset list   <ref>

loti skill              # prints the static SKILL.md (see The `skill` subcommand & help)
```

- **`ticket list` requires a scope** — `<epic-id>` (whole epic) or `<epic-id>/<n>`
  (under a node). Each defaults to the full tree rooted at the scope; `--shallow`
  keeps only the immediate level. There is **no bare cross-epic list**; aggregate
  via `epic list` + per-epic `ticket list` (epics are detached units of work).

---

## 2. Read / output formats

- **`--json` is the source of truth** on every read command; the human form is the
  default (one extra flag for machine output).
- **`show`** has three modes: **`--markdown` (default)**, `--json`, `--raw`.
  - **Markdown** (viewer-friendly) emits *everything* in order: metadata table →
    name (H1) → summary (blockquote) → direct-children table → assets table →
    body (verbatim) → comments.
  - **`--json`** = the whole node/epic with all fields (the canonical form;
    markdown/raw are renderings of it).
  - **`--raw`** operates on **leaves**, one value per line, **strict-unambiguous**;
    any ambiguous multi-field selection is a **hard error** pointing at `--json`
    (no `--sep`).
- **`list`** has three modes: **default plain text** (git-log-like, management-
  oriented — indented depth-first tree with a trailing `[blocked-by: …]` tag
  listing a node's dependency refs (shown in any state), closed
  by a **per-status progress footer**: the total plus one entry per non-empty
  status in lifecycle order, over the nodes actually listed, tagged when a filter
  narrowed them and marked done when all are terminal — plain text only),
  `--json` (**flat array** with `parent` pointers, never nested; `--ndjson` to
  stream one object per line), and `--raw` (flat, tab-separated). **No
  `--markdown`** — `list` never *presents*.
- **`--fields`** takes **dotted leaf paths** (e.g. `comments.author`), in all
  three modes. `list` is restricted to **summary/listable fields**
  (`ref|number|name|status|parent|labels|blocked-by`; epics
  `id|name|status|labels|nodes`+counts); requesting heavy/structured fields
  (`body`/`comments`/`assets`/`subtickets`) on `list` is a **hard error** — those
  are `show`-only.
- **Colour.** Default plain-text `list` MAY use ANSI colour on a TTY, auto-stripped
  when piped. `--raw`/`--json`/`--ndjson`/`--markdown` are never coloured.
- **Actor format** in output: `human` / `agent:<name>`. Deleted comments hidden by
  default; shown as an author+timestamp **tombstone** (text withheld) under
  `--include-deleted`.

---

## 3. Filtering model for `list`

- Filters live **on `list`** — there is **no separate `search` command**. Four
  families: **scope**, **label**, **status**, **match**; they combine with **AND**
  across families. Structured filters (scope/label/status) evaluate first, then
  `--match` runs over the survivors.
- **Scope (required, ref-polymorphic).** `ticket list <epic-id>` (whole epic) or
  `ticket list <epic-id>/<n>` (under a node). The **default is the full tree
  rooted at the scope**; **`--shallow`** collapses it to the immediate level only
  — an epic's top-level nodes, or a node's direct children. One rule holds at
  both scopes, so the flag never becomes a silent no-op. `--shallow` is
  `ticket list`-only (the `epic list` roster has no scope depth). The anchor node
  is never part of its own listing. There is **no bare cross-epic `ticket
  list`**. `epic list` is a flat roster with no scope arg.
- **Labels.** `--label` repeated = AND, comma = OR-group
  (`--label a --label b,c` ⇒ `a ∧ (b∨c)`); `--not-label` = "has none of"
  (`¬(union of excluded)`; comma and repeat coincide for exclusion).
- **Status.** `--status` comma = OR; **repeat = error** (statuses are mutually
  exclusive). `--not-status` is symmetric with `--not-label`. Aggregators
  **`--open`** (`to-do|in-progress|blocked`) and **`--resolved`** (`done|closed`)
  are mutually exclusive with each other and with `--status`/`--not-status`.
- **Match.** `--match <query>` selects surviving candidates via a matcher chosen
  by **`--match-impl <impl>`**. The built-in **`regex`** impl (name reserved) is
  the **default** — matching **name + summary + body** — so `loti` has **zero
  external dependency** out of the box. Protocol: `loti` resolves scope+structured
  filters to a **candidate file set**, passes it to the matcher, which returns a
  **newline-separated subset of those paths** on stdout (order significant and
  preserved); paths outside the set / unparseable lines are ignored with a
  warning. grep-style exit handling (non-zero + empty stdout = zero matches;
  non-zero + stderr surfaced).
- **Matcher config.** External impls are **argv-array command templates** with
  `<QUERY>` (one arg) and `<CANDIDATES>` (expands to N path args) placeholders —
  argv, not a shell string. Layered TOML: **user-global** XDG config
  (`~/.config/loti/config.toml`) merged with **project `.loti.conf`**, project
  winning on name collision. An unknown `--match-impl` errors, listing configured
  impls.
- **Result model.** The result set is **flat = exactly the nodes satisfying the
  full AND predicate**; `--shallow` affects **scope only**. Each result is
  self-locating via its `<epic-id>/<n>` + `parent`, so hierarchy is reconstructable
  (tree rendering: see *Read / output formats*). External matchers receive
  **raw `<n>.md` paths** (whole-file, incl. frontmatter) — an accepted asymmetry
  vs the internal `regex` surface.

---

## 4. The `skill` subcommand & help

- **`loti skill` prints a fully static, hand-authored SKILL.md** verbatim — no
  generation, no template markers, no splicing.
- **Single-source-of-truth (MUST).** Argument parsing, scoped `--help`, and the
  global **`--help-full`** all render from **one declarative command-tree
  definition**; they MUST NOT be independently hand-maintained. `skill` is **not**
  a consumer of that tree (it is static prose), so the drift-prone hand-written
  surface is minimized to concepts/workflow.
- **`--help-full`** is a **global flag** emitting the **complete command tree in
  one pass**, workflow-ordered (nouns → verbs → collections). Both scoped `--help`
  and `--help-full` MUST carry the annotations agents need: per-flag **input rule**
  (inline / stdin / `--file`) and the **actor requirement** on comment ops. The
  agent's path is bounded: `loti skill` (concepts + workflow) → one `loti
  --help-full` (whole annotated surface).
- **Content focus: CLI-only.** The skill teaches driving the CLI and nothing else;
  the on-disk format is **not** an authoring path, nor a reading path. The skill
  MUST carry an explicit **MUST-NOT** rule: agents must not read from or write to
  store files directly (hand-author, hand-edit, copy, or search them) — **every
  operation, read and write alike, goes through the CLI** (invariants are
  CLI-enforced, not file-enforced, and the on-disk layout is internal and may
  change; see [`core-spec.md`](core-spec.md) → *Concurrency & multi-actor
  safety*). Plain-text legibility is a human-inspection affordance only. This
  is why reading back and updating assets are first-class CLI operations rather
  than a reason to touch files.
- **SKILL.md structure (7 sections):** frontmatter → what/when → the hard rule
  (prominent) → distilled core concepts (self-contained, no external glossary) →
  lifecycle/workflow → gotchas → `--help-full` handoff.
