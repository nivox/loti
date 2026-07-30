# `loti tui` — the full-screen browser

`loti tui` browses a store the way a file manager browses a directory tree.
Epics are the top level; entering one lists its tickets, entering a ticket lists
its subtickets, to any depth. It is a **reading** surface: it never writes to the
store, so it can be left open while agents work.

It needs an interactive terminal. Piped or redirected, it refuses rather than
emitting anything.

```
loti tui              # browse the store for the current directory
loti tui --root DIR   # browse the store at DIR
```

## The screen

```
 epics › my-feature › 3 Wire the store lock into ops
┌ navigation ─────────────────┐┌ my-feature/8 ──────────────────────────────┐
│◐ 3  (4)  Wire the store lo… ││  … the markdown `loti ticket show` prints  │
│○ 7       Add --cascade to … ││                                            │
│◐ 8  (2)  Rework the read l… ││                                            │
└─────────────────────────────┘└────────────────────────────────────────────┘
 j/k move · Enter open · Esc back · … · ? keys · q quit
```

- **The breadcrumb** (top line) names the path to the level on screen, outermost
  first. On a narrow terminal it is shortened from the left, so the level you are
  in always survives.
- **The navigation pane** lists exactly one level: the children of the last
  breadcrumb entry. Each row is a status glyph, the identifier (an epic id, or a
  bare ticket number — the epic is already in the breadcrumb), the number of
  direct children, a claim marker, and the name. The marker is a dim `@` on a
  ticket someone holds a claim on; its column is there only while something on
  the level is claimed, and who holds it is in the preview.
- **The preview pane** shows the same document `loti epic show` /
  `loti ticket show` print, rendered as markdown — including tables, code blocks
  and mermaid diagrams in a ticket body. Its title is the reference it shows.
- **The preview follows the cursor**, not the level: moving the highlight changes
  what is previewed.

### Status glyphs

| Glyph | Node | Epic |
|---|---|---|
| `○` | to-do | open |
| `◐` | in-progress | — |
| `⊘` | blocked | — |
| `✓` | done | completed |
| `✗` | closed | closed |

Colours match what `loti ticket list` and `loti epic list` print. With `NO_COLOR`
set, the glyphs alone carry the state.

### The child count

The `(n)` beside a row is how many **direct children** it has — an epic's
top-level tickets, or a ticket's subtickets. A row with no count is a leaf, and
**entering a leaf does nothing**: the count is what tells you, before you press
a key, whether there is a level below.

## Keys

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | move the cursor (scroll the preview while zoomed) |
| `g` / `G` | first / last row |
| `Enter` / `l` / `→` | open the highlighted row; nothing if it is a leaf |
| `Backspace` / `Esc` / `h` / `←` | leave the level, back to the row you entered from |
| `Ctrl-D` / `Ctrl-U` | scroll the preview half a screen |
| `PgDn` / `PgUp` / `Space` | scroll the preview a screen |
| `Home` / `End` | preview start / end |
| `<` / `>` | narrow / widen the navigation pane by 5% |
| `=` | restore the default 30/70 split |
| `z` | zoom: the preview fills the width |
| `r` | re-read the store |
| `?` | the key overlay |
| `q` / `Ctrl-C` | quit |

`Esc` leaves a *level*, never the browser — `q` is the only way out. Neither pane
takes focus from the other: the cursor keys always drive the navigation pane and
the paging keys always drive the preview, so there is no mode to switch.

## Resizing, the mouse, and copying text

The divider can be dragged with the mouse, which requires the browser to capture
mouse events — and while it does, the terminal's own click-drag text selection
does not work.

**`z` is the way out of that.** Zooming gives the preview the whole width and
releases the mouse, so you can select and copy from a ticket the usual way; `z`
again restores the split and the divider. While zoomed there is no visible
cursor, so `j`/`k` scroll the preview and entering or leaving a level is
disabled.

The split is per-session: it is not saved between runs.

## Freshness

The browser reads the store when you move, enter or leave a level — and **only
then**. If an agent changes a ticket while you are looking at it, press `r` to
re-read. A reload keeps each level's cursor on the same ticket even if siblings
were added or removed around it, and if a level has disappeared entirely it drops
you at the deepest one that still exists.

One read is not on that list: opening an editing buffer on text the store already
holds re-reads that one entity at that moment, so you start from the current text
rather than from a preview that may be minutes old. The save then applies only
while the entity has not changed since that read; if it has, the browser asks
whether to overwrite the change or go back to your text, and writes nothing until
you answer.
