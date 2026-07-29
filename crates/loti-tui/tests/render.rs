//! Rendering smoke tests against a real store, on a headless backend.
//!
//! These assert the frame's *structure* — the breadcrumb line, the panes, the
//! hint strip — and deliberately not the markdown body: the preview's inner
//! layout belongs to the rendering library, so pinning it here would turn every
//! upstream release into a test failure without telling us anything about loti.

use loti_core::domain::NodeRef;
use loti_core::ops::{self, NewEpic, NewNode};
use loti_core::store::{self, Store};
use loti_tui::action::Action;
use loti_tui::app::App;
use loti_tui::theme::Theme;
use loti_tui::ui;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

/// A store with one epic, a ticket with a subticket, and a childless ticket.
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
    (dir, store)
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
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
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
