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
use crate::data::{Row, Selection};
use crate::keymap;
use crate::theme::{glyph, Theme};

/// The separator between breadcrumb entries.
const CRUMB_SEPARATOR: &str = " › ";

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
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {text}"),
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Left),
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
        rows.iter()
            .enumerate()
            .map(|(index, row)| {
                ListItem::new(row_line(
                    row,
                    theme,
                    label_width,
                    count_width,
                    inner.width as usize,
                    index == app.nav().cursor(),
                ))
            })
            .collect()
    };

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .add_modifier(Modifier::REVERSED)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(app.nav().cursor()));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// The child-count cell: how many direct children the row has, or nothing for a
/// leaf. An absent count is the signal that entering the row would do nothing.
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
    let status_color = match &row.selection {
        Selection::Epic(_) => theme.epic_status(&row.status),
        Selection::Node(_) => theme.node_status(&row.status),
    };
    let status_style = if selected {
        Style::default()
    } else {
        Style::default().fg(status_color)
    };
    let muted_style = if selected {
        Style::default()
    } else {
        Style::default().fg(theme.muted())
    };
    let prefix_width = 2 + label_width + 1 + count_width + 2;
    let name_budget = pane_width.saturating_sub(prefix_width);
    Line::from(vec![
        Span::styled(glyph(&row.status), status_style),
        Span::raw(" "),
        Span::styled(format!("{:<label_width$}", row.label), status_style),
        Span::raw(" "),
        Span::styled(format!("{:<count_width$}", count_cell(row)), muted_style),
        Span::raw("  "),
        Span::raw(truncate(&row.name, name_budget)),
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
    fn a_leaf_has_no_count_cell_so_the_absence_marks_it() {
        let leaf = Row {
            selection: Selection::Epic("e".into()),
            label: "e".into(),
            name: "n".into(),
            status: "open".into(),
            children: 0,
        };
        let parent = Row {
            children: 3,
            ..leaf.clone()
        };
        assert_eq!(count_cell(&leaf), "");
        assert_eq!(count_cell(&parent), "(3)");
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
