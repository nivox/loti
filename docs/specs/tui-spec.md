# `loti tui` — TUI specification

This document is **normative** (MUST/SHOULD/MAY per RFC 2119). It specifies the
observable behaviour of the full-screen browser: navigation, rendering, input,
writing, and recovery. [`../tui.md`](../tui.md) is the end-user guide; it
explains how to use this behaviour without defining the implementation contract.

The domain model, storage rules, format-version gate, and concurrency primitives
are specified in [core-spec.md](core-spec.md). The command grammar is specified
in [cli-spec.md](cli-spec.md).

## 1. Session and screen

`loti tui` MUST require an interactive terminal and MUST refuse before taking
that terminal over if store discovery or the format gate fails. It presents one
navigation level beside one preview pane. The breadcrumb names the path to the
level, outermost first; its final crumb MUST survive narrowing, with a cut marker
when shortened. A fixed right-hand state slot, separated from the breadcrumb by
one column, shows either editing state or a read-only reason.

The navigation level MUST put meta collections first — `labels`, `comments`, and
`assets` on every epic or node, plus `blockedBy` on a node — then a dim horizontal
rule, then work rows. A meta collection MUST be present even when empty. Work
rows show a status glyph, identifier, direct-child count, optional claim marker,
and name. Collection rows have neither glyph nor work identifier. A glyph is
therefore a work signal, not a decoration.

The preview of an epic or work row is its rendered `show` document. The browser
MAY compose a document for a comment or asset; labels and collection rows MUST
retain their container document. Preview identity is the document being shown,
not simply the cursor row: moving between rows that show one document preserves
its scroll position.

Status glyphs MUST be `○` (to-do/open), `◐` (in-progress), `⊘` (blocked), `✓`
(done/completed), and `✗` (closed). Their colours MUST use the shared status
palette; the glyphs alone MUST distinguish states under `NO_COLOR`.

## 2. Navigation and displayed data

Every epic and node MUST be enterable, including one with no work children.
Their always-present collection rows provide that level. A direct-child count on
an epic or work row means only work children; it MUST NOT promise that no level
exists when absent. Only collection members are leaves. Entering a leaf MUST keep
the level in place and report that there is nothing below.

Collection members MUST preserve their store order. A comment row shows its id,
author, and relative age. A deleted comment remains a dim tombstone with its id
and author but no text and no editing actions. A blocker row shows its full
reference, state glyph, and name and previews the blocking work. An asset row
shows name and size; its preview provides metadata and description, renders text,
and represents binary data without exposing raw bytes. A malformed blocker MUST
remain visible as its stored text and MUST NOT offer typed removal.

A claim marker is a dim `@` in a fixed column immediately before names. The
column MUST exist only when at least one work row on that level is claimed, and
be blank for unclaimed work rows. It MUST be absent from the epic roster. A
blocker row MAY carry it because it represents work; claim actions still belong
only to the work item's own row. The preview names the holder.

## 3. Browsing input and layout

In browse mode, `j`/`k` and arrows move the cursor; `g`/`G` move to the first or
last row; `Enter`, `l`, and right-arrow enter; and Backspace, `Esc`, `h`, and
left-arrow ascend. `q` and `Ctrl-C` quit. `?` and `F1` open help. `r` reloads.
`Ctrl-D`/`Ctrl-U`, PgDn/PgUp/Space, and Home/End scroll the preview as their
respective half-page, page, and end motions. Mouse-wheel up/down MUST scroll the
preview up/down one line without moving the navigation cursor. A half-page motion
MUST use half of the last rendered preview height, rounded down and with a minimum
of one line.
When that height exceeds one line, it MUST retain at least one line from the
previous view; a one-line preview advances by one line because overlap is
impossible. `<`/`>` change the split by 5%, `=` restores it, and `z` zooms the
preview.

Zoom MUST fill the width with the preview, retain mouse capture so wheel reports
continue to reach it, hide the navigation cursor, and disable entering or leaving
a level. In that state, `j`/`k` scroll the preview. The split is per-session and
MUST NOT persist.

Dragging the divider MAY resize the panes while editing mode or a surface is
open, because the split is reader-owned furniture. It MUST be refused while a
dialog is open: no input may move content beneath a question. Wheel movement is
silent while editing.

`w` chooses a workflow and agent profile to launch against the row under the
cursor. It is row-resolved rather than level-resolved: unlike `N`, the
highlighted row is the target, not the level containing it. It MUST be offered
only on an epic or a node; a blocker row names a ticket without showing that
ticket's own state, so it does not qualify, nor do comments, labels, assets, or
a collection row. It MUST be refused, in the store's own words, on a read-only
store, and refused with its own notice while the navigation pane is hidden,
naming `z` as the way to restore it. On an ineligible row it MUST state the
rule instead of opening anything, and on the one screen with no row at all it
MUST say there is nothing to hand over. Opening the picker MUST select valid
effective resources, hand the terminal to the selected agent, and restore the
browser after the agent exits.

## 4. Editing mode and actions

`e` enters editing mode only on a selected row. The selected row and level MUST
remain frozen until the mode ends. A successful write MUST end the mode; each
editing session therefore performs at most one edit. The state slot MUST display
`── EDITING ──`; the frozen row MUST retain contrast and gain a gutter bar, other
rows MUST dim, and the navigation border MUST change to the editing treatment.

At action selection, only available editing actions, `Esc`, and help are live.
`Esc` leaves the mode. Browsing and layout keys MUST NOT take effect. An input a
reader would reasonably expect to act in that context SHOULD report how to leave;
a key never bound in that context MAY remain silent.

The actions are `a` to add a member to the selected container, `d` to remove a
selected member, `n` for name, `S` for summary, `b` for body or comment text,
`s` for state, `c` to take or reassign a claim, and `C` to release a held claim.
The browser MUST offer only actions valid for the current selection. `N` creates
an epic at the roster without entering editing mode, and `w` launches a workflow
against the row under the cursor; neither enters editing mode, and neither is
offered by it. The browser writes comments as the human; it MUST NOT request an
agent identity. Asset addition and replacement are CLI operations.

A single many-line body or comment field MUST render in the preview pane. Short
fields, pickers, and multi-field forms MUST render as centred floats. A float
MUST cover rather than reflow its background. When terminal height is short, its
answers MUST retain their place and its message MUST yield first with a cut
marker. The underlying hint strip remains the editing strip, so a dialog MUST
print its own answers.

## 5. Surface input, warnings, and notices

`Ctrl-S` MUST accept a surface regardless of focused field. `Tab` and
`Shift-Tab` MUST move between fields. `Enter` MUST insert a line break only into
a many-line field and otherwise be inert; it MUST NOT accept a surface or select
a picker value. A picker value is its current highlight, and vertical motion
changes that value directly. `Ctrl-C` is equivalent to `Esc` in editing mode.
`F1` opens help inside a field; `?` is field content.

`Ctrl-G` MUST hand the current text field to `$VISUAL`, falling back to `$EDITOR`.
The command is executed directly, split on whitespace, rather than through a
shell; shell quoting and home-relative expansion are unavailable. The terminal
MUST be released before the editor runs and reclaimed afterwards, including after
failure. A one-line field MUST strip returned newlines; a many-line field MUST
retain newlines and support vertical and line-end motion.

Opening editing mode while zoomed MUST be refused without changing the reader's
layout, and the refusal MUST name `z` as the way to restore the navigation pane.
A body buffer MUST neither zoom nor resize the split automatically.

A field is dirty after content-mutating input and remains dirty until the surface
ends. `Esc` on a clean field cancels immediately; on a dirty field it MUST ask
before discarding. Required fields that are empty or whitespace-only MUST refuse
save. A nonblank label is otherwise stored verbatim.

Failed or costly outcomes MUST use dialogs, not notices. The browser supports
discard, missing-required-field, store-refusal, stale-conflict, and
external-editor-failure dialogs. `Esc` MUST never be destructive. A destructive
confirmation uses `d`; `Enter` MUST NOT answer it. An acknowledgement may accept
`Esc` or `Enter`; a missing-required acknowledgement MUST focus the first missing
required field. A store refusal and external-editor failure MUST return to the
intact field. A stale conflict MUST offer `o` to overwrite anyway and `Esc` to
return to the intact buffer.

Every successful write MUST show a one-line notice naming what changed. Notices
MUST also explain expected-but-unavailable editing actions. A notice replaces the
whole hint strip for at most five seconds, is measured against wall-clock time,
clears on a key press, and is replaced by a newer notice. Its leading marker MUST
remain visible without colour. A critical failure MUST NOT be reduced to a
notice.

A close picker MAY offer `also close N open descendants: no / yes` only when its
current plan has descendants; its default is no. A cascade is independent,
idempotent closes, not one atomic write. Success MUST name the parent and count.
When a cascade stops after publishing a change, the browser MUST refresh rows
before showing the refusal dialog and MUST NOT also show a success notice.

## 6. Read-only operation, reload, and conflicts

At startup and every reload, the browser MUST ask the store whether it is
mutable. An older store major or a migration sentinel leaves the browser
read-only; the state slot MUST name the reason, and `e`, `N`, and `w` MUST not
be offered. A newer incompatible major MUST refuse before terminal takeover. A
reload MAY change read-only state when another actor migrates the store.

Reads are lock-free and MUST NOT be represented as a global snapshot. The browser
MUST refresh on cursor movement, level enter/leave, explicit reload, and a write
outcome that changed the store. Reload retains a cursor on the same row where
possible and drops an empty level to the deepest surviving level. If a frozen
target disappears or the store becomes read-only, editing mode MUST end with a
notice.

Opening a whole-field replacement MUST re-read its target and capture the
entity's `updated` stamp. The subsequent replacement MUST name that stamp as a
precondition. A changed entity produces a no-write conflict and preserves the
buffer until the reader overwrites deliberately or returns to it. Status picks,
claims, appends, and merges MUST NOT use this precondition.

Where possible, an unreadable member MUST remain an unreadable row. A failure to
list a level MUST report in a dialog and preserve the current level. An unreadable
epic MUST remain an unreadable roster row so other epics are reachable.
