# `loti tui` — browser guide

`loti tui` is the interactive browser for a store. It lets a human read epics and
tickets, make the supported edits, and launch a configured workflow without
leaving the terminal.

```
loti tui              # browse the store for the current directory
loti tui --root DIR   # browse the store at DIR
```

It requires an interactive terminal. For the full behaviour contract, see the
[TUI specification](specs/tui-spec.md).

## Reading the screen

The left pane is the navigation tree and the right pane is the selected item's
preview. The breadcrumb at the top tells you where you are. A right-hand message
shows either `EDITING` or a read-only migration warning.

Within an epic or ticket, the first rows are its collections:

- `labels`
- `comments`
- `blockedBy` (tickets only)
- `assets`

They remain visible even when empty, so you can enter them and add the first
label, comment, or dependency. Work rows follow the divider. A number in
parentheses is the direct subticket count; an absent count does not stop you
from entering an epic or ticket. Collection members are leaves, and opening one
that has nothing below reports that fact.

A dim `@` before a work item's name means somebody holds its claim. Move to the
row to see the holder in the preview. Comments, labels, dependencies, and assets
have their own useful previews; labels retain the containing item's preview.

## Browse

| Key | Action |
|---|---|
| `j` / `k` / arrows | move between rows |
| `g` / `G` | first / last row |
| `Enter` / `l` / `→` | open the highlighted row |
| `Backspace` / `Esc` / `h` / `←` | go back one level |
| `Ctrl-D` / `Ctrl-U` | scroll preview half a screen, retaining context |
| mouse wheel | scroll preview one line without moving the highlighted row |
| `PgDn` / `PgUp` / `Space` | scroll preview a screen |
| `Home` / `End` | preview start / end |
| `<` / `>` | change pane split |
| `=` | restore the default split |
| `z` | preview-only zoom |
| `r` | reload from the store |
| `?` / `F1` | key help |
| `q` / `Ctrl-C` | quit |

Half-screen preview movement retains at least one line from the previous view
when the preview can show more than one line. A one-line preview advances one
line because no overlap is possible.

Zoom fills the terminal with the preview and keeps mouse capture enabled, so the
mouse wheel keeps scrolling the preview. Press `z` again to restore the tree.
Drag the divider to adjust the split; it lasts for this session only.

## Edit

Press `e` on a row to enter editing mode. The selected row stays fixed while you
choose one action and finish it. The footer lists only actions available for that
row, including:

| Key | Action |
|---|---|
| `a` | add a ticket, subticket, label, dependency, or comment |
| `d` | remove a label or dependency, or delete a comment or asset |
| `n` / `S` / `b` | edit name / summary / body or comment text |
| `s` | change open/closed state or ticket status |
| `c` / `C` | take or release a claim |

`N` creates an epic from the top-level roster. Asset uploads and replacements
remain CLI operations.

Use `Esc` to leave editing mode before opening an action. Once a form is open:

- `Ctrl-S` saves.
- `Tab` and `Shift-Tab` move between fields.
- `Enter` adds a line in a body or comment; it does not save.
- `Ctrl-C` acts like `Esc`.
- `F1` opens help.
- `Ctrl-G` opens the field in `$VISUAL`, or `$EDITOR` when `VISUAL` is unset.
- In a picker, `↑`/`↓`, `Ctrl-P`/`Ctrl-N`, or `k`/`j` move the highlighted
  choice, which is the value the form saves.

A body or comment edits in the preview pane. Short fields and multi-field forms
open as centred dialogs. A save returns you to browsing and confirms what changed.
If you cancel changed text, the browser asks first; destructive confirmations use
`d`, never `Enter`.

## Launch an agent

Press `w` while browsing, on the highlighted epic or ticket, to open a centred
picker for that row, with `workflow` first and `agent` second. Move a picker's
highlight, which is its value, with `↑`/`↓`, `Ctrl-P`/`Ctrl-N`, or `k`/`j`.
Choices identify whether their configured resource is local or global. Choose
both values and accept the form to launch immediately; there is no second
confirmation.

Loti validates the selected target, resources, and launch configuration before
it gives up the terminal. A validation failure leaves the picker open and shows
the reason. Once the agent starts, the browser temporarily releases its terminal
screen and input modes; when the agent exits, it restores and repaints the
browser. A launch failure or non-zero agent exit is reported only after that
restoration.

Launching an agent never changes claims, statuses, comments, or other tracker
data. The selected workflow directs the agent's work; see [Launching external
agents](agents.md) to configure profiles and workflows or to use the equivalent
CLI command.

## Read-only stores and concurrent changes

A store that needs migration is browseable but read-only. The banner says why,
and write keys are unavailable. Reload with `r` after a migration completes.

The browser does not block agents while it is open or while you type. Before it
opens a whole-field editor, it refreshes that item. If someone changes the same
item before you save, the browser keeps your text and lets you either overwrite
deliberately or return to the editor. Reload whenever the preview looks stale.

## Help and recovery

The footer shows confirmations and short explanations for unavailable editing
actions. Problems that need a decision appear in a dialog and preserve text where
possible. Store errors are shown in the store's own words.

Use the built-in `?` or `F1` key list for the exact keys available in the current
context. The normative [TUI specification](specs/tui-spec.md) records the complete
behaviour and edge cases.
