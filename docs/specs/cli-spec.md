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
  `ticket create <epic-id> --parent <ref>`), `agent`, and `workflow`, plus
  top-level `init`, `skill`, `tui`, `migrate-store`. Collections nest a third
  level (`ticket comment add`).
- **Operations grouped by nature:** `edit` = plain scalar fields
  (`name`/`summary`/`body`/`parent`); `status` = the state machine (**set-only**;
  read via `show`); `claim` = the node-only single-holder claim
  (`take`/`release`; **set-only**, read via `show`); `label`/`comment`/`asset` =
  collections (`add`/`remove`|`delete`/`list`, plus asset `update`/`show`);
  `show` = the **sole projectable reader** of nodes/epics. `agent` and
  `workflow` expose effective configured resources; `agent run` is an explicit
  foreground launch rather than a tracker mutation.
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
- **Write precondition (opt-in).** The modifiers that replace a **whole field** —
  `epic edit`, `ticket edit`, and `comment edit` — MAY carry
  `--expect-updated <stamp>`, naming the target's `updated` value exactly as
  `show` prints it. The write MUST apply only while the stored stamp still equals
  it; otherwise it MUST be refused with a non-zero exit **having written
  nothing**. Granularity is the **target**, not the field: any concurrent change
  to the same epic or ticket moves its `updated` stamp and so refuses the write.
  Omitting the flag names no precondition — **last write wins**, exactly as
  before the flag existed. Creates, appends (`comment add`, `label add`,
  `blocked-by add`) and pickers (`status`, `claim`) MUST NOT offer it: an append
  adds an entry of its own rather than replacing anyone's text, and a pick's
  "conflict" is simply the later of two deliberate choices.
- **Mutation confirmations.** A successful `edit`/`status` on an epic or ticket
  confirms on stdout **naming the target** — the ref/id **and** its name — since
  refs are numeric and a mistyped but valid ref would otherwise apply silently
  to the wrong node.

```
loti [--root <path>] <init|epic|ticket|agent|workflow|skill|tui|migrate-store> ...

loti init [<dir>]        # default container: .loti/ here (no pointer). --root or
                         # the positional <dir> (equivalent, mutually exclusive)
                         # names the container literally (no .loti appended);
                         # meta lands at <container>/meta and a .loti.conf
                         # pointer is written here. Refuses if the scope is
                         # already inside a store (upward walk). Warns if in a
                         # git repo but not at its root.
loti --help-full         # entire command tree in one pass (global; see
                         #   The `skill` subcommand & help)
loti migrate-store       # align an older on-disk format to this binary
                         #   (see core-spec.md → Format versioning & migration)

loti epic create <epic-id> --name <s> --summary <s> [--label <l>]...   # body ← stdin|--file
loti epic show   <id> [--field <f> | --fields <f,…>] [--markdown|--json|--raw]
loti epic edit   <id> [--name <s>] [--summary <s>] [--file <path>]
                      [--expect-updated <stamp>]      # apply only if unchanged
loti epic status <id> (--closed [--reason <s>] | --open)               # set-only
loti epic label   (add|remove|list) <id> [<label>…]
loti epic comment (add|edit|delete|list) <id> …
loti epic asset   (add|update|show|delete|list) <id> …
loti epic list   [filters — see Filtering model for `list`]

loti ticket create <epic-id> [--parent <ref>] --name <s> --summary <s> [--label <l>]...  # body ← stdin|--file
loti ticket show   <ref> [--field <f> | --fields <f,…>] [--markdown|--json|--raw]
loti ticket edit   <ref> [--name <s>] [--summary <s>] [--parent <ref>] [--file <path>]
                        [--expect-updated <stamp>]    # apply only if unchanged
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
loti <e|t> comment edit   <ref> <comment-id> (-u | -a <agent>) [--file <path>]
                          [--expect-updated <stamp>]                  # own author only
loti <e|t> comment delete <ref> <comment-id> (-u | -a <agent>)        # own author only; soft
loti <e|t> comment list   <ref> [--include-deleted]
loti <e|t> asset add    <ref> --name <name> [--file <path>] [--description <s>]  # data ← stdin|--file; create-only
loti <e|t> asset update <ref> <name> [--file <path>] [--description <s>]  # data ← stdin|--file; ≥1 change
loti <e|t> asset show   <ref> <name>                                 # data → stdout, verbatim
loti <e|t> asset delete <ref> <name>                                 # hard
loti <e|t> asset list   <ref>

# Effective agent profiles and workflows
loti agent list [--field <f> | --fields <f,…>] [--json|--ndjson|--raw]
loti agent show <id> [--field <f> | --fields <f,…>] [--markdown|--json|--raw]
loti agent run <epic-id>|<epic-id>/<n> --agent <id> --workflow <id>
loti workflow list [--field <f> | --fields <f,…>] [--json|--ndjson|--raw]
loti workflow show <id>  # Markdown → stdout, verbatim

loti skill              # prints the static SKILL.md (see The `skill` subcommand & help)
loti tui                # full-screen browser for epics and tickets; requires an
                        #   interactive terminal (see docs/tui.md for the keys)
```

- **`ticket list` requires a scope** — `<epic-id>` (whole epic) or `<epic-id>/<n>`
  (under a node). Each defaults to the full tree rooted at the scope; `--shallow`
  keeps only the immediate level. There is **no bare cross-epic list**; aggregate
  via `epic list` + per-epic `ticket list` (epics are detached units of work).

---

## 2. Read / output formats

- **`--json` is the source of truth** on every formatted read command that
  offers it; the human form is the default (one extra flag for machine output).
  `workflow show` is deliberately different: it writes the selected workflow
  source verbatim and has no formatting flags.
- **Epic and ticket `show`** have three modes: **`--markdown` (default)**,
  `--json`, `--raw`.
  - **Markdown** (viewer-friendly) emits *everything* in order: metadata table →
    name (H1) → summary (blockquote) → direct-children table → assets table →
    body (verbatim) → comments.
  - **`--json`** = the whole node/epic with all fields (the canonical form;
    markdown/raw are renderings of it).
  - **`--raw`** operates on **leaves**, one value per line, **strict-unambiguous**;
    any ambiguous multi-field selection is a **hard error** pointing at `--json`
    (no `--sep`).
- **Epic and ticket `list`** have three modes: **default plain text**
  (git-log-like, management-oriented — indented depth-first tree with a trailing `[blocked-by: …]` tag
  listing a node's dependency refs (shown in any state), closed
  by a **per-status progress footer**: the total plus one entry per non-empty
  status in lifecycle order, over the nodes actually listed, tagged when a filter
  narrowed them and marked done when all are terminal — plain text only),
  `--json` (**flat array** with `parent` pointers, never nested; `--ndjson` to
  stream one object per line), and `--raw` (flat, tab-separated). **No
  `--markdown`** — `list` never *presents*.
- **`--fields`** takes **dotted leaf paths** (e.g. `comments.author`), in all
  three modes. Epic and ticket `list` are restricted to **summary/listable fields**
  (`ref|number|name|status|parent|labels|blocked-by`; epics
  `id|name|status|labels|nodes`+counts); requesting heavy/structured fields
  (`body`/`comments`/`assets`/`subtickets`) on `list` is a **hard error** — those
  are `show`-only.
- **Colour.** Default plain-text `list` MAY use ANSI colour on a TTY, auto-stripped
  when piped. `--raw`/`--json`/`--ndjson`/`--markdown` are never coloured.
- **One status palette (MUST).** Which colour a status is painted in is defined
  once and shared by every surface that paints one, so two views of the same
  store cannot disagree about a state's colour. A surface maps that palette onto
  its own colour type; it MUST NOT restate the status-to-colour mapping.
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

## 4. Agent profiles, workflows & foreground launch

- **Effective resources.** `agent` reads harness profiles and `workflow` reads
  opaque Markdown instructions. A project MAY configure repository-local roots
  in the nearest `.loti.conf` found by walking upward from the store:
  `agent-root` for profiles and `workflow-root` for workflows. Each configured
  root is absolute or relative to that config file and MUST resolve to an
  existing directory; an absent key supplies no local root, while a broken
  configured root is an error. The user-global roots are
  `$XDG_CONFIG_HOME/loti/agents` and `$XDG_CONFIG_HOME/loti/workflows` (or the
  normal XDG config-home equivalent). Only direct `.toml` profile files and
  direct `.md` workflow files are candidates.
- **IDs and precedence.** An operator-supplied resource ID MUST be non-empty
  ASCII letters, digits, `-`, or `_`; IDs are case-sensitive. A local candidate
  with the same filename stem shadows the global candidate before either is
  validated, so a bad local override is reported rather than silently falling
  back to global. Effective rosters sort IDs in bytewise lexical order.
- **Resource lists.** `agent list` and `workflow list` list every effective
  resource, including invalid ones. Each row carries exactly `id`, `origin`
  (`local` or `global`), and diagnostics; there is no separate validity flag.
  Plain output is one resource per line with its origin and diagnostic tag;
  `--json` is a flat array of those rows; `--ndjson` writes one row per line;
  and `--raw` writes tab-separated `id`, `origin`, and diagnostics. Their only
  listable fields are `id`, `origin`, and `diagnostics`.
- **Profile show.** `agent show <id>` resolves the selected usable effective
  profile. Its default is viewer-friendly Markdown; `--json` is the canonical
  parsed profile (`id`, `origin`, `command`, `args`, `cwd`, `env`, and
  `diagnostics`), and `--raw` provides strict-unambiguous leaf projections.
  An invalid selected profile fails with its diagnostic; an absent selected ID
  fails as not found.
- **Workflow show.** `workflow show <id>` resolves the selected usable
  effective workflow and writes its valid UTF-8 Markdown source to stdout
  exactly as loaded: no wrapper, formatting mode, normalization, or trailing
  bytes are added. An invalid selected workflow or an absent ID fails.
- **Cooperative session visibility.** Presence of either
  `LOTI_AGENT_SESSION` or `LOTI_AGENT_WORKFLOW` (including an empty value)
  makes the operator-facing `agent` namespace unavailable. A
  `LOTI_AGENT_WORKFLOW` marker additionally narrows `workflow list` and
  `workflow show` to that exact ID; a non-selected workflow has the ordinary
  not-found behaviour. `LOTI_AGENT_SESSION` alone does not narrow workflow
  access. These markers are cooperative guidance, not access control: a child
  process can alter its own environment.
- **Explicit foreground launch.** `loti agent run <target> --agent <id>
  --workflow <id>` requires both selections; it has no profile or workflow
  default. The target is an existing epic ID or ticket reference. The command
  first refuses a caller already in a cooperative agent session, then requires
  stdin, stdout, and stderr all to be terminals before it opens the store.
  It then resolves the target and both selected effective resources and
  validates the prepared launch plan. Refusal at any preflight step hands off
  no process and makes no tracker mutation.
- **Direct foreground handoff.** On Unix, a successful `agent run` replaces
  the `loti` process with the prepared profile command directly: no shell and
  no wrapper child. The replacement retains the terminal streams and its exit
  status is the command's exit status. The prepared argv, working directory,
  and environment are passed to that process, including the cooperative session
  markers and the bootstrap instruction required by the selected profile. On
  non-Unix platforms the command refuses rather than emulating those process
  replacement semantics.

---

## 5. The `skill` subcommand & help

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

---

## 6. The `tui` subcommand

- **`loti tui` is a full-screen browser** for the store's epics and tickets: a
  navigation pane over the epic/ticket hierarchy beside a preview pane, with a
  breadcrumb naming the path to the level on screen.
- **It requires an interactive terminal (MUST).** With output not attached to a
  terminal it MUST refuse with a plain message and a non-zero exit rather than
  emitting anything, since it has no non-interactive rendering.
- **Preconditions are checked before the screen is taken over.** Store discovery
  (including `--root`) and the format-version check behave exactly as for any
  other command, and a refusal MUST leave the terminal untouched.
- **The preview shows a `show` document.** What the preview renders for an epic
  or a node is the markdown `show` produces for it, so the browser presents no
  document shape of its own.
- **The key bindings are not normative** — they are documented in
  [`../tui.md`](../tui.md) and listed by the browser's own help overlay.
