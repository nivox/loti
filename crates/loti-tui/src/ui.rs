//! Drawing: a breadcrumb bar, the two panes, a hint strip, and the overlay.
//!
//! Layout invariant: the breadcrumb and the hint strip each take exactly one
//! line, top and bottom, and the panes divide what is left. Zoom removes the
//! navigation pane but keeps the breadcrumb, so the reader never loses track of
//! where the previewed ticket sits. A transient notice draws over the strip
//! rather than beside or above it, so nothing under it ever moves.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, Modal, Placement, Surface};
use crate::data::{ReadOnly, Row, RowKind, Selection};
use crate::keymap;
use crate::theme::{glyph, Theme};

/// The separator between breadcrumb entries.
const CRUMB_SEPARATOR: &str = " › ";

/// The rule drawn between a level's collection rows and its work rows.
const STRUCTURE_RULE: &str = "─";

/// What the breadcrumb line's right-hand slot says while editing mode is on. A
/// word rather than a colour, because a mode that only a hue announced would be
/// invisible with colour disabled.
const EDITING_INDICATOR: &str = "── EDITING ──";

/// What the same slot says while the store's own format gate will not let this
/// binary write, one marker per reason.
///
/// It names the reason as well as the state, because the remedy differs by
/// reason: an unmigrated store is the reader's to migrate, a migration in flight
/// is somebody else's and clears on its own, a format newer than the binary
/// needs a newer loti, and a version nothing can parse needs looking at. Words
/// rather than a colour, like every other state this browser signals.
fn read_only_indicator(reason: ReadOnly) -> &'static str {
    match reason {
        ReadOnly::NeedsMigration => "── MIGRATION NEEDED: READ-ONLY ──",
        ReadOnly::MigrationInProgress => "── MIGRATION IN PROGRESS: READ-ONLY ──",
        ReadOnly::NeedsNewerLoti => "── NEWER LOTI NEEDED: READ-ONLY ──",
        ReadOnly::VersionUnreadable => "── VERSION UNREADABLE: READ-ONLY ──",
    }
}

/// The marker the breadcrumb line's state slot carries, and the columns the slot
/// itself takes.
///
/// The slot has two possible occupants and they are mutually exclusive: a store
/// that may not be written can never be in editing mode, because the mode is not
/// offered there and a reload that finds the store read-only leaves it. Where
/// both somehow held, the store's word is the one drawn — it is the durable fact,
/// and it is the one that says the mode is unavailable.
///
/// The slot is as wide as the widest marker **the session could show**, not as
/// the marker being drawn: a reason that changes under a reload — a migration in
/// flight abandoned, leaving a store to migrate — must not shift the path the
/// reader is reading. It is deliberately not sized for every marker in the
/// browser: the read-only markers are three times the mode's, and a store that
/// may be written would then spend half of an eighty-column line reserving room
/// for a marker it can never show.
fn state_slot(app: &App) -> Option<(&'static str, usize)> {
    match app.read_only() {
        Some(reason) => Some((read_only_indicator(reason), widest_read_only_indicator())),
        None => app
            .editing_target()
            .is_some()
            .then(|| (EDITING_INDICATOR, EDITING_INDICATOR.chars().count())),
    }
}

/// The columns the widest read-only marker needs. Taken over every reason rather
/// than hardwired, so a marker reworded to be the longest one cannot quietly
/// start overflowing the slot it is drawn in.
fn widest_read_only_indicator() -> usize {
    ReadOnly::ALL
        .iter()
        .copied()
        .map(|reason| read_only_indicator(reason).chars().count())
        .max()
        .unwrap_or(0)
}

/// The bar drawn in the gutter of editing mode's frozen row. A shape, so the row
/// being acted on is identifiable with colour disabled, where dimming the others
/// carries nothing.
const GUTTER_BAR: &str = "▌";

/// How wide a dialog wants to be. Fixed rather than sized to its content: a
/// refusal can be a paragraph, and a float as wide as the terminal would stop
/// reading as something laid over the screen.
const DIALOG_WIDTH: u16 = 60;

/// Draw one frame.
pub fn draw(f: &mut Frame, app: &mut App) {
    let theme = app.theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_breadcrumb(f, chunks[0], app, theme);

    let body = chunks[1];
    // Where the preview was drawn, because a surface may render there: the pane is
    // the whole body while the preview fills the width and the right-hand pane
    // otherwise, so a surface that renders in the pane follows the pane rather than
    // holding a second opinion about the layout.
    let preview_area;
    if app.zoomed() {
        preview_area = body;
        app.set_divider_column(None);
        app.sync_preview(preview_wrap_width(body.width));
        let title = app.preview_title();
        let viewer = app.preview_viewer();
        *viewer = std::mem::take(viewer).with_title(title);
        viewer.render(f, body, &theme);
    } else {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(app.nav_percent()),
                Constraint::Percentage(100 - app.nav_percent()),
            ])
            .split(body);
        preview_area = panes[1];
        // The divider is where the two pane borders meet, which is the column a
        // drag has to grab.
        app.set_divider_column(Some(panes[1].x));
        draw_nav(f, panes[0], app, theme);

        app.sync_preview(preview_wrap_width(panes[1].width));
        let title = app.preview_title();
        let viewer = app.preview_viewer();
        *viewer = std::mem::take(viewer).with_title(title);
        viewer.render(f, panes[1], &theme);
    }

    f.render_widget(
        Paragraph::new(Line::from(footer(app, chunks[2].width))),
        chunks[2],
    );

    // The surface is drawn under whatever was raised about it: a warning covers
    // the buffer it asks about rather than replacing it, so the buffer is still
    // there to land back in.
    if let Some(surface) = app.surface() {
        draw_surface(f, theme, surface, preview_area);
    }

    match app.modal() {
        Some(Modal::Help) => draw_help(f, f.area(), theme),
        // One widget for every dialog: it draws what the dialog carries, so a
        // further kind of question needs nothing here.
        Some(Modal::Dialog(dialog)) => draw_dialog(
            f,
            theme,
            dialog.title(),
            dialog.message(),
            &keymap::dialog_answers(dialog.answers(), dialog.words()),
        ),
        None => {}
    }
}

/// An editing surface: its fields, drawn where the surface says and above
/// everything, reflowing nothing underneath.
///
/// Where it goes is the surface's own say and never read off what it holds — see
/// [`surface_area`]. A float has the same geometry as a dialog, because a reader
/// looks for both in the same place.
///
/// It lists no keys of its own: the hint strip carries them, and unlike a dialog a
/// surface is not a question with answers but a buffer with a mode's worth of keys.
///
/// The terminal's own cursor is placed in the focused field, because a text field
/// with no cursor does not say where the next character lands.
fn draw_surface(f: &mut Frame, theme: Theme, surface: &Surface, pane: Rect) {
    let fields = surface.fields();
    let popup = surface_area(surface.placement(), f.area(), pane, fields.len());
    let width = popup.width;
    let label_width = fields
        .iter()
        .map(|field| field.label().chars().count())
        .max()
        .unwrap_or(0);
    let value_width = value_width(width, label_width);
    let lines: Vec<Line> = fields
        .iter()
        .map(|field| {
            let (shown, _) = field_window(field.value(), field.cursor(), value_width);
            Line::from(vec![
                Span::styled(
                    format!(" {:<label_width$}  ", field.label()),
                    Style::default().fg(theme.muted()),
                ),
                Span::raw(shown),
            ])
        })
        .collect();
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.notice()))
                .title(surface.title().to_string()),
        ),
        popup,
    );
    if let Some(field) = fields.get(surface.focus()) {
        let (_, cursor) = field_window(field.value(), field.cursor(), value_width);
        f.set_cursor_position((
            popup.x + 1 + 1 + label_width as u16 + 2 + cursor as u16,
            popup.y + 1 + surface.focus() as u16,
        ));
    }
}

/// The cells a surface draws in, which the surface itself decides.
///
/// A float is centred on the whole terminal and as tall as its fields need, like
/// every other float. A surface that renders in the pane takes the pane exactly, so
/// the frozen row stays visible in the navigation pane beside it — and it follows
/// the pane wherever the pane is, rather than answering the separate question of
/// whether an open surface may fill the width.
fn surface_area(placement: Placement, screen: Rect, pane: Rect, fields: usize) -> Rect {
    match placement {
        Placement::Float => centred(screen, DIALOG_WIDTH, fields as u16 + 2),
        Placement::Pane => pane,
    }
}

/// The columns a field's value may use: the surface less its borders, the leading
/// indent, the label column and the gap after it.
fn value_width(width: u16, label_width: usize) -> usize {
    (width as usize)
        .saturating_sub(2 + 1 + label_width + 2)
        .max(1)
}

/// The part of a one-line value a field this wide shows, and where the cursor
/// falls inside it.
///
/// A field holds one line, so content wider than the field scrolls rather than
/// wrapping — and the part shown always contains the cursor, or a reader typing at
/// the end of a long value would be typing off the screen. One column is kept for
/// the cursor itself, which has to sit somewhere when it is past the last
/// character.
fn field_window(value: &str, cursor: usize, width: usize) -> (String, usize) {
    let chars: Vec<char> = value.chars().collect();
    let width = width.max(1);
    if chars.len() < width {
        return (value.to_string(), cursor.min(chars.len()));
    }
    let last_start = chars.len() + 1 - width;
    let start = cursor.saturating_sub(width - 1).min(last_start);
    let end = (start + width - 1).min(chars.len());
    (chars[start..end].iter().collect(), cursor - start)
}

/// A dialog: centred on the whole terminal, above everything, and it never
/// reflows what is underneath — the panes and the strip stay exactly where they
/// were, merely covered.
///
/// Centred rather than anchored to whatever raised it, because a question that
/// moves is harder to spot and the centre is the one position that never collides
/// with the row or the buffer being asked about.
///
/// It lists its own answers, so the way out of it never depends on the hint strip
/// — which a notice, or a narrow terminal, may have taken. Its text wraps inside
/// the float, because a store refusal is as long as the store made it.
fn draw_dialog(f: &mut Frame, theme: Theme, title: &str, message: &str, answers: &[String]) {
    let area = f.area();
    let width = DIALOG_WIDTH.min(area.width);
    let text_width = dialog_text_width(width);
    let text = wrap(message, text_width);
    // As tall as its text needs, and never taller than the terminal: a float that
    // ran off the screen would take its own answers with it.
    let popup = centred(area, width, text.len() as u16 + 4);
    let lines = dialog_lines(
        &text,
        answers,
        text_width,
        popup.height.saturating_sub(2) as usize,
        theme,
    );
    // Above everything: the cells underneath are cleared rather than blended, so
    // no pane text shows through the float.
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                // The colour a transient notice takes: a dialog is the critical
                // end of the same channel, and its title says which in words.
                .border_style(Style::default().fg(theme.notice()))
                .title(title),
        ),
        popup,
    );
}

/// The columns a dialog's text may use: the float less its two borders and a
/// column of padding either side.
fn dialog_text_width(width: u16) -> usize {
    (width.saturating_sub(4)).max(1) as usize
}

/// A dialog's interior, fitted to the lines the float has: the message, then a
/// blank line and the answers, which are the last thing the eye lands on before
/// it acts.
///
/// Ranked, because a terminal shorter than the message has to give something up:
/// **the answers always survive** — a dialog listing no way to answer it seals the
/// reader inside it — then as much of the message as is left, and the blank
/// separator is the first thing surrendered.
fn dialog_lines<'a>(
    text: &[String],
    answers: &[String],
    width: usize,
    height: usize,
    theme: Theme,
) -> Vec<Line<'a>> {
    let mut lines: Vec<Line> = Vec::new();
    if height == 0 {
        return lines;
    }
    let (text_height, separated) = match height - 1 {
        remaining @ (0 | 1) => (remaining, false),
        remaining => (remaining - 1, true),
    };
    lines.extend(
        fit(text, text_height, width)
            .into_iter()
            .map(|line| Line::from(format!(" {line}"))),
    );
    if separated {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        format!(" {}", answers.join(keymap::HINT_SEPARATOR)),
        Style::default().fg(theme.muted()),
    )));
    lines
}

/// As many of `text`'s lines as `height` allows, marking the cut when some are
/// left out, so a message a short terminal shortened cannot be read as the whole
/// of one.
fn fit(text: &[String], height: usize, width: usize) -> Vec<String> {
    if text.len() <= height {
        return text.to_vec();
    }
    let mut kept = text[..height].to_vec();
    if let Some(last) = kept.last_mut() {
        let head: String = last.chars().take(width.saturating_sub(1)).collect();
        *last = format!("{head}…");
    }
    kept
}

/// Break text into lines no wider than `width`, at spaces where there is one and
/// mid-word only where a single word is wider than the float.
///
/// The browser wraps rather than clipping here because the text can be the
/// store's own: a refusal naming several offending entities is long by nature,
/// and a clipped rule teaches half a rule.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let mut word = word;
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut line));
            }
            // A word too long for a line of its own is cut across lines: leaving
            // it whole would push it past the border. Every cut consumes at least
            // one character, so a float narrower than a character terminates
            // instead of breaking forever.
            while word.chars().count() > width {
                let (head, tail) = split_at_chars(word, width.max(1));
                out.push(head.to_string());
                word = tail;
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
    }
    out
}

/// Split at a character boundary `count` characters in, so a multi-byte character
/// is never cut in half.
fn split_at_chars(text: &str, count: usize) -> (&str, &str) {
    let at = text
        .char_indices()
        .nth(count)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text.split_at(at)
}

/// A float of this size, centred on `area` and never larger than it.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The width markdown wraps to inside the preview: the pane less its border and
/// the padding the viewer adds, and less one column for the scrollbar it draws
/// when the document overflows.
fn preview_wrap_width(pane_width: u16) -> u16 {
    pane_width.saturating_sub(5).max(1)
}

/// The columns a full-width line may use, allowing for the one-column indent.
fn area_text_width(width: u16) -> usize {
    width.saturating_sub(1) as usize
}

/// The bottom line: a live notice, or the hint strip.
///
/// A notice replaces the whole strip for its lifetime — one line, never wrapped,
/// truncated if it does not fit — so the essential hints are hidden for those
/// seconds and a notice's own wording has to carry the way out where that
/// matters. It is painted in the notice colour, so it reads as a message rather
/// than as one more binding.
///
/// The strip is the keys that apply right now, so each mode brings its own: while
/// editing, neither the level's keys nor `q` are among them.
fn footer(app: &App, width: u16) -> Span<'static> {
    let theme = app.theme();
    let columns = area_text_width(width);
    match app.flash_message() {
        Some(message) => Span::styled(
            format!(" {}", truncate(message, columns)),
            Style::default().fg(theme.notice()),
        ),
        None => {
            // Editing mode's droppable hints are the actions the frozen row
            // offers, which only the state machine knows; browse mode's are the
            // same on every row. An open surface has no row offering anything: the
            // keys that apply are its own, and the way out is out of the buffer
            // rather than out of the mode.
            let (hints, essential) = match (app.surface(), app.editing_target().is_some()) {
                // Which of the surface's keys apply depends on how many fields it
                // holds, so the strip asks the surface for its shape exactly as the
                // key map does: the strip and the keys cannot then disagree.
                (Some(surface), _) => (
                    keymap::footer_hints_surface(surface.shape().fields),
                    keymap::FOOTER_ESSENTIAL_SURFACE,
                ),
                (None, true) => (app.editing_hints(), keymap::FOOTER_ESSENTIAL_EDITING),
                (None, false) => (keymap::FOOTER_HINTS.to_vec(), keymap::FOOTER_ESSENTIAL),
            };
            Span::styled(
                format!(" {}", hint_strip(columns, &hints, essential)),
                Style::default().fg(theme.muted()),
            )
        }
    }
}

/// Build the hint strip for a width: as many droppable hints as fit, whole, never
/// a hint cut in half, and then the mode's essential pair.
///
/// A width too narrow for even the pair is clipped rather than shortened or
/// laddered, and clipping eats the tail — which is why the pair is ranked with
/// the way out first: help is the hint that goes.
fn hint_strip(width: usize, hints: &[&str], essential_hints: &[&str]) -> String {
    let essential = essential_hints.join(keymap::HINT_SEPARATOR);
    let mut shown: Vec<&str> = Vec::new();
    for hint in hints {
        let candidate = shown
            .iter()
            .chain(std::iter::once(hint))
            .copied()
            .collect::<Vec<_>>()
            .join(keymap::HINT_SEPARATOR);
        let length = candidate.chars().count()
            + keymap::HINT_SEPARATOR.chars().count()
            + essential.chars().count();
        if length > width {
            break;
        }
        shown.push(hint);
    }
    shown.extend_from_slice(essential_hints);
    let strip = shown.join(keymap::HINT_SEPARATOR);
    // Narrower than even the essential hints: clip, since something is better
    // than a blank strip.
    strip.chars().take(width).collect()
}

fn draw_breadcrumb(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let crumbs = app.nav().crumbs();
    // The state of the session — the mode it is in, or the store refusing to be
    // written — holds the right-hand end of the line and the breadcrumb elides
    // into what is left: a session-level fact must not be the thing a narrow
    // terminal scrolls away. One column of gap, so a crumb can never run into the
    // marker.
    let slot = state_slot(app);
    let reserved = slot.map_or(0, |(_, width)| width + 1);
    let text = elide_left(
        &crumbs,
        area_text_width(area.width).saturating_sub(reserved),
    );
    let path = Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD);
    // A collection is structure, not work, so its crumb is dimmed exactly as its
    // row is. Elision drops from the left, so the deepest entry is the one that
    // survives — and it is the one to style.
    let spans = match app.nav().at_collection() {
        true => match text.rsplit_once(CRUMB_SEPARATOR) {
            Some((head, tail)) => vec![
                Span::styled(format!(" {head}{CRUMB_SEPARATOR}"), path),
                Span::styled(tail.to_string(), Style::default().fg(theme.muted())),
            ],
            // Elision kept nothing but the collection itself.
            None => vec![Span::styled(
                format!(" {text}"),
                Style::default().fg(theme.muted()),
            )],
        },
        false => vec![Span::styled(format!(" {text}"), path)],
    };
    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
        area,
    );
    if let Some((marker, _)) = slot {
        // The colour a notice takes, which the navigation pane's border takes
        // while the mode is on too, so a marker and what else the state changed on
        // screen read as one fact. Painted rather than relied on: the words are
        // what carry the state, and the colour only keeps the marker from reading
        // as the deepest crumb.
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                marker,
                Style::default()
                    .fg(theme.notice())
                    .add_modifier(Modifier::BOLD),
            )))
            .alignment(Alignment::Right),
            area,
        );
    }
}

/// Join breadcrumb entries to fit, dropping from the left. The tail says where
/// you are, so the tail is what must survive a narrow terminal.
fn elide_left(crumbs: &[&str], width: usize) -> String {
    let full = crumbs.join(CRUMB_SEPARATOR);
    if full.chars().count() <= width {
        return full;
    }
    for start in 1..crumbs.len() {
        let tail = crumbs[start..].join(CRUMB_SEPARATOR);
        let candidate = format!("…{CRUMB_SEPARATOR}{tail}");
        if candidate.chars().count() <= width {
            return candidate;
        }
    }
    // Even the deepest entry alone does not fit: truncate it.
    let last = crumbs.last().copied().unwrap_or_default();
    last.chars().take(width).collect()
}

fn draw_nav(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    // While editing mode is on the whole pane is in a mode — one row can be acted
    // on and no other row can be reached — so the frame around it says so.
    let editing = app.editing_target().is_some();
    let border = match editing {
        true => theme.notice(),
        false => theme.muted(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(" navigation ");
    let inner = block.inner(area);

    let rows = app.nav().rows();
    let items: Vec<ListItem> = if rows.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "(no epics)",
            Style::default().fg(theme.muted()),
        )))]
    } else {
        let label_width = rows
            .iter()
            .map(|r| r.label.chars().count())
            .max()
            .unwrap_or(0);
        let count_width = rows
            .iter()
            .map(|r| count_cell(r).chars().count())
            .max()
            .unwrap_or(0);
        let rule_at = rule_position(rows);
        let mut items: Vec<ListItem> = Vec::with_capacity(rows.len() + 1);
        for (index, row) in rows.iter().enumerate() {
            if Some(index) == rule_at {
                items.push(ListItem::new(Line::from(Span::styled(
                    STRUCTURE_RULE.repeat(inner.width as usize),
                    Style::default().fg(theme.muted()),
                ))));
            }
            items.push(ListItem::new(row_line(
                row,
                theme,
                label_width,
                count_width,
                inner.width as usize,
                emphasis(editing, index == app.nav().cursor()),
            )));
        }
        items
    };

    // Editing mode's frozen row is marked by its gutter bar and by being the one
    // row not dimmed; inverting it as well would keep the level looking like a
    // list being browsed, which is exactly what it has stopped being.
    let highlight = match editing {
        true => Style::default().add_modifier(Modifier::BOLD),
        false => Style::default()
            .add_modifier(Modifier::REVERSED)
            .add_modifier(Modifier::BOLD),
    };
    let list = List::new(items).block(block).highlight_style(highlight);
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(highlight_index(rows, app.nav().cursor())));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// Where the rule that separates structure from work goes: before the first work
/// row of a level that leads with collection rows.
///
/// `None` when there is nothing to separate — the roster, a collection's members,
/// or an epic or node whose only rows are its collections — since a rule with
/// nothing under it says the level is missing something.
fn rule_position(rows: &[Row]) -> Option<usize> {
    let collections = rows
        .iter()
        .take_while(|r| matches!(r.kind, RowKind::Collection(_)))
        .count();
    (collections > 0 && collections < rows.len()).then_some(collections)
}

/// The list index of the cursor's row. The rule is an item of the list but not a
/// row of the level, so everything below it is drawn one line further down than
/// the cursor counts — and the highlight must land on the row, never on the rule.
fn highlight_index(rows: &[Row], cursor: usize) -> usize {
    match rule_position(rows) {
        Some(at) if cursor >= at => cursor + 1,
        _ => cursor,
    }
}

/// The child-count cell: how many direct children the row has, or nothing when
/// it has none.
///
/// For a collection row an absent count is the signal that entering it would do
/// nothing. For an epic or a node it means "no subtickets" only — every epic and
/// node is enterable, because its collections are always rows there.
fn count_cell(row: &Row) -> String {
    if row.children == 0 {
        String::new()
    } else {
        format!("({})", row.children)
    }
}

/// How a row is drawn relative to what the reader can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Emphasis {
    /// A row of a level being browsed.
    Plain,
    /// The row the cursor is on while browsing.
    Cursor,
    /// Editing mode's frozen row: the one row that can be acted on.
    Target,
    /// Every other row while editing mode is on. It really is disabled — the
    /// selection cannot move to it — so it is drawn that way.
    Disabled,
}

/// How the row at the cursor and the rows around it are drawn in each mode.
fn emphasis(editing: bool, at_cursor: bool) -> Emphasis {
    match (editing, at_cursor) {
        (true, true) => Emphasis::Target,
        (true, false) => Emphasis::Disabled,
        (false, true) => Emphasis::Cursor,
        (false, false) => Emphasis::Plain,
    }
}

/// One row: `<glyph> <identifier> <(children)>  <name>`. Everything the eye
/// needs to judge a row sits on the left, so the name may be truncated at the
/// pane edge without losing state or enterability.
///
/// The identifier is the column a reader types into a command, so it carries the
/// state's colour and full contrast; only the child count is muted, since it is
/// a hint rather than something to read off.
///
/// A row that is not work has an empty glyph column — inventing a glyph would
/// claim a state it has not got — so a glyph in that column is itself the signal
/// that the row is work. A collection row and a withdrawn comment are dim over
/// their whole width, because neither is work to be done.
///
/// The highlighted row is drawn without any per-column colour: the selection is
/// shown by inverting the line, and inverting a line whose columns each carry
/// their own foreground breaks the bar into mismatched blocks.
///
/// Editing mode inverts nothing and adds a gutter column instead: the frozen row
/// keeps every column's own colour and takes the bar, and every other row is
/// dimmed by modifier as well as by colour, so which row is being acted on is
/// legible with colour disabled too. The gutter is reserved only while the mode is
/// on, so a level being browsed spends no width on it.
fn row_line<'a>(
    row: &'a Row,
    theme: Theme,
    label_width: usize,
    count_width: usize,
    pane_width: usize,
    emphasis: Emphasis,
) -> Line<'a> {
    let muted = Style::default().fg(theme.muted());
    let disabled = muted.add_modifier(Modifier::DIM);
    let plain = Style::default();
    let (glyph_cell, id_style, name_style) = match &row.kind {
        RowKind::Work(status) => {
            // Only an epic, a node or a blocker is ever work, and the last two
            // are nodes: an epic's states are its own.
            let color = match &row.selection {
                Selection::Epic(_) => theme.epic_status(status),
                _ => theme.node_status(status),
            };
            (glyph(status), Style::default().fg(color), plain)
        }
        RowKind::Collection(_) | RowKind::Withdrawn => (" ", muted, muted),
        RowKind::Member => (" ", plain, plain),
        // A member the store lists and the browser could not read. Its identifier
        // is real and reads as one; what stands where its detail would go is the
        // reason, in the notice colour, because it is a message rather than
        // something the store holds. The row's own words say so with colour off.
        RowKind::Unreadable => (" ", plain, Style::default().fg(theme.notice())),
    };
    let (id_style, count_style, name_style) = match emphasis {
        Emphasis::Cursor => (plain, plain, plain),
        Emphasis::Disabled => (disabled, disabled, disabled),
        Emphasis::Plain | Emphasis::Target => (id_style, muted, name_style),
    };
    let gutter = match emphasis {
        Emphasis::Target => Some(Span::styled(
            GUTTER_BAR,
            Style::default().fg(theme.notice()),
        )),
        // The bar's column, kept blank, so the rows of a level stay aligned.
        Emphasis::Disabled => Some(Span::raw(" ")),
        Emphasis::Plain | Emphasis::Cursor => None,
    };
    let prefix_width = usize::from(gutter.is_some()) + 2 + label_width + 1 + count_width + 2;
    let name_budget = pane_width.saturating_sub(prefix_width);
    let mut spans = Vec::with_capacity(8);
    spans.extend(gutter);
    spans.extend([
        Span::styled(glyph_cell, id_style),
        Span::raw(" "),
        Span::styled(format!("{:<label_width$}", row.label), id_style),
        Span::raw(" "),
        Span::styled(format!("{:<count_width$}", count_cell(row)), count_style),
        Span::raw("  "),
        Span::styled(truncate(&row.name, name_budget), name_style),
    ]);
    Line::from(spans)
}

/// Truncate to a column budget, marking the cut so a clipped name cannot be
/// mistaken for a short one.
fn truncate(text: &str, budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    if text.chars().count() <= budget {
        return text.to_string();
    }
    let kept: String = text.chars().take(budget.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// The key column's width: the widest binding, so descriptions line up.
fn help_key_width() -> usize {
    keymap::HELP
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
}

/// The overlay's width. Sized from its own content — the widest
/// `<keys>  <description>` row plus borders — so a binding's description is
/// never clipped by a popup that guessed too narrow. A terminal too small for
/// that still bounds it.
fn help_width(available: u16) -> u16 {
    let widest = keymap::HELP
        .iter()
        .map(|(_, what)| help_key_width() + 2 + what.chars().count())
        .max()
        .unwrap_or(0);
    let wanted = widest as u16 + 2;
    wanted.min(available.saturating_sub(4)).max(20)
}

fn draw_help(f: &mut Frame, area: Rect, theme: Theme) {
    let width = help_width(area.width);
    let height = (keymap::HELP.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = centred(area, width, height);
    let key_width = help_key_width();
    let lines: Vec<Line> = keymap::HELP
        .iter()
        .map(|(keys, what)| {
            Line::from(vec![
                Span::styled(
                    format!("{keys:<key_width$}"),
                    Style::default().fg(theme.accent()),
                ),
                Span::raw("  "),
                Span::raw(*what),
            ])
        })
        .collect();
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.accent()))
                .title(" keys — ? or Esc to close "),
        ),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_slot_is_never_narrower_than_the_marker_drawn_in_it() {
        // The slot's width is derived from the markers rather than hardwired, and
        // this is what makes that derivation load-bearing: a slot narrower than the
        // marker it holds does not fail to draw, it draws over the tail of the path
        // beside it, so the reader loses a crumb with nothing saying they did.
        for reason in ReadOnly::ALL.iter().copied() {
            let marker = read_only_indicator(reason);
            assert!(
                marker.chars().count() <= widest_read_only_indicator(),
                "{reason:?} is wider than the slot reserved for it: {marker:?}"
            );
        }
    }

    #[test]
    fn a_breadcrumb_that_fits_is_left_whole() {
        assert_eq!(elide_left(&["epics", "feature"], 40), "epics › feature");
    }

    #[test]
    fn a_long_breadcrumb_keeps_its_tail() {
        let crumbs = ["epics", "feature", "1 a long ticket name", "8 deeper still"];
        let elided = elide_left(&crumbs, 30);
        assert!(elided.starts_with('…'), "{elided}");
        assert!(elided.ends_with("8 deeper still"), "{elided}");
        assert!(elided.chars().count() <= 30, "{elided}");
    }

    #[test]
    fn a_row_with_nothing_under_it_has_no_count_cell() {
        let childless = crate::data::fixture::epic_row("e", 0);
        let parent = crate::data::fixture::epic_row("e", 3);
        assert_eq!(count_cell(&childless), "");
        assert_eq!(count_cell(&parent), "(3)");
    }

    /// A node level as the browser builds it: its collections, then its work.
    fn mixed_level() -> Vec<Row> {
        use crate::data::fixture::{collection_row, node_container, node_row};
        let container = node_container("e", 1);
        let mut rows: Vec<Row> = container
            .collections()
            .iter()
            .map(|kind| collection_row(container.clone(), *kind, 0))
            .collect();
        rows.push(node_row("e", 2, 0));
        rows
    }

    #[test]
    fn the_rule_sits_between_the_collections_and_the_work() {
        let rows = mixed_level();
        assert_eq!(rule_position(&rows), Some(rows.len() - 1));
    }

    #[test]
    fn a_level_with_nothing_to_separate_gets_no_rule() {
        use crate::data::fixture::{epic_row, label_row};
        use crate::data::Container;
        // The roster: work rows only.
        assert_eq!(rule_position(&[epic_row("a", 0)]), None);
        // A collection's members: no work rows at all.
        let container = Container::Epic("a".into());
        assert_eq!(rule_position(&[label_row(container, "ui")]), None);
        // A childless ticket: collections and nothing to separate them from.
        let mut collections = mixed_level();
        collections.pop();
        assert_eq!(rule_position(&collections), None);
    }

    #[test]
    fn the_highlight_never_lands_on_the_rule() {
        let rows = mixed_level();
        let at = rule_position(&rows).unwrap();
        for cursor in 0..rows.len() {
            let index = highlight_index(&rows, cursor);
            assert_ne!(index, at, "cursor {cursor} highlighted the rule");
            // Every row keeps its own line: the rule shifts only what follows it.
            assert_eq!(index, if cursor < at { cursor } else { cursor + 1 });
        }
    }

    #[test]
    fn only_a_work_row_carries_a_glyph() {
        let theme = Theme::with_color(false);
        for row in mixed_level() {
            let line = row_line(&row, theme, 2, 3, 40, Emphasis::Plain);
            let cell = line.spans[0].content.to_string();
            let is_work = matches!(row.kind, RowKind::Work(_));
            assert_eq!(
                cell != " ",
                is_work,
                "{:?} drew the glyph cell {cell:?}",
                row.kind
            );
        }
    }

    #[test]
    fn editing_marks_the_frozen_row_and_disables_the_others_without_colour() {
        // Colour disabled: whatever distinguishes the rows here is shape and
        // modifier, which is what a `NO_COLOR` reader has.
        let theme = Theme::with_color(false);
        let row = crate::data::fixture::epic_row("e", 0);
        let target = row_line(&row, theme, 2, 3, 40, Emphasis::Target);
        let disabled = row_line(&row, theme, 2, 3, 40, Emphasis::Disabled);
        let browsed = row_line(&row, theme, 2, 3, 40, Emphasis::Plain);

        // The bar is the frozen row's own column; the others keep it blank so the
        // level stays aligned, and a browsed level spends no width on it at all.
        assert_eq!(target.spans[0].content, GUTTER_BAR);
        assert_eq!(disabled.spans[0].content, " ");
        assert_eq!(browsed.spans[0].content, super::glyph("open"));

        let dimmed = |line: &Line| {
            line.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::DIM))
        };
        assert!(dimmed(&disabled), "a disabled row has to read as disabled");
        assert!(
            !dimmed(&target),
            "the row being acted on keeps its contrast"
        );

        // The bar's column is charged to the name budget like every other
        // column. Left uncharged, a row whose name fills the budget is one column
        // wider than its pane, and the renderer clips the last character — which
        // is the cut marker, so a clipped name would read as a whole one.
        let width =
            |line: &Line| -> usize { line.spans.iter().map(|s| s.content.chars().count()).sum() };
        let mut long = row.clone();
        long.name = "a name longer than any pane is wide".repeat(2);
        for emphasis in [Emphasis::Target, Emphasis::Disabled, Emphasis::Plain] {
            let line = row_line(&long, theme, 2, 3, 40, emphasis);
            assert!(
                width(&line) <= 40,
                "a {emphasis:?} row overruns its pane by {}",
                width(&line) - 40
            );
        }
    }

    #[test]
    fn the_overlay_is_wide_enough_for_every_binding_it_lists() {
        // On a terminal with room to spare, and on the ordinary eighty- and
        // hundred-column ones too: the overlay is bounded by the screen, so a
        // description longer than that is clipped, and half a binding teaches a key
        // that does something else.
        for available in [80, 100, 200] {
            let width = help_width(available) as usize;
            for (keys, what) in keymap::HELP {
                let row = help_key_width() + 2 + what.chars().count();
                assert!(
                    row + 2 <= width,
                    "{keys:?} / {what:?} needs {row} columns inside a {width}-wide overlay"
                );
            }
        }
    }

    #[test]
    fn the_hint_strip_never_clips_a_hint_in_half() {
        for width in 10..120usize {
            let strip = hint_strip(width, keymap::FOOTER_HINTS, keymap::FOOTER_ESSENTIAL);
            assert!(strip.chars().count() <= width, "width {width}: {strip:?}");
            if width >= 40 {
                for hint in strip.split(keymap::HINT_SEPARATOR) {
                    assert!(
                        keymap::FOOTER_HINTS.contains(&hint)
                            || keymap::FOOTER_ESSENTIAL.contains(&hint),
                        "width {width} produced a partial hint {hint:?}"
                    );
                }
            }
        }
    }

    /// Every strip a mode can ask for: the essential pair travels with the
    /// droppable hints it is appended to. Editing mode's are taken at their
    /// widest — every action any row offers — since a row shows a subset, and a
    /// surface's are taken once per field shape, since the shape decides which of
    /// its keys apply.
    fn strips() -> Vec<(Vec<&'static str>, &'static [&'static str])> {
        let mut strips = vec![
            (keymap::FOOTER_HINTS.to_vec(), keymap::FOOTER_ESSENTIAL),
            (
                keymap::FOOTER_HINTS_EDITING
                    .iter()
                    .map(|(_, hint)| *hint)
                    .collect(),
                keymap::FOOTER_ESSENTIAL_EDITING,
            ),
        ];
        for fields in crate::action::Fields::ALL.iter().copied() {
            strips.push((
                keymap::footer_hints_surface(fields),
                keymap::FOOTER_ESSENTIAL_SURFACE,
            ));
        }
        strips
    }

    #[test]
    fn the_way_out_is_the_hint_a_narrow_strip_keeps() {
        for (hints, essential) in strips() {
            let pair = essential.join(keymap::HINT_SEPARATOR);
            let way_out = essential[0];
            for width in 1..120usize {
                let strip = hint_strip(width, &hints, essential);
                assert!(strip.chars().count() <= width, "width {width}: {strip:?}");
                if width >= pair.chars().count() {
                    // Wide enough for the pair: both are there, and the pair is
                    // the tail whatever else fits before it.
                    assert!(strip.ends_with(&pair), "width {width}: {strip:?}");
                } else if width >= way_out.chars().count() {
                    // Too narrow for the pair: clipping eats help, never the way
                    // out, so a reader is never sealed in with no visible exit.
                    assert!(strip.starts_with(way_out), "width {width}: {strip:?}");
                }
            }
        }
    }

    #[test]
    fn a_wide_strip_carries_every_hint() {
        for (hints, essential) in strips() {
            let strip = hint_strip(200, &hints, essential);
            for hint in hints.iter().chain(essential.iter()) {
                assert!(strip.contains(hint), "{strip:?} is missing {hint:?}");
            }
        }
    }

    #[test]
    fn a_clipped_name_is_marked() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a longer name", 6), "a lon…");
        assert_eq!(truncate("anything", 0), "");
    }

    #[test]
    fn wrapped_text_stays_inside_the_float_and_keeps_the_stores_own_breaks() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        // A word wider than the float is cut across lines rather than pushed past
        // its border.
        assert_eq!(
            wrap("supercalifragilistic", 6),
            vec!["superc", "alifra", "gilist", "ic"]
        );
        // A refusal's own line breaks are the store's paragraphs, so they survive.
        assert_eq!(wrap("first\nsecond", 20), vec!["first", "second"]);
        for width in 1..40usize {
            for line in wrap(
                "a refusal naming several offending descendants, at length",
                width,
            ) {
                assert!(line.chars().count() <= width, "width {width}: {line:?}");
            }
        }
    }

    #[test]
    fn a_dialog_too_tall_for_the_screen_gives_up_its_message_not_its_answers() {
        let theme = Theme::with_color(false);
        let text: Vec<String> = (0..6).map(|n| format!("line {n}")).collect();
        let answers = ["Esc cancel".to_string()];
        let rendered = |height: usize| -> Vec<String> {
            dialog_lines(&text, &answers, 20, height, theme)
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|s| s.content.to_string())
                        .collect::<String>()
                })
                .collect()
        };

        // Whatever the float has room for, the answers are on the last line of it:
        // a dialog listing no way to answer it seals the reader inside it. What
        // gives way instead is the message — and it says when it did, so half a
        // rule cannot be read as the whole of one.
        for height in 1..=text.len() + 2 {
            let shown = rendered(height);
            assert_eq!(shown.len(), height, "height {height}: {shown:?}");
            assert!(
                shown.last().unwrap().contains("Esc cancel"),
                "height {height}: {shown:?}"
            );
            let message: Vec<&String> = shown.iter().filter(|l| l.contains("line")).collect();
            if message.is_empty() {
                // A float with room for one line spends it on the answers: there
                // is no line left for the message, so none to mark either.
                assert_eq!(height, 1, "{shown:?}");
                continue;
            }
            assert_eq!(
                message.len() == text.len(),
                !shown.iter().any(|l| l.contains('…')),
                "height {height}: {shown:?}"
            );
        }
        // Room for nothing: a border with a stray line in it says less than none.
        assert!(rendered(0).is_empty());
    }

    #[test]
    fn a_field_shows_the_part_of_its_content_the_cursor_is_in() {
        // Content that fits is shown whole, with the cursor where it is.
        assert_eq!(field_window("short", 2, 20), ("short".to_string(), 2));

        // A field holds one line, so content wider than the field scrolls: what is
        // shown always contains the cursor, or a reader typing at the end of a long
        // value would be typing off the screen.
        let long = "a label somebody pasted a whole sentence into";
        for cursor in 0..=long.chars().count() {
            for width in 1..12usize {
                let (shown, at) = field_window(long, cursor, width);
                assert!(shown.chars().count() < width.max(2), "{width}: {shown:?}");
                assert!(at <= shown.chars().count(), "{width}/{cursor}: {at}");
                assert!(at < width, "{width}/{cursor}: the cursor is off the field");
                // The window is a run of the value, never a re-arrangement of it.
                assert!(long.contains(&shown), "{shown:?}");
            }
        }
        // At the start the window starts at the start; at the end it ends there, so
        // what the reader last typed is what they can see.
        assert_eq!(field_window(long, 0, 10).0, "a label s");
        let (tail, at) = field_window(long, long.chars().count(), 10);
        assert!(long.ends_with(&tail), "{tail:?}");
        assert_eq!(at, tail.chars().count(), "the cursor sits after the text");
    }

    #[test]
    fn a_surface_draws_where_it_says_and_the_centred_float_is_one_answer_of_more_than_one() {
        let screen = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let pane = Rect {
            x: 30,
            y: 1,
            width: 50,
            height: 22,
        };
        // Exhaustive over the answers, so a further answer has to say here where it
        // draws rather than inheriting whatever the last arm did.
        for placement in Placement::ALL.iter().copied() {
            let area = surface_area(placement, screen, pane, 1);
            match placement {
                // Centred on the whole terminal and as tall as its fields need,
                // wherever the panes happen to be: a reader looks for a float in the
                // same place whatever raised it.
                Placement::Float => {
                    assert_eq!((area.width, area.height), (DIALOG_WIDTH, 3));
                    assert_eq!(area.x, (screen.width - area.width) / 2);
                    assert_eq!(area.y, (screen.height - area.height) / 2);
                    assert_ne!(area, pane, "a float is not the pane");
                }
                // The pane exactly, so the navigation pane keeps the frozen row
                // visible beside it.
                Placement::Pane => assert_eq!(area, pane),
            }
        }
        // The pane answer follows the pane rather than a shape of its own: it is the
        // pane's width and the pane's place, whichever those are.
        let wide = Rect {
            x: 0,
            y: 1,
            width: 80,
            height: 22,
        };
        assert_eq!(surface_area(Placement::Pane, screen, wide, 1), wide);
        // A float is as tall as the fields it holds, and bounded by the screen like
        // every other float — a surface drawn off the edge would take the field the
        // reader is typing into with it.
        assert_eq!(surface_area(Placement::Float, screen, pane, 3).height, 5);
        let tiny = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 4,
        };
        let float = surface_area(Placement::Float, tiny, pane, 8);
        assert_eq!((float.width, float.height), (tiny.width, tiny.height));
    }

    #[test]
    fn a_float_is_centred_and_never_larger_than_the_screen() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let popup = centred(area, 60, 6);
        assert_eq!((popup.x, popup.width), (10, 60));
        assert_eq!((popup.y, popup.height), (9, 6));

        // Bounded by the terminal rather than drawn off it, so a float always has
        // its own borders — and its answers — on the screen.
        let big = centred(area, 200, 100);
        assert_eq!((big.x, big.y, big.width, big.height), (0, 0, 80, 24));
    }
}
