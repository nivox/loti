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

use crate::app::App;
use crate::data::{Row, RowKind, Selection};
use crate::keymap;
use crate::theme::{glyph, Theme};

/// The separator between breadcrumb entries.
const CRUMB_SEPARATOR: &str = " › ";

/// The rule drawn between a level's collection rows and its work rows.
const STRUCTURE_RULE: &str = "─";

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
    if app.zoomed() {
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

    if app.modal().is_some() {
        draw_help(f, f.area(), theme);
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
fn footer(app: &App, width: u16) -> Span<'static> {
    let theme = app.theme();
    let columns = area_text_width(width);
    match app.flash_message() {
        Some(message) => Span::styled(
            format!(" {}", truncate(message, columns)),
            Style::default().fg(theme.notice()),
        ),
        None => Span::styled(
            format!(" {}", hint_strip(columns)),
            Style::default().fg(theme.muted()),
        ),
    }
}

/// Build the hint strip for a width: as many hints as fit, whole, never a hint
/// cut in half. The overlay and quit hints are kept whatever the width, since
/// they are how a reader finds every other binding and how they leave.
fn hint_strip(width: usize) -> String {
    let essential = keymap::FOOTER_ESSENTIAL.join(keymap::HINT_SEPARATOR);
    let mut shown: Vec<&str> = Vec::new();
    for hint in keymap::FOOTER_HINTS {
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
    shown.extend_from_slice(keymap::FOOTER_ESSENTIAL);
    let strip = shown.join(keymap::HINT_SEPARATOR);
    // Narrower than even the essential hints: clip, since something is better
    // than a blank strip.
    strip.chars().take(width).collect()
}

fn draw_breadcrumb(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let crumbs = app.nav().crumbs();
    let text = elide_left(&crumbs, area.width.saturating_sub(1) as usize);
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.muted()))
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
                index == app.nav().cursor(),
            )));
        }
        items
    };

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .add_modifier(Modifier::REVERSED)
            .add_modifier(Modifier::BOLD),
    );
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
fn row_line<'a>(
    row: &'a Row,
    theme: Theme,
    label_width: usize,
    count_width: usize,
    pane_width: usize,
    selected: bool,
) -> Line<'a> {
    let muted = Style::default().fg(theme.muted());
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
    };
    let (id_style, count_style, name_style) = if selected {
        (plain, plain, plain)
    } else {
        (id_style, muted, name_style)
    };
    let prefix_width = 2 + label_width + 1 + count_width + 2;
    let name_budget = pane_width.saturating_sub(prefix_width);
    Line::from(vec![
        Span::styled(glyph_cell, id_style),
        Span::raw(" "),
        Span::styled(format!("{:<label_width$}", row.label), id_style),
        Span::raw(" "),
        Span::styled(format!("{:<count_width$}", count_cell(row)), count_style),
        Span::raw("  "),
        Span::styled(truncate(&row.name, name_budget), name_style),
    ])
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
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
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
            let line = row_line(&row, theme, 2, 3, 40, false);
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
    fn the_overlay_is_wide_enough_for_every_binding_it_lists() {
        let width = help_width(200) as usize;
        for (keys, what) in keymap::HELP {
            let row = help_key_width() + 2 + what.chars().count();
            assert!(
                row + 2 <= width,
                "{keys:?} / {what:?} needs {row} columns inside a {width}-wide overlay"
            );
        }
    }

    #[test]
    fn the_hint_strip_never_clips_a_hint_in_half() {
        for width in 10..120usize {
            let strip = hint_strip(width);
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

    #[test]
    fn the_hint_strip_always_says_how_to_get_help_and_out() {
        for width in 40..120usize {
            let strip = hint_strip(width);
            assert!(strip.contains("? keys"), "width {width}: {strip:?}");
            assert!(strip.contains("q quit"), "width {width}: {strip:?}");
        }
    }

    #[test]
    fn a_wide_strip_carries_every_hint() {
        let strip = hint_strip(200);
        for hint in keymap::FOOTER_HINTS {
            assert!(strip.contains(hint), "{strip:?} is missing {hint:?}");
        }
    }

    #[test]
    fn a_clipped_name_is_marked() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a longer name", 6), "a lon…");
        assert_eq!(truncate("anything", 0), "");
    }
}
