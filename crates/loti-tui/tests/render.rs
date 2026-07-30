//! Rendering smoke tests against a real store, on a headless backend.
//!
//! These assert the frame's *structure* — the breadcrumb line, the panes, the
//! hint strip — and deliberately not the markdown body: the preview's inner
//! layout belongs to the rendering library, so pinning it here would turn every
//! upstream release into a test failure without telling us anything about loti.

use loti_core::domain::NodeRef;
use loti_core::ops::{self, NewEpic, NewNode, Target};
use loti_core::store::{self, Store};
use loti_core::Actor;
use loti_tui::action::Action;
use loti_tui::app::App;
use loti_tui::data::RowKind;
use loti_tui::theme::Theme;
use loti_tui::ui;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

/// A store with one epic carrying meta, a ticket with a subticket, and a
/// childless ticket.
fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".loti");
    store::init(dir.path(), &root).unwrap();
    let store = Store::at(&root);
    ops::create_epic(
        &store,
        NewEpic {
            epic_id: "browser".into(),
            name: "The browser".into(),
            summary: "A full-screen view".into(),
            labels: vec![],
            body: "Some **body** text.\n".into(),
        },
    )
    .unwrap();
    let parent = ops::create_node(
        &store,
        NewNode {
            epic_id: "browser".into(),
            parent: None,
            name: "Navigation pane".into(),
            summary: "s".into(),
            labels: vec![],
            body: String::new(),
        },
    )
    .unwrap();
    ops::create_node(
        &store,
        NewNode {
            epic_id: "browser".into(),
            parent: Some(NodeRef {
                epic_id: "browser".into(),
                number: parent.frontmatter.number,
            }),
            name: "Row rendering".into(),
            summary: "s".into(),
            labels: vec![],
            body: String::new(),
        },
    )
    .unwrap();
    ops::create_node(
        &store,
        NewNode {
            epic_id: "browser".into(),
            parent: None,
            name: "Preview pane".into(),
            summary: "s".into(),
            labels: vec![],
            body: String::new(),
        },
    )
    .unwrap();
    // Meta on the epic, so a drawn frame has a populated collection row to enter
    // as well as empty ones.
    let epic = Target::Epic("browser".into());
    ops::add_labels(&store, &epic, &["ui".to_string()]).unwrap();
    ops::add_comment(&store, &epic, Actor::Human, "a remark\n".to_string()).unwrap();
    (dir, store)
}

/// Put the cursor on the first work row of the level on screen. Every epic and
/// node level leads with its collection rows.
fn to_work_row(app: &mut App) {
    let index = app
        .nav()
        .rows()
        .iter()
        .position(|r| matches!(r.kind, RowKind::Work(_)))
        .expect("the level has a work row");
    app.apply(Action::CursorFirst).unwrap();
    for _ in 0..index {
        app.apply(Action::CursorDown).unwrap();
    }
}

/// The frame's lines as plain strings, top to bottom.
fn frame_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn draw(app: &mut App) -> (Terminal<TestBackend>, Vec<String>) {
    draw_at(app, 100, 24)
}

fn draw_at(app: &mut App, width: u16, height: u16) -> (Terminal<TestBackend>, Vec<String>) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    let lines = frame_lines(&terminal);
    (terminal, lines)
}

#[test]
fn the_first_frame_carries_the_breadcrumb_the_panes_and_the_hints() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    let (_t, lines) = draw(&mut app);

    // The breadcrumb owns the top line and reads as the root level.
    assert_eq!(lines[0].trim(), "epics");
    // The navigation pane is titled and lists the epic with its ticket count.
    assert!(lines[1].contains("navigation"), "{:?}", lines[1]);
    let epic_row = lines.iter().find(|l| l.contains("The browser")).unwrap();
    assert!(epic_row.contains("browser"), "{epic_row:?}");
    assert!(
        epic_row.contains("(2)"),
        "expected the top-level ticket count: {epic_row:?}"
    );
    // The preview is titled with the reference it shows.
    assert!(
        lines[1].contains("browser"),
        "expected the preview title on the border line: {:?}",
        lines[1]
    );
    // The hint strip owns the bottom line.
    assert!(lines[23].contains("q quit"), "{:?}", lines[23]);
}

#[test]
fn descending_updates_the_breadcrumb_and_lists_the_level_below() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    app.apply(Action::Descend).unwrap();
    let (_t, lines) = draw(&mut app);

    assert_eq!(lines[0].trim(), "epics › browser");
    assert!(lines.iter().any(|l| l.contains("Navigation pane")));
    assert!(lines.iter().any(|l| l.contains("Preview pane")));
    // The ticket that has a subticket shows a count; the one that has none does
    // not, which is what marks it as not enterable.
    let with_child = lines
        .iter()
        .find(|l| l.contains("Navigation pane"))
        .unwrap();
    let without = lines.iter().find(|l| l.contains("Preview pane")).unwrap();
    assert!(with_child.contains("(1)"), "{with_child:?}");
    assert!(!without.contains('('), "{without:?}");
}

#[test]
fn zoom_replaces_the_navigation_pane_but_keeps_the_breadcrumb() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    app.apply(Action::Descend).unwrap();
    to_work_row(&mut app);
    app.apply(Action::ToggleZoom).unwrap();
    let (_t, lines) = draw(&mut app);

    assert_eq!(lines[0].trim(), "epics › browser");
    assert!(
        !lines.iter().any(|l| l.contains("navigation")),
        "the navigation pane should be gone while zoomed"
    );
    assert!(lines.iter().any(|l| l.contains("browser/1")));
}

#[test]
fn a_level_leads_with_its_collections_and_a_rule_before_the_work() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    app.apply(Action::Descend).unwrap();
    let width = nav_pane_width(&app);
    let (terminal, _) = draw(&mut app);
    // Bounded to the navigation pane's rows: the pane's own border is drawn with
    // the same character the rule is, and the preview shares every line.
    let all = nav_lines(&terminal, width);
    let lines = &all[2..];

    let at = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line containing {needle:?} in {lines:#?}"))
    };
    // An epic has no dependency list, so it carries three collections.
    assert!(at("labels") < at("comments"));
    assert!(at("comments") < at("assets"));
    // One rule separates structure from work, and the work is below it.
    assert!(at("assets") < at("\u{2500}\u{2500}\u{2500}"));
    assert!(at("\u{2500}\u{2500}\u{2500}") < at("Navigation pane"));

    // A count is printed when the collection has members and blank when it has
    // none — the same contract a child count follows.
    let row = |needle: &str| lines[at(needle)].clone();
    assert!(row("labels").contains("(1)"), "{:?}", row("labels"));
    assert!(!row("assets").contains('('), "{:?}", row("assets"));
}

#[test]
fn a_collection_row_has_no_glyph_so_a_glyph_means_work() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    app.apply(Action::Descend).unwrap();
    let width = nav_pane_width(&app);
    let (terminal, _) = draw(&mut app);

    // A collection has no state, so inventing a glyph for it would claim one.
    let comments = row_cells(&terminal, "comments", width);
    assert_eq!(comments[0].0, " ");
    let work = row_cells(&terminal, "Navigation pane", width);
    assert_ne!(work[0].0, " ");
}

#[test]
fn entering_a_collection_names_it_in_the_breadcrumb_and_dims_the_crumb() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(true)).unwrap();
    app.apply(Action::Descend).unwrap(); // into the epic
    app.apply(Action::CursorDown).unwrap(); // onto `comments`
    app.apply(Action::Descend).unwrap();
    let width = nav_pane_width(&app);
    let (terminal, lines) = draw(&mut app);

    assert_eq!(lines[0].trim(), "epics › browser › comments");
    // Bounded to the navigation pane: the author is in the preview pane's own
    // metadata table on the same terminal lines, so a whole-line search would
    // pass with the row empty.
    let rows = nav_lines(&terminal, width);
    assert!(
        rows.iter().any(|l| l.contains("human")),
        "expected the comment's author on its row: {rows:#?}"
    );

    // The deepest crumb is dim, like the row it was entered from; the path above
    // it keeps the accent.
    let buffer = terminal.backend().buffer();
    let crumb = lines[0].find("comments").unwrap() as u16;
    let path = lines[0].find("browser").unwrap() as u16;
    assert_ne!(
        buffer[(crumb, 0)].style().fg,
        buffer[(path, 0)].style().fg,
        "a collection crumb must not read as part of the work path"
    );
}

#[test]
fn the_help_overlay_lists_the_bindings() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    app.apply(Action::ToggleHelp).unwrap();
    let (_t, lines) = draw(&mut app);

    assert!(lines.iter().any(|l| l.contains("keys")));
    assert!(lines.iter().any(|l| l.contains("move the cursor")));
}

#[test]
fn an_empty_store_is_browsable_rather_than_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".loti");
    store::init(dir.path(), &root).unwrap();
    let mut app = App::new(Store::at(&root), Theme::with_color(false)).unwrap();
    let (_t, lines) = draw(&mut app);

    assert_eq!(lines[0].trim(), "epics");
    assert!(lines.iter().any(|l| l.contains("(no epics)")));
    // Nothing to enter, and trying must not panic or change the level.
    app.apply(Action::Descend).unwrap();
    assert_eq!(app.nav().crumbs(), vec!["epics"]);
}

#[test]
fn a_flash_replaces_the_hint_strip_and_moves_nothing_else() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    let (_t, hinted) = draw(&mut app);
    app.flash("nothing to open here");
    let (_t2, flashed) = draw(&mut app);

    assert!(
        flashed[23].contains("nothing to open here"),
        "{:?}",
        flashed[23]
    );
    // The whole strip goes, essential hints included: that cost is deliberate.
    assert!(!flashed[23].contains("q quit"), "{:?}", flashed[23]);
    // The layout invariant: breadcrumb and strip stay one line each, and nothing
    // above the strip reflows.
    assert_eq!(hinted[..23], flashed[..23]);
}

#[test]
fn a_flash_too_wide_for_the_line_is_clipped_rather_than_wrapped() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    app.flash("a notice ".repeat(40));
    let (_t, lines) = draw(&mut app);

    assert!(lines[23].chars().count() <= 100, "{:?}", lines[23]);
    assert!(lines[23].ends_with('…'), "{:?}", lines[23]);
    // The line above is still the panes' bottom border, not spilled notice text.
    assert!(!lines[22].contains("a notice"), "{:?}", lines[22]);
}

#[test]
fn a_flash_is_not_painted_like_the_hints_it_covers() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(true)).unwrap();
    let (hinted, _) = draw(&mut app);
    let hint_fg = hinted.backend().buffer()[(1, 23)].style().fg;
    app.flash("a notice");
    let (flashed, _) = draw(&mut app);
    let flash_fg = flashed.backend().buffer()[(1, 23)].style().fg;

    assert_ne!(
        flash_fg, hint_fg,
        "a notice in the hint colour reads as a binding"
    );
}

#[test]
fn editing_mode_names_itself_on_the_breadcrumb_line_and_stops_when_it_is_left() {
    let (_dir, store) = fixture();
    // Colour disabled: the mode has to be readable from the words alone.
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    app.apply(Action::EnterEditing).unwrap();
    let (terminal, lines) = draw(&mut app);

    // The marker holds the right-hand end of the line and the breadcrumb keeps
    // the left, so the two never compete for the same columns.
    assert!(lines[0].contains("EDITING"), "{:?}", lines[0]);
    assert!(
        lines[0].ends_with("EDITING \u{2500}\u{2500}"),
        "{:?}",
        lines[0]
    );
    assert!(lines[0].starts_with(" epics"), "{:?}", lines[0]);
    // Pinned from the raw buffer, because the lines above are trimmed: a centred
    // marker would end with the same characters and read as right-aligned here.
    let buffer = terminal.backend().buffer();
    let last_painted = (0..buffer.area.width)
        .filter(|x| buffer[(*x, 0)].symbol() != " ")
        .max()
        .expect("the line is not blank");
    assert_eq!(
        last_painted,
        buffer.area.width - 1,
        "the marker has to reach the right-hand end of the line"
    );

    app.apply(Action::Unwind).unwrap();
    let (_t, browsing) = draw(&mut app);
    assert!(!browsing[0].contains("EDITING"), "{:?}", browsing[0]);
}

#[test]
fn a_narrow_line_elides_the_breadcrumb_rather_than_the_mode_it_is_in() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    app.apply(Action::Descend).unwrap(); // into the epic
    to_work_row(&mut app);
    app.apply(Action::Descend).unwrap(); // into the ticket
    app.apply(Action::EnterEditing).unwrap();
    let (_t, lines) = draw_at(&mut app, 40, 12);

    // A fact about the whole session must not be what a narrow terminal drops:
    // the path is elided from the left instead, keeping the level you are in.
    assert!(
        lines[0].contains("\u{2500}\u{2500} EDITING \u{2500}\u{2500}"),
        "{:?}",
        lines[0]
    );
    assert!(lines[0].contains('\u{2026}'), "{:?}", lines[0]);
    assert!(lines[0].chars().count() <= 40, "{:?}", lines[0]);
}

#[test]
fn the_frozen_row_is_barred_and_at_contrast_while_every_other_row_dims() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(true)).unwrap();
    app.apply(Action::Descend).unwrap(); // into the epic
    to_work_row(&mut app);
    app.apply(Action::EnterEditing).unwrap();
    let width = nav_pane_width(&app);
    let (terminal, _) = draw(&mut app);

    // The frozen row: a gutter bar, no dimming, and none of the inversion a
    // browsed level uses — the pane has stopped being a list being browsed.
    let target = row_cells(&terminal, "Navigation pane", width);
    assert_eq!(target[0].0, "\u{258c}");
    assert!(target.iter().all(|(_, style)| {
        !style.add_modifier.contains(Modifier::DIM)
            && !style.add_modifier.contains(Modifier::REVERSED)
    }));

    // Every other row is disabled: the selection cannot move to it.
    for other in ["Preview pane", "labels", "comments"] {
        let cells = row_cells(&terminal, other, width);
        assert_ne!(cells[0].0, "\u{258c}", "{other} took the bar");
        assert!(
            cells
                .iter()
                .any(|(_, style)| style.add_modifier.contains(Modifier::DIM)),
            "{other} does not read as disabled"
        );
    }
}

#[test]
fn the_navigation_pane_is_framed_in_the_modes_own_colour_while_it_is_on() {
    let (_dir, store) = fixture();
    let theme = Theme::with_color(true);
    let mut app = App::new(store, theme).unwrap();
    let (browsing, _) = draw(&mut app);
    // The pane's own top-left corner, on the first line of the body.
    let corner = |terminal: &Terminal<TestBackend>| terminal.backend().buffer()[(0, 1)].style().fg;
    assert_eq!(corner(&browsing), Some(theme.muted()));

    app.apply(Action::EnterEditing).unwrap();
    let (editing, _) = draw(&mut app);
    // The same colour the indicator is painted in, so the marker and the framed
    // pane read as one fact rather than two.
    assert_eq!(corner(&editing), Some(theme.notice()));
}

#[test]
fn the_strip_offers_the_modes_own_keys_and_not_the_levels() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    app.apply(Action::EnterEditing).unwrap();
    let (_t, lines) = draw(&mut app);

    // The way out of the mode, first, and help beside it. Neither browse hint
    // applies: `q` does not quit while the mode is on.
    assert!(lines[23].contains("Esc leave"), "{:?}", lines[23]);
    assert!(lines[23].contains("? keys"), "{:?}", lines[23]);
    assert!(!lines[23].contains("q quit"), "{:?}", lines[23]);
    assert!(!lines[23].contains("j/k move"), "{:?}", lines[23]);
}

#[test]
fn a_key_the_mode_ignores_says_how_to_leave_it_on_the_strips_line() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    app.apply(Action::EnterEditing).unwrap();
    app.apply(Action::CursorDown).unwrap();
    let (_t, lines) = draw(&mut app);

    // A notice covers the strip, so it has to carry the way out itself. Matched
    // on the notice's own wording: the essential hint the strip would show
    // instead says `Esc` too, so `Esc` alone would pass with no notice at all.
    assert!(
        lines[23].contains("not an editing action"),
        "{:?}",
        lines[23]
    );
    assert!(lines[23].contains("Esc"), "{:?}", lines[23]);
    // And the mode is still on: an ignored key is not an implicit exit.
    assert!(lines[0].contains("EDITING"), "{:?}", lines[0]);
}

/// A store whose epics cover every epic state: open, completed and closed.
fn every_epic_state() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".loti");
    store::init(dir.path(), &root).unwrap();
    let store = Store::at(&root);
    for id in ["an-open", "a-done", "a-shut"] {
        ops::create_epic(
            &store,
            NewEpic {
                epic_id: id.into(),
                name: format!("epic {id}"),
                summary: "s".into(),
                labels: vec![],
                body: String::new(),
            },
        )
        .unwrap();
    }
    // An epic is `completed` only once every one of its nodes is terminal.
    ops::create_node(
        &store,
        NewNode {
            epic_id: "a-done".into(),
            parent: None,
            name: "Only ticket".into(),
            summary: "s".into(),
            labels: vec![],
            body: String::new(),
        },
    )
    .unwrap();
    ops::set_node_status(
        &store,
        &NodeRef {
            epic_id: "a-done".into(),
            number: 1,
        },
        ops::NodeStatusChange::Done,
    )
    .unwrap();
    ops::set_epic_closed(&store, "a-shut", true, Some("superseded".into())).unwrap();
    (dir, store)
}

/// The width of the navigation pane in the 100-column test frame.
fn nav_pane_width(app: &App) -> u16 {
    100 * app.nav_percent() / 100
}

/// The navigation pane's interior, line by line: the rows the cursor moves over,
/// without the border or the preview pane's text from the same terminal line.
fn nav_lines(terminal: &Terminal<TestBackend>, nav_width: u16) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (1..nav_width - 1)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// The styled cells of the navigation row containing `needle`.
///
/// The search is bounded to the navigation pane: the same identifier appears in
/// the preview pane's title and body, and matching there would compare the wrong
/// cells.
fn row_cells(
    terminal: &Terminal<TestBackend>,
    needle: &str,
    nav_width: u16,
) -> Vec<(String, ratatui::style::Style)> {
    let buffer = terminal.backend().buffer();
    let inner = 1..nav_width - 1;
    let y = (0..buffer.area.height)
        .find(|y| {
            inner
                .clone()
                .map(|x| buffer[(x, *y)].symbol())
                .collect::<String>()
                .contains(needle)
        })
        .unwrap_or_else(|| panic!("no navigation row containing {needle:?}"));
    inner
        .map(|x| {
            let cell = &buffer[(x, y)];
            (cell.symbol().to_string(), cell.style())
        })
        .collect()
}

#[test]
fn a_glyph_and_its_colour_agree_for_every_epic_state() {
    let (_dir, store) = every_epic_state();
    let mut app = App::new(store, Theme::with_color(true)).unwrap();
    // Keep the cursor off the rows under test: a highlighted row is drawn
    // uncoloured on purpose.
    // Epics sort by id, so the cursor lands on `an-open`; the two rows under test
    // must not be the highlighted one, which is drawn uncoloured on purpose.
    app.apply(Action::CursorLast).unwrap();
    let width = nav_pane_width(&app);
    let (terminal, _) = draw(&mut app);

    // `completed` is a resolved state, so it takes the resolved glyph and the
    // resolved colour — a green circle would say "finished" and "not started"
    // at once.
    let done = row_cells(&terminal, "a-done", width);
    assert_eq!(done[0].0, "✓");
    assert_eq!(done[0].1.fg, Some(Color::Green));

    // A closed epic is abandoned, not pending: dim cross, not a plain circle.
    let shut = row_cells(&terminal, "a-shut", width);
    assert_eq!(shut[0].0, "✗");
    assert_eq!(shut[0].1.fg, Some(Color::DarkGray));
}

#[test]
fn the_identifier_is_readable_and_only_the_count_is_muted() {
    let (_dir, store) = every_epic_state();
    let mut app = App::new(store, Theme::with_color(true)).unwrap();
    app.apply(Action::CursorLast).unwrap();
    let width = nav_pane_width(&app);
    let (terminal, _) = draw(&mut app);

    let cells = row_cells(&terminal, "a-done", width);
    // The identifier is what a reader types into a command, so it carries the
    // state's colour at full contrast rather than being dimmed.
    let id_cell = cells.iter().find(|(s, _)| s == "a").unwrap();
    assert_eq!(id_cell.1.fg, Some(Color::Green));
    // The child count is the one muted column.
    let count_cell = cells.iter().find(|(s, _)| s == "(").unwrap();
    assert_eq!(count_cell.1.fg, Some(Color::DarkGray));
}

#[test]
fn the_highlighted_row_is_one_uniform_bar() {
    let (_dir, store) = every_epic_state();
    let mut app = App::new(store, Theme::with_color(true)).unwrap();
    let width = nav_pane_width(&app);
    let (terminal, _) = draw(&mut app);

    // The cursor starts on the first row, which is `a-done`.
    let cells = row_cells(&terminal, "a-done", width);
    for (symbol, style) in &cells {
        assert!(
            style.add_modifier.contains(Modifier::REVERSED),
            "cell {symbol:?} is not part of the highlight bar"
        );
        // Inverting a cell that also carries its own foreground is what breaks
        // the bar into mismatched blocks; the terminal default reads back as
        // `Reset`, which is what "no colour of its own" looks like in a buffer.
        assert_eq!(
            style.fg,
            Some(Color::Reset),
            "cell {symbol:?} keeps a foreground under the highlight"
        );
    }
}

#[test]
fn resizing_moves_the_divider_in_the_drawn_frame() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    let (_t, before) = draw(&mut app);
    let wide_at = before[1].find("navigation").unwrap();
    for _ in 0..6 {
        app.apply(Action::GrowNav).unwrap();
    }
    let (_t2, after) = draw(&mut app);
    // The navigation pane's border box grew, so the preview title moved right.
    let preview_before = before[1].rfind("browser").unwrap();
    let preview_after = after[1].rfind("browser").unwrap();
    assert!(
        preview_after > preview_before,
        "expected the divider to move right: {preview_before} -> {preview_after}"
    );
    assert_eq!(after[1].find("navigation").unwrap(), wide_at);
}
