//! Rendering smoke tests against a real store, on a headless backend.
//!
//! These assert the frame's *structure* — the breadcrumb line, the panes, the
//! hint strip — and deliberately not the markdown body: the preview's inner
//! layout belongs to the rendering library, so pinning it here would turn every
//! upstream release into a test failure without telling us anything about loti.

use loti_core::domain::NodeRef;
use loti_core::meta::{self, Meta};
use loti_core::ops::{self, NewEpic, NewNode, Target};
use loti_core::store::{self, Store};
use loti_core::Actor;
use loti_tui::action::{Action, Answers};
use loti_tui::app::{App, Modal};
use loti_tui::data::{ReadOnly, RowKind};
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
    // Every binding the list carries reaches the screen: a list one row too tall
    // for the terminal teaches nothing about the binding it drops.
    for (keys, what) in loti_tui::keymap::HELP {
        assert!(
            lines.iter().any(|l| l.contains(what)),
            "{keys:?} / {what:?} is listed but not on the frame"
        );
    }
    // The keys that move between a surface's fields among them.
    assert!(
        lines.iter().any(|l| l.contains("Tab / Shift-Tab")),
        "the field keys are not listed: {lines:#?}"
    );
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

/// Put the store into the read-only state `state` names, by recording the format
/// version that reaches it: an unmigrated store, a migration in flight, a format
/// newer than this binary, or a version nothing can parse. Returns whether this
/// binary's own version can express it — there is no major below the first one,
/// so an unmigrated store is out of reach for a binary at major zero.
///
/// Written to the store's metadata rather than reached by running a migration,
/// because the states the browser has to show are the ones a store is *left* in:
/// a migration that commits leaves nothing to see.
fn turn_read_only(store: &Store, state: ReadOnly) -> bool {
    let (major, minor) = loti_core::FORMAT_VERSION;
    let recorded = match state {
        ReadOnly::NeedsMigration => match major.checked_sub(1) {
            Some(older) => Meta::clean(older, minor),
            None => return false,
        },
        ReadOnly::MigrationInProgress => Meta::migrating(major, minor),
        ReadOnly::NeedsNewerLoti => Meta::clean(major + 1, minor),
        ReadOnly::VersionUnreadable => Meta {
            format_version: "not-a-version".to_string(),
        },
    };
    meta::write(store.root(), &recorded).unwrap();
    true
}

/// The breadcrumb line split into the path and the state slot's marker, or `None`
/// for a line with no marker on it.
///
/// The marker's own decoration is what tells the two apart: no crumb carries it,
/// and the marker holds the right-hand end of the line.
fn crumbs_and_marker(line: &str) -> (String, Option<String>) {
    match line.find("\u{2500}\u{2500}") {
        Some(at) => (
            line[..at].trim_end().to_string(),
            Some(line[at..].to_string()),
        ),
        None => (line.trim_end().to_string(), None),
    }
}

/// The column the last painted cell of the top line sits in. Read from the raw
/// buffer, because the lines a test reads are trimmed: a centred marker would end
/// with the same characters and read as right-aligned.
fn last_painted_column(terminal: &Terminal<TestBackend>) -> u16 {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.width)
        .filter(|x| buffer[(*x, 0)].symbol() != " ")
        .max()
        .expect("the line is not blank")
}

#[test]
fn a_store_that_may_not_be_written_names_the_state_in_words_in_the_slot() {
    let (_dir, store) = fixture();
    assert!(turn_read_only(&store, ReadOnly::MigrationInProgress));
    // Colour disabled: the state has to be readable from the words alone, since a
    // state only a hue announced would be invisible to a reader who turned colour
    // off.
    let mut app = App::new(store.clone(), Theme::with_color(false)).unwrap();
    let (terminal, lines) = draw(&mut app);

    let (crumbs, marker) = crumbs_and_marker(&lines[0]);
    let marker = marker.expect("the state slot carries a marker");
    assert!(marker.contains("READ-ONLY"), "{marker:?}");
    // The reason as well as the state, so the remedy is discoverable: this one is
    // somebody else's migration and clears on its own.
    assert!(marker.contains("MIGRATION IN PROGRESS"), "{marker:?}");
    // The path keeps the left-hand end and the marker the right, so the two never
    // compete for the same columns.
    assert_eq!(crumbs.trim(), "epics");
    assert_eq!(
        last_painted_column(&terminal),
        terminal.backend().buffer().area.width - 1,
        "the marker has to reach the right-hand end of the line"
    );

    // And the very same line with colour on: what says the store may not be
    // written is the word, and the colour is decoration over it.
    let mut coloured = App::new(store, Theme::with_color(true)).unwrap();
    let (_t, in_colour) = draw(&mut coloured);
    assert_eq!(in_colour[0], lines[0]);
}

#[test]
fn every_read_only_state_names_its_own_reason_in_the_slot() {
    let (_dir, store) = fixture();
    let mut markers: Vec<String> = Vec::new();
    for state in ReadOnly::ALL.iter().copied() {
        if !turn_read_only(&store, state) {
            continue;
        }
        let mut app = App::new(store.clone(), Theme::with_color(false)).unwrap();
        assert_eq!(app.read_only(), Some(state));
        let (_t, lines) = draw(&mut app);
        let marker = crumbs_and_marker(&lines[0])
            .1
            .unwrap_or_else(|| panic!("{state:?} drew no marker: {:?}", lines[0]));
        assert!(marker.contains("READ-ONLY"), "{state:?}: {marker:?}");
        markers.push(marker);
    }
    // Each state's own words, because the remedy differs by state: one is the
    // reader's to run, one is somebody else's to finish, and one needs a newer
    // loti. A marker two states shared would name the wrong remedy on one of them.
    let distinct = {
        let mut seen = markers.clone();
        seen.sort();
        seen.dedup();
        seen
    };
    assert_eq!(distinct.len(), markers.len(), "{markers:#?}");
}

#[test]
fn the_state_slot_is_as_wide_as_the_widest_marker_the_session_could_show() {
    // Deep enough, and narrow enough, that the path is elided: the width the slot
    // takes is only observable in what the path has left.
    let elided = |state: ReadOnly, store: &Store| -> (String, String) {
        let mut app = App::new(store.clone(), Theme::with_color(false)).unwrap();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        app.apply(Action::Descend).unwrap(); // into the ticket
        let (terminal, lines) = draw_at(&mut app, 60, 16);
        let (crumbs, marker) = crumbs_and_marker(&lines[0]);
        assert_eq!(
            last_painted_column(&terminal),
            59,
            "{state:?} did not reach the right-hand end of the line"
        );
        (crumbs, marker.expect("the state slot carries a marker"))
    };

    let (_dir, store) = fixture();
    assert!(turn_read_only(&store, ReadOnly::MigrationInProgress));
    let (widest_crumbs, widest) = elided(ReadOnly::MigrationInProgress, &store);
    assert!(turn_read_only(&store, ReadOnly::NeedsNewerLoti));
    let (crumbs, shorter) = elided(ReadOnly::NeedsNewerLoti, &store);

    // Two markers of different widths, and the path they leave room for is the
    // same one: the slot is sized for the widest marker the session could show
    // rather than for the one in it, so a reason that changes under a reload does
    // not shift the path the reader is reading.
    assert!(
        shorter.chars().count() < widest.chars().count(),
        "{shorter:?} / {widest:?}"
    );
    assert!(!crumbs.trim().is_empty(), "{crumbs:?}");
    assert_eq!(crumbs, widest_crumbs);
}

#[test]
fn the_marker_arrives_on_the_reload_that_finds_it_and_outlives_the_notice() {
    let (_dir, store) = fixture();
    let mut app = App::new(store.clone(), Theme::with_color(false)).unwrap();
    app.apply(Action::EnterEditing).unwrap();
    let (_t, editing) = draw(&mut app);
    assert!(editing[0].contains("EDITING"), "{:?}", editing[0]);

    // An agent can begin migrating the store while the browser is open, so the
    // question is asked again on every reload and not settled at startup.
    assert!(turn_read_only(&store, ReadOnly::MigrationInProgress));
    app.apply(Action::Reload).unwrap();
    let (_t, arrived) = draw(&mut app);
    // The mode's marker goes as the store's arrives: the slot holds one of them,
    // and the mode is not available under the other.
    assert!(arrived[0].contains("READ-ONLY"), "{:?}", arrived[0]);
    assert!(!arrived[0].contains("EDITING"), "{:?}", arrived[0]);
    // The transition is reported once, on the strip's line, where everything
    // transient is reported.
    assert!(arrived[23].contains("editing stopped"), "{:?}", arrived[23]);

    // The notice is transient and the condition is not: any keypress retires the
    // notice, and the slot goes on saying the store may not be written.
    app.clear_flash();
    let (_t, later) = draw(&mut app);
    assert!(later[0].contains("READ-ONLY"), "{:?}", later[0]);
    assert!(
        later[23].contains("q quit"),
        "the strip is back: {:?}",
        later[23]
    );
}

#[test]
fn the_editing_key_on_a_read_only_store_says_the_stores_own_words_on_the_strip() {
    let (_dir, store) = fixture();
    assert!(turn_read_only(&store, ReadOnly::MigrationInProgress));
    let mut app = App::new(store.clone(), Theme::with_color(false)).unwrap();
    app.apply(Action::EnterEditing).unwrap();
    let (_t, lines) = draw(&mut app);

    // Verbatim: the words the store refuses a write with, taken from the store
    // itself rather than spelled out here — the remedy is a store rule, and a
    // browser paraphrase of one goes stale.
    let refusal = store
        .verify_mutable()
        .expect_err("a mid-migration store refuses every write")
        .to_string();
    assert!(lines[23].contains(&refusal), "{:?}", lines[23]);
    // And the mode was not entered: its marker is nowhere, and the store's still
    // holds the slot.
    assert!(!lines[0].contains("EDITING"), "{:?}", lines[0]);
    assert!(lines[0].contains("READ-ONLY"), "{:?}", lines[0]);
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

/// Stand on the first label of the epic's own labels level, which is where a
/// removal is offered. An epic level leads with its collection rows, so `labels`
/// is the row the cursor lands on.
fn to_a_label_row(app: &mut App) {
    to_the_labels_row(app);
    app.apply(Action::Descend).unwrap();
}

/// Stand on the epic's own `labels` row, which is where an addition is offered:
/// creation acts on the container row the cursor stands on.
fn to_the_labels_row(app: &mut App) {
    app.apply(Action::Descend).unwrap(); // into the epic
    app.apply(Action::CursorFirst).unwrap();
}

/// Open the label surface the way a reader does, and type into it.
fn open_the_label_surface(app: &mut App, text: &str) {
    to_the_labels_row(app);
    app.apply(Action::EnterEditing).unwrap();
    app.apply(Action::Add).unwrap();
    for c in text.chars() {
        app.apply(Action::Insert(c)).unwrap();
    }
}

/// Stand on the epic's own `assets` row, which is the row an addition would be
/// offered on if the browser attached payloads at all.
fn to_the_assets_row(app: &mut App) {
    app.apply(Action::Descend).unwrap(); // into the epic
    let index = app
        .nav()
        .rows()
        .iter()
        .position(|r| r.name == "assets")
        .expect("every container carries an assets row");
    app.apply(Action::CursorFirst).unwrap();
    for _ in 0..index {
        app.apply(Action::CursorDown).unwrap();
    }
}

/// Stand on a ticket's own `blocked-by` row, which is where a blocker is added.
/// A dependency list belongs to a node — an epic is not a unit of work that can
/// be blocked — so it is a level deeper than the epic's own collections.
fn to_the_blocked_by_row(app: &mut App) {
    app.apply(Action::Descend).unwrap(); // into the epic
    to_work_row(app);
    app.apply(Action::Descend).unwrap(); // into the ticket
    let index = app
        .nav()
        .rows()
        .iter()
        .position(|r| r.name == "blocked-by")
        .expect("a node's level carries its dependency list");
    app.apply(Action::CursorFirst).unwrap();
    for _ in 0..index {
        app.apply(Action::CursorDown).unwrap();
    }
}

/// The answers the open dialog lists, as the float shows them.
fn listed_answers(app: &App) -> Vec<String> {
    let Some(Modal::Dialog(dialog)) = app.modal() else {
        panic!("no dialog is open")
    };
    loti_tui::keymap::dialog_answers(dialog.answers(), dialog.words())
}

/// The set of answers the open dialog admits.
fn dialog_answer_set(app: &App) -> Answers {
    let Some(Modal::Dialog(dialog)) = app.modal() else {
        panic!("no dialog is open")
    };
    dialog.answers()
}

/// Every cell's symbol, row by row and untrimmed, so comparing two frames sees
/// the columns a trimmed line would have dropped.
fn cells(terminal: &Terminal<TestBackend>) -> Vec<Vec<String>> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
        .collect()
}

/// The bounding box of every cell that differs between two frames, as
/// `(x, y, width, height)`.
///
/// Everything outside it is identical by construction, which is how a float that
/// reflows nothing is told from one that pushed the screen around.
fn changed_box(before: &[Vec<String>], after: &[Vec<String>]) -> (usize, usize, usize, usize) {
    let mut changed = Vec::new();
    for (y, (row, was)) in after.iter().zip(before).enumerate() {
        for (x, (cell, previously)) in row.iter().zip(was).enumerate() {
            if cell != previously {
                changed.push((x, y));
            }
        }
    }
    assert!(!changed.is_empty(), "the frame did not change at all");
    let x0 = changed.iter().map(|(x, _)| *x).min().unwrap();
    let x1 = changed.iter().map(|(x, _)| *x).max().unwrap();
    let y0 = changed.iter().map(|(_, y)| *y).min().unwrap();
    let y1 = changed.iter().map(|(_, y)| *y).max().unwrap();
    (x0, y0, x1 - x0 + 1, y1 - y0 + 1)
}

/// The lines of a frame bounded to a box, so a dialog's own text is read without
/// the screen it is laid over.
fn box_lines(
    frame: &[Vec<String>],
    (x, y, width, height): (usize, usize, usize, usize),
) -> Vec<String> {
    frame[y..y + height]
        .iter()
        .map(|row| row[x..x + width].concat())
        .collect()
}

#[test]
fn a_dialog_is_a_centred_float_that_covers_what_is_under_it_and_moves_nothing() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    to_a_label_row(&mut app);
    app.apply(Action::EnterEditing).unwrap();
    let (before, _) = draw(&mut app);
    app.apply(Action::Delete).unwrap();
    let (after, _) = draw(&mut app);

    let (before, after) = (cells(&before), cells(&after));
    let region @ (x, y, width, height) = changed_box(&before, &after);
    let lines = box_lines(&after, region);
    // The changed region is the float itself, not a coincidence of characters: it
    // starts at the float's own corner and its text is the question.
    assert_eq!(after[y][x], "\u{250c}", "{lines:#?}");
    assert!(lines[1].contains("Remove label ui?"), "{lines:#?}");

    // Centred on the whole terminal, on both axes, rather than anchored to the
    // pane that raised it: a question that moves is harder to spot. The margins
    // match to within the odd column or row a centre cannot split.
    let (columns, rows) = (after[0].len(), after.len());
    assert!(
        x.abs_diff(columns - (x + width)) <= 1,
        "not centred across: {region:?}"
    );
    assert!(
        y.abs_diff(rows - (y + height)) <= 1,
        "not centred down: {region:?}"
    );

    // Nothing underneath moved: every cell outside the float is what it was, the
    // breadcrumb line, the hint strip and both pane frames included — which is
    // what being strictly inside the terminal on all four sides proves, given
    // that the region above bounds every cell that changed at all.
    assert!(x > 0 && y > 0, "{region:?}");
    assert!(x + width < columns && y + height < rows, "{region:?}");

    // Above everything, not merely framed on top of it: the cells the float
    // covers are cleared, so no pane text shows through beside its own.
    for row in &lines[1..height - 1] {
        let interior: String = row.chars().skip(1).take(width - 2).collect();
        assert!(
            interior.chars().all(|c| !"─│┌┐└┘├┤┬┴┼".contains(c)),
            "the screen shows through the float: {row:?}"
        );
    }
}

#[test]
fn a_destructive_question_lists_its_answers_and_never_the_reflex_key() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    to_a_label_row(&mut app);
    app.apply(Action::EnterEditing).unwrap();
    let (before, _) = draw(&mut app);
    app.apply(Action::Delete).unwrap();
    let (after, _) = draw(&mut app);

    let (before, after) = (cells(&before), cells(&after));
    let lines = box_lines(&after, changed_box(&before, &after));
    // A dialog says how to answer it, so the way out never depends on the hint
    // strip a notice or a narrow terminal may have taken.
    // The answers the open dialog lists: the key map's letters carrying this
    // dialog's own words, which is where each half lives.
    for answer in listed_answers(&app) {
        assert!(
            lines.iter().any(|l| l.contains(&answer)),
            "{answer:?} is not listed: {lines:#?}"
        );
    }
    assert_eq!(dialog_answer_set(&app), Answers::Destructive);
    // And the reflex key is listed nowhere, because it answers nothing here: a
    // reader arrives at a destructive question in a hurry.
    assert!(
        !lines.iter().any(|l| l.contains("Enter")),
        "the float offers the reflex key: {lines:#?}"
    );
}

#[test]
fn a_refusal_appears_in_a_dialog_carrying_the_stores_own_words() {
    let (_dir, store) = fixture();
    let mut app = App::new(store.clone(), Theme::with_color(false)).unwrap();
    to_a_label_row(&mut app);
    app.apply(Action::EnterEditing).unwrap();

    // Only the store can judge a write, so the browser offers the action and
    // shows what comes back: here the entity goes between offer and answer.
    let (before, _) = draw(&mut app);
    std::fs::remove_file(store.epic_path("browser")).unwrap();
    app.apply(Action::Delete).unwrap();
    app.apply(Action::Delete).unwrap();
    let (after, lines) = draw(&mut app);

    // Verbatim: the store's message as the store produced it, so the browser and
    // the CLI teach the same rule in the same words. Taken from the operation
    // itself rather than spelled out here, which is what a reworded refusal would
    // pass.
    let refusal = ops::remove_labels(&store, &Target::Epic("browser".into()), &["ui".to_string()])
        .expect_err("the store refuses a label removal on a missing entity")
        .to_string();
    let float = box_lines(&cells(&after), changed_box(&cells(&before), &cells(&after)));
    // The float's first text row is the message and nothing before it: a browser
    // word introducing the store's own would be a second voice on the rule.
    let first = float[1].trim_start_matches('\u{2502}').trim_start();
    assert!(
        first.starts_with(&refusal),
        "{refusal:?} is not what the float leads with: {float:#?}"
    );
    // A fixed title says what the float is, since the text in it is not the
    // browser's own and introduces itself with nothing.
    assert!(float[0].contains("refused"), "{float:#?}");
    // A failure is a dialog, never a transient notice: the strip is untouched.
    assert!(lines[23].contains("Esc leave"), "{:?}", lines[23]);
}

#[test]
fn a_long_question_wraps_inside_the_float_and_keeps_its_answers_on_screen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".loti");
    store::init(dir.path(), &root).unwrap();
    let store = Store::at(&root);
    ops::create_epic(
        &store,
        NewEpic {
            epic_id: "browser".into(),
            name: "The browser".into(),
            summary: "s".into(),
            labels: vec![],
            body: String::new(),
        },
    )
    .unwrap();
    // A label as long as a sentence, because a store refusal can be a paragraph
    // and both go through the same float.
    let label = "a label whose name someone pasted a whole sentence into, \
                 with a supercalifragilisticexpialidocious word in it";
    ops::add_labels(
        &store,
        &Target::Epic("browser".into()),
        &[label.to_string()],
    )
    .unwrap();

    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    to_a_label_row(&mut app);
    app.apply(Action::EnterEditing).unwrap();
    let (asked, _) = draw_at(&mut app, 30, 16);
    app.apply(Action::Delete).unwrap();

    // Room for the whole question: it wraps across the float's lines and none of
    // it is lost, so a reader is never asked about an object they can only half
    // see — whatever length the store or the reader gave it.
    let (roomy, _) = draw_at(&mut app, 30, 16);
    let whole = box_lines(&cells(&roomy), changed_box(&cells(&asked), &cells(&roomy)));
    assert!(
        whole.iter().any(|l| l.contains("word in it?")),
        "the question lost its tail: {whole:#?}"
    );
    assert!(
        !whole.iter().any(|l| l.contains('\u{2026}')),
        "nothing had to give way here: {whole:#?}"
    );

    // A terminal too short for the whole question, which is where the ranking
    // shows: the answers survive and the message is what gives way.
    let (width, height) = (30u16, 8u16);
    let (terminal, lines) = draw_at(&mut app, width, height);
    for line in &lines {
        assert!(
            line.chars().count() <= width as usize,
            "a line ran past the terminal: {line:?}"
        );
    }
    let float = box_lines(&cells(&terminal), (0, 0, width as usize, height as usize));
    // Every row of the float is closed on both sides, so the text wrapped inside
    // it rather than running over its border.
    let framed: Vec<&String> = float
        .iter()
        .filter(|l| {
            l.starts_with('\u{250c}') || l.starts_with('\u{2502}') || l.starts_with('\u{2514}')
        })
        .collect();
    for row in &framed {
        let last = row.trim_end().chars().last().unwrap();
        assert!(
            ['\u{2510}', '\u{2502}', '\u{2518}'].contains(&last),
            "a float row is not closed: {row:?}"
        );
    }
    // A dialog that listed no way to answer it would seal the reader inside it.
    assert!(float.iter().any(|l| l.contains("Esc cancel")), "{float:#?}");
    // And the message it had to shorten says so, so half a rule cannot be read as
    // the whole of one.
    assert!(float.iter().any(|l| l.contains('\u{2026}')), "{float:#?}");
}

#[test]
fn the_strip_lists_the_removal_only_where_the_row_offers_it() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    to_a_label_row(&mut app);
    app.apply(Action::EnterEditing).unwrap();
    let (_t, on_a_label) = draw(&mut app);
    assert!(on_a_label[23].contains("d remove"), "{:?}", on_a_label[23]);

    // A ticket cannot be deleted at all, so the letter is listed nowhere on it:
    // there is no dimmed, present-but-unavailable hint.
    app.apply(Action::Unwind).unwrap();
    app.apply(Action::Ascend).unwrap();
    to_work_row(&mut app);
    app.apply(Action::EnterEditing).unwrap();
    let (_t, on_a_ticket) = draw(&mut app);
    assert!(!on_a_ticket[23].contains("remove"), "{:?}", on_a_ticket[23]);
    assert!(
        on_a_ticket[23].contains("Esc leave"),
        "{:?}",
        on_a_ticket[23]
    );
}

#[test]
fn a_confirmed_removal_leaves_the_mode_and_names_the_label_on_the_strip() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    to_a_label_row(&mut app);
    app.apply(Action::EnterEditing).unwrap();
    app.apply(Action::Delete).unwrap();
    app.apply(Action::Delete).unwrap();
    let (_t, lines) = draw(&mut app);

    // The mode indicator going and the notice arriving are one frame, which is
    // what reads as "that finished".
    assert!(!lines[0].contains("EDITING"), "{:?}", lines[0]);
    assert!(lines[23].contains("label ui removed"), "{:?}", lines[23]);
    // The store was re-read: the epic held one label, so its labels level no
    // longer exists and the browser lands on the level above.
    assert_eq!(lines[0].trim(), "epics › browser");
    assert!(
        !lines.iter().skip(1).any(|l| l.contains("Remove label")),
        "the float outlived the write: {lines:#?}"
    );
}

#[test]
fn an_editing_surface_is_a_centred_float_that_covers_what_is_under_it_and_moves_nothing() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    to_the_labels_row(&mut app);
    app.apply(Action::EnterEditing).unwrap();
    let (before, _) = draw(&mut app);
    app.apply(Action::Add).unwrap();
    for c in "a new label".chars() {
        app.apply(Action::Insert(c)).unwrap();
    }
    let (after, _) = draw(&mut app);

    let (before, after) = (cells(&before), cells(&after));
    // Bounded to everything above the hint strip: the strip legitimately changes,
    // because the keys that apply are now the surface's own.
    let body = ..after.len() - 1;
    let region @ (x, y, width, height) = changed_box(&before[body], &after[body]);
    let lines = box_lines(&after, region);
    // The changed region is the float itself: it starts at its own corner, says
    // what is being added and to what, and carries the field and its text.
    assert_eq!(after[y][x], "\u{250c}", "{lines:#?}");
    assert!(lines[0].contains("new label on browser"), "{lines:#?}");
    assert!(lines[1].contains("label"), "{lines:#?}");
    assert!(lines[1].contains("a new label"), "{lines:#?}");

    // Centred on the whole terminal, like every other float: a reader looks for
    // one in the same place whatever raised it.
    let (columns, rows) = (after[0].len(), after.len());
    assert!(
        x.abs_diff(columns - (x + width)) <= 1,
        "not centred across: {region:?}"
    );
    assert!(
        y.abs_diff(rows - (y + height)) <= 1,
        "not centred down: {region:?}"
    );
    // Nothing underneath moved: every cell outside the float is what it was, the
    // breadcrumb line and both pane frames included — which is what being strictly
    // inside the body proves, given that the region bounds every cell above the
    // strip that changed at all.
    assert!(x > 0 && y > 0, "{region:?}");
    assert!(x + width < columns && y + height < rows - 1, "{region:?}");

    // Above everything: the cells it covers are cleared, so no pane text or border
    // shows through beside the field.
    for row in &lines[1..height - 1] {
        let interior: String = row.chars().skip(1).take(width - 2).collect();
        assert!(
            interior.chars().all(|c| !"─│┌┐└┘├┤┬┴┼".contains(c)),
            "the screen shows through the float: {row:?}"
        );
    }
}

#[test]
fn the_strip_carries_the_open_surfaces_own_keys_and_drops_the_editor_first() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    open_the_label_surface(&mut app, "x");
    let (_t, lines) = draw(&mut app);

    // The keys that apply right now are the surface's: saving, the external editor,
    // and the way out of the buffer. None of the mode's letters apply — no row
    // offers anything while a field is open — and neither browse hint does.
    for hint in ["Ctrl-S save", "Ctrl-G editor", "Esc cancel", "F1 keys"] {
        assert!(lines[23].contains(hint), "{hint:?}: {:?}", lines[23]);
    }
    for absent in ["a add", "Esc leave", "q quit", "? keys"] {
        assert!(!lines[23].contains(absent), "{absent:?}: {:?}", lines[23]);
    }
    // This surface holds one field, so it answers no key that moves between
    // fields, and the strip must not teach one: a hint naming a key the surface
    // ignores teaches a key that does nothing.
    assert!(!lines[23].contains("Tab"), "{:?}", lines[23]);

    // Ranked, not in key order: a terminal too narrow for everything drops the
    // power-user escape first and keeps the way out and help.
    let (_t, narrow) = draw_at(&mut app, 40, 12);
    assert!(narrow[11].contains("Ctrl-S save"), "{:?}", narrow[11]);
    assert!(!narrow[11].contains("Ctrl-G"), "{:?}", narrow[11]);
    assert!(narrow[11].contains("Esc cancel"), "{:?}", narrow[11]);
    assert!(narrow[11].contains("F1 keys"), "{:?}", narrow[11]);
}

#[test]
fn the_field_shows_the_text_with_the_cursor_where_the_next_character_lands() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    open_the_label_surface(&mut app, "typed");
    let (mut terminal, _) = draw(&mut app);

    // A field with no cursor does not say where typing goes, so the terminal's own
    // cursor is placed just after the text.
    let (x, y) = {
        let position = terminal.get_cursor_position().unwrap();
        (position.x as usize, position.y as usize)
    };
    let frame = cells(&terminal);
    let row = frame[y].concat();
    assert!(
        row.contains("typed"),
        "the cursor is not on the field: {row:?}"
    );
    assert_eq!(
        frame[y][x - 1],
        "d",
        "the cursor does not follow the text: {row:?}"
    );

    // Moving to the start of the field moves the cursor there and moves nothing
    // else: a motion is not a change.
    let before = frame;
    app.apply(Action::MoveToStart).unwrap();
    let (mut terminal, _) = draw(&mut app);
    let home = terminal.get_cursor_position().unwrap();
    assert_eq!((home.y as usize, home.x as usize), (y, x - "typed".len()));
    assert_eq!(before, cells(&terminal), "a motion changed the frame");

    // Content wider than the field scrolls rather than wrapping — it holds one
    // line — and the part shown contains the cursor, so a reader typing at the end
    // of a long value can see what they are typing.
    app.apply(Action::MoveToEnd).unwrap();
    let long = "a label somebody pasted a whole sentence into, at length";
    for c in long.chars() {
        app.apply(Action::Insert(c)).unwrap();
    }
    let (mut terminal, lines) = draw_at(&mut app, 40, 12);
    let end = terminal.get_cursor_position().unwrap();
    let float = cells(&terminal);
    assert!(
        lines.iter().all(|l| l.chars().count() <= 40),
        "the field ran past the terminal: {lines:#?}"
    );
    assert_eq!(float[end.y as usize][end.x as usize - 1], "h", "{lines:#?}");
    assert!(
        float[end.y as usize].concat().contains("at length"),
        "the end of the value is off the field: {lines:#?}"
    );
}

#[test]
fn the_discard_warning_covers_the_buffer_it_asks_about_and_words_its_own_answers() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    open_the_label_surface(&mut app, "half a thought");
    let (buffer, _) = draw(&mut app);
    app.apply(Action::Unwind).unwrap();
    let (warned, _) = draw(&mut app);

    let (buffer, warned) = (cells(&buffer), cells(&warned));
    let float = box_lines(&warned, changed_box(&buffer, &warned));
    // It names the field, because the frozen row is covered and a buffer carries no
    // label near it.
    assert!(
        float
            .iter()
            .any(|l| l.contains("Discard changes to label?")),
        "{float:#?}"
    );
    // The same destructive letter as a removal, with this dialog's own words: the
    // key is learned once and what it does is said here.
    for answer in listed_answers(&app) {
        assert!(
            float.iter().any(|l| l.contains(&answer)),
            "{answer:?} is not listed: {float:#?}"
        );
    }
    assert!(float.iter().any(|l| l.contains("d discard")), "{float:#?}");
    assert!(
        float.iter().any(|l| l.contains("Esc keep editing")),
        "{float:#?}"
    );
    assert!(
        !float.iter().any(|l| l.contains("remove")),
        "the buffer's warning borrowed the removal's words: {float:#?}"
    );
    // A reader arrives here in a hurry, so the reflex key answers nothing and is
    // listed nowhere.
    assert!(
        !float.iter().any(|l| l.contains("Enter")),
        "the float offers the reflex key: {float:#?}"
    );

    // Keeping the buffer lands back in it, text and all: the frame is the one the
    // warning was raised over.
    app.apply(Action::Unwind).unwrap();
    let (again, _) = draw(&mut app);
    assert_eq!(cells(&again), buffer);
}

#[test]
fn an_empty_required_field_warns_naming_it_and_the_buffer_survives_the_warning() {
    let (_dir, store) = fixture();
    let mut app = App::new(store.clone(), Theme::with_color(false)).unwrap();
    open_the_label_surface(&mut app, "");
    let (buffer, _) = draw(&mut app);
    app.apply(Action::Accept).unwrap();
    let (warned, lines) = draw(&mut app);

    let (buffer, warned) = (cells(&buffer), cells(&warned));
    let float = box_lines(&warned, changed_box(&buffer, &warned));
    assert!(
        float.iter().any(|l| l.contains("label is required.")),
        "{float:#?}"
    );
    // Dismissal is what this dialog carries, and it says where it lands.
    assert!(
        float.iter().any(|l| l.contains("back to the field")),
        "{float:#?}"
    );
    // A warning is a dialog, never a notice: the strip is untouched.
    assert!(lines[23].contains("Ctrl-S save"), "{:?}", lines[23]);
    // And nothing was written: the label set still holds what it held.
    assert_eq!(
        ops::list_labels(&store, &Target::Epic("browser".into())).unwrap(),
        vec!["ui".to_string()]
    );

    // Acknowledging lands back in the field, which is the frame the warning covered.
    app.apply(Action::Unwind).unwrap();
    let (again, _) = draw(&mut app);
    assert_eq!(cells(&again), buffer);
}

#[test]
fn a_blocker_surface_takes_a_reference_and_the_notice_names_what_the_store_recorded() {
    let (_dir, store) = fixture();
    let mut app = App::new(store.clone(), Theme::with_color(false)).unwrap();
    to_the_blocked_by_row(&mut app);
    app.apply(Action::EnterEditing).unwrap();
    let (before, _) = draw(&mut app);
    app.apply(Action::Add).unwrap();
    // A bare number is one of the two forms a reference is written in, and it names
    // a node of the dependency list's own epic.
    app.apply(Action::Insert('3')).unwrap();
    let (after, _) = draw(&mut app);

    let (before, after) = (cells(&before), cells(&after));
    // Bounded to everything above the hint strip: the strip legitimately changes,
    // because the keys that apply are now the surface's own.
    let body = ..after.len() - 1;
    let float = box_lines(&after, changed_box(&before[body], &after[body]));
    // The float says what is being added and to what, and carries the field and
    // the reference typed into it.
    assert!(float[0].contains("new blocker on browser/1"), "{float:#?}");
    assert!(float[1].contains("blocker reference"), "{float:#?}");
    assert!(float[1].contains('3'), "{float:#?}");

    app.apply(Action::Accept).unwrap();
    let (_t, lines) = draw(&mut app);
    // The mode indicator going, the float going and the notice arriving are one
    // frame — and the notice names the blocker as the store recorded it, which a
    // bare number is not.
    assert!(!lines[0].contains("EDITING"), "{:?}", lines[0]);
    assert!(
        !lines.iter().any(|l| l.contains("new blocker on")),
        "the float outlived the write: {lines:#?}"
    );
    assert!(
        lines[23].contains("blocker browser/3 added"),
        "{:?}",
        lines[23]
    );
    assert_eq!(
        ops::list_blocked_by(&store, &NodeRef::new("browser", 1)).unwrap(),
        vec!["browser/3".to_string()]
    );

    // The store was re-read: the list the reader is standing on now counts an
    // entry, and entering it shows the blocker itself.
    let row = lines
        .iter()
        .find(|l| l.contains("blocked-by"))
        .expect("the dependency list's row");
    assert!(row.contains("(1)"), "{row:?}");
    app.apply(Action::Descend).unwrap();
    let (_t, members) = draw(&mut app);
    assert!(
        members.iter().any(|l| l.contains("browser/3")),
        "the blocker is not in the level it was added to: {members:#?}"
    );
}

#[test]
fn a_blocker_removal_asks_naming_it_and_the_notice_names_it_once_it_is_gone() {
    let (_dir, store) = fixture();
    // A blocker to stand on, added before the level is read: a dependency list with
    // no entries has no row to remove.
    ops::add_blocked_by(
        &store,
        &NodeRef::new("browser", 1),
        &[NodeRef::new("browser", 3)],
    )
    .unwrap();
    let mut app = App::new(store.clone(), Theme::with_color(false)).unwrap();
    to_the_blocked_by_row(&mut app);
    app.apply(Action::Descend).unwrap(); // onto the blocker itself
    app.apply(Action::EnterEditing).unwrap();
    let (before, on_a_blocker) = draw(&mut app);
    // A dependency list has no rename, so an entry is only ever removed, and the
    // strip says so on the row that offers it.
    assert!(
        on_a_blocker[23].contains("d remove"),
        "{:?}",
        on_a_blocker[23]
    );

    app.apply(Action::Delete).unwrap();
    let (after, _) = draw(&mut app);
    let (before, after) = (cells(&before), cells(&after));
    let float = box_lines(&after, changed_box(&before, &after));
    // The question names the entry, because the frozen row is dimmed and the
    // entries of a list read alike.
    assert!(
        float
            .iter()
            .any(|l| l.contains("Remove blocker browser/3?")),
        "{float:#?}"
    );
    for answer in listed_answers(&app) {
        assert!(
            float.iter().any(|l| l.contains(&answer)),
            "{answer:?} is not listed: {float:#?}"
        );
    }

    app.apply(Action::Delete).unwrap();
    let (_t, lines) = draw(&mut app);
    assert!(!lines[0].contains("EDITING"), "{:?}", lines[0]);
    assert!(
        lines[23].contains("blocker browser/3 removed"),
        "{:?}",
        lines[23]
    );
    assert!(ops::list_blocked_by(&store, &NodeRef::new("browser", 1))
        .unwrap()
        .is_empty());
    // The store was re-read: the list is empty, so it is no longer a level and the
    // browser lands on the level above.
    assert_eq!(lines[0].trim(), "epics › browser › 1 Navigation pane");
    assert!(
        !lines.iter().skip(1).any(|l| l.contains("Remove blocker")),
        "the float outlived the write: {lines:#?}"
    );
}

#[test]
fn a_saved_label_leaves_the_mode_and_the_notice_names_it() {
    let (_dir, store) = fixture();
    let mut app = App::new(store, Theme::with_color(false)).unwrap();
    open_the_label_surface(&mut app, "shipped");
    app.apply(Action::Accept).unwrap();
    let (_t, lines) = draw(&mut app);

    // The mode indicator going, the float going and the notice arriving are one
    // frame, which is what reads as "that finished".
    assert!(!lines[0].contains("EDITING"), "{:?}", lines[0]);
    assert!(
        !lines.iter().any(|l| l.contains("new label on")),
        "the float outlived the write: {lines:#?}"
    );
    assert!(lines[23].contains("label shipped added"), "{:?}", lines[23]);

    // The store was re-read: the label set the reader is standing on now counts one
    // more member, and entering it shows the label itself.
    let row = lines
        .iter()
        .find(|l| l.contains("labels"))
        .expect("the label set's row");
    assert!(row.contains("(2)"), "{row:?}");
    app.apply(Action::Descend).unwrap();
    let (_t, members) = draw(&mut app);
    assert!(
        members.iter().any(|l| l.contains("shipped")),
        "the label is not in the level it was added to: {members:#?}"
    );
}

#[test]
fn an_asset_deletion_asks_naming_the_asset_and_the_notice_names_it_once_it_is_gone() {
    let (_dir, store) = fixture();
    // Two assets to stand among, added before the level is read: a collection with
    // no members has no row to delete, and one member could not tell the asset the
    // row names from the whole level.
    let epic = Target::Epic("browser".into());
    ops::add_asset(&store, &epic, "sketch.txt", None, b"sketch\n").unwrap();
    ops::add_asset(&store, &epic, "diagram.png", None, b"\x89PNG\r\n").unwrap();
    let mut app = App::new(store.clone(), Theme::with_color(false)).unwrap();
    to_the_assets_row(&mut app);
    app.apply(Action::Descend).unwrap(); // into the assets
    app.apply(Action::CursorLast).unwrap(); // onto the second of them
    app.apply(Action::EnterEditing).unwrap();
    let (before, on_an_asset) = draw(&mut app);
    // An asset cannot be attached or replaced from the browser, so it is only ever
    // deleted, and the strip says so on the row that offers it.
    assert!(
        on_an_asset[23].contains("d remove"),
        "{:?}",
        on_an_asset[23]
    );

    app.apply(Action::Delete).unwrap();
    let (after, _) = draw(&mut app);
    let (before, after) = (cells(&before), cells(&after));
    let float = box_lines(&after, changed_box(&before, &after));
    // The question names the asset, because the frozen row is dimmed and the
    // members of a collection read alike — and what goes here are bytes the store
    // keeps no tombstone for.
    assert!(
        float
            .iter()
            .any(|l| l.contains("Delete asset diagram.png?")),
        "{float:#?}"
    );
    for answer in listed_answers(&app) {
        assert!(
            float.iter().any(|l| l.contains(&answer)),
            "{answer:?} is not listed: {float:#?}"
        );
    }
    assert_eq!(dialog_answer_set(&app), Answers::Destructive);
    // Worded for what this question does, in the store's own verb for an asset:
    // the destructive letter removes a label on one dialog and deletes bytes here,
    // so a word shared across dialogs would say the wrong thing on one of them.
    assert!(float.iter().any(|l| l.contains("d delete")), "{float:#?}");
    assert!(float.iter().any(|l| l.contains("Esc cancel")), "{float:#?}");

    app.apply(Action::Delete).unwrap();
    let (_t, lines) = draw(&mut app);
    // The mode indicator going, the float going and the notice arriving are one
    // frame, which is what reads as "that finished".
    assert!(!lines[0].contains("EDITING"), "{:?}", lines[0]);
    assert!(
        lines[23].contains("asset diagram.png deleted"),
        "{:?}",
        lines[23]
    );
    assert!(
        !lines.iter().skip(1).any(|l| l.contains("Delete asset")),
        "the float outlived the write: {lines:#?}"
    );
    // The asset the row named and no other, and the store was re-read: the level
    // the reader is standing on holds the survivor alone.
    let names: Vec<String> = ops::list_assets(&store, &epic)
        .unwrap()
        .into_iter()
        .map(|asset| asset.name)
        .collect();
    assert_eq!(names, vec!["sketch.txt".to_string()]);
    assert!(
        lines.iter().any(|l| l.contains("sketch.txt")),
        "the survivor left the level too: {lines:#?}"
    );
    // Everything above the strip, because the notice on the strip's own line names
    // the asset deliberately: what must be gone is the row.
    assert!(
        !lines[1..23].iter().any(|l| l.contains("diagram.png")),
        "the deleted asset is still on screen: {lines:#?}"
    );
}

#[test]
fn the_assets_row_teaches_no_letter_and_names_the_command_that_attaches_one() {
    let (_dir, store) = fixture();
    let mut app = App::new(store.clone(), Theme::with_color(false)).unwrap();
    to_the_assets_row(&mut app);
    app.apply(Action::EnterEditing).unwrap();
    let (_t, lines) = draw(&mut app);

    // Attaching a payload is picking a file and carrying bytes about, which the
    // browser does not do at all: the row offers nothing, so the strip teaches no
    // letter on it — there is no dimmed, present-but-unavailable hint.
    assert!(!lines[23].contains("a add"), "{:?}", lines[23]);
    assert!(!lines[23].contains("d remove"), "{:?}", lines[23]);
    assert!(lines[23].contains("Esc leave"), "{:?}", lines[23]);

    // The letter pressed anyway names the command that does the job, on the strip's
    // own line, and the mode is still on: a row that offers nothing is not an
    // implicit exit.
    app.apply(Action::Add).unwrap();
    let (_t, signposted) = draw(&mut app);
    assert!(
        signposted[23].contains("loti epic asset add browser --file"),
        "{:?}",
        signposted[23]
    );
    assert!(signposted[0].contains("EDITING"), "{:?}", signposted[0]);
    // A notice is one line and clipping eats its tail, so the command leads and the
    // words come after it: what a reader has to retype is what survives a narrow
    // terminal, and losing the prose in front of it costs them nothing. Asserted as
    // the ordering rather than as a width, because the reference is as long as the
    // epic id a reader chose and no width can be promised for all of them.
    for width in [80u16, 60, 40] {
        let (_t, narrow) = draw_at(&mut app, width, 24);
        assert!(
            narrow[23].trim_start().starts_with("loti epic asset add"),
            "width {width}: {:?}",
            narrow[23]
        );
    }
    let (_t, narrow) = draw_at(&mut app, 80, 24);
    assert!(
        narrow[23].contains("loti epic asset add browser --file <path>"),
        "{:?}",
        narrow[23]
    );
    // And nothing was written: the browser has no way to attach one.
    assert!(ops::list_assets(&store, &Target::Epic("browser".into()))
        .unwrap()
        .is_empty());

    // A node's assets are a different command: the noun the command line gives a
    // node, and the node's own reference. A signpost naming the container the
    // reader is not standing on sends them to the wrong assets.
    app.apply(Action::Unwind).unwrap();
    app.apply(Action::Ascend).unwrap(); // back to the roster
    app.apply(Action::Descend).unwrap(); // into the epic
    to_work_row(&mut app);
    app.apply(Action::Descend).unwrap(); // into the ticket
    let index = app
        .nav()
        .rows()
        .iter()
        .position(|r| r.name == "assets")
        .expect("every container carries an assets row");
    app.apply(Action::CursorFirst).unwrap();
    for _ in 0..index {
        app.apply(Action::CursorDown).unwrap();
    }
    app.apply(Action::EnterEditing).unwrap();
    app.apply(Action::Add).unwrap();
    let (_t, on_a_ticket) = draw(&mut app);
    assert!(
        on_a_ticket[23].contains("loti ticket asset add browser/1 --file"),
        "{:?}",
        on_a_ticket[23]
    );
}
