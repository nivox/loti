//! The whole browser state, and the one place an [`Action`] is carried out.
//!
//! This module owns no terminal and draws nothing: it holds the position, the
//! layout and the preview, and applies intents to them. That keeps the state
//! machine testable without a terminal, and means a future write path is an
//! extra arm in [`App::apply`] rather than a change to the event loop.

use std::time::{Duration, Instant};

use anyhow::Result;
use loti_core::store::Store;
use ratatui_markdown::viewer::MarkdownViewer;

use crate::action::Action;
use crate::data::{self, Level, Selection};
use crate::nav::Nav;
use crate::theme::Theme;

/// The default share of the width given to the navigation pane.
pub const DEFAULT_NAV_PERCENT: u16 = 30;
/// The narrowest and widest the navigation pane may get. Neither pane may be
/// resized away entirely — a browser with no list, or no preview, is a broken
/// screen rather than a preference.
pub const MIN_NAV_PERCENT: u16 = 15;
/// See [`MIN_NAV_PERCENT`].
pub const MAX_NAV_PERCENT: u16 = 70;
/// How much one resize keypress moves the divider.
const RESIZE_STEP: u16 = 5;

/// How long a flash stays up. A maximum rather than a minimum — any key press
/// retires it early — and fixed rather than configurable, so the browser has one
/// learnable behaviour instead of two defaults.
const FLASH_LIFETIME: Duration = Duration::from_secs(5);

/// A transient one-line notice, holding the hint strip's line until its deadline
/// passes.
///
/// The deadline is wall-clock, not a count of wakeups: a notice raised just
/// before the browser hands the terminal to an external editor is simply expired
/// by the time the reader comes back.
struct Flash {
    message: String,
    deadline: Instant,
}

/// An overlay that takes the keyboard while it is open. Only the key-binding
/// overlay exists today; prompts and pickers a write path would need are the
/// same mechanism, so the routing is in place rather than retrofitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    /// The key-binding overlay.
    Help,
}

/// The preview pane's rendered document.
///
/// The viewer wraps to a width fixed when it is built and truncates anything
/// wider, so the pane's width is part of what identifies a rendering: when the
/// width changes the viewer is rebuilt rather than reused.
struct Preview {
    viewer: MarkdownViewer,
    width: u16,
    shown: Option<Selection>,
}

/// The browser.
pub struct App {
    store: Store,
    nav: Nav,
    theme: Theme,
    preview: Preview,
    nav_percent: u16,
    zoomed: bool,
    modal: Option<Modal>,
    /// The screen column the pane divider was last drawn at, so a mouse drag can
    /// tell whether it grabbed the divider. `None` until the first frame.
    divider_column: Option<u16>,
    dragging_divider: bool,
    /// Whether a frame is owed. The loop wakes on a tick and draws only when
    /// this is set; every input event sets it before dispatch, so a handler that
    /// forgets to ask costs a late timed repaint, never a stale reaction to the
    /// reader's own keypress.
    redraw: bool,
    /// The live notice, if any. One at a time: the strip it draws over holds a
    /// single line, so there is nothing a queue could show.
    flash: Option<Flash>,
}

impl App {
    /// Open the browser on a store, positioned at the epic roster.
    pub fn new(store: Store, theme: Theme) -> Result<Self> {
        let rows = data::rows(&store, &Level::Epics)?;
        Ok(Self {
            store,
            nav: Nav::new(rows),
            theme,
            preview: Preview {
                viewer: MarkdownViewer::new(),
                width: 0,
                shown: None,
            },
            nav_percent: DEFAULT_NAV_PERCENT,
            zoomed: false,
            modal: None,
            divider_column: None,
            dragging_divider: false,
            // The opening frame is owed: the browser paints before any input.
            redraw: true,
            flash: None,
        })
    }

    /// The navigation position.
    pub fn nav(&self) -> &Nav {
        &self.nav
    }

    /// The theme in force.
    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// The open overlay, if any.
    pub fn modal(&self) -> Option<&Modal> {
        self.modal.as_ref()
    }

    /// Whether the preview currently fills the width.
    pub fn zoomed(&self) -> bool {
        self.zoomed
    }

    /// The navigation pane's share of the width.
    pub fn nav_percent(&self) -> u16 {
        self.nav_percent
    }

    /// Carry out an intent. Returns whether the browser should exit.
    ///
    /// While an overlay is open it takes every key: only closing it, or
    /// quitting, gets through — so a keypress can never move an unseen cursor.
    pub fn apply(&mut self, action: Action) -> Result<bool> {
        if self.modal.is_some() {
            match action {
                Action::Quit => return Ok(true),
                Action::ToggleHelp | Action::Ascend => self.modal = None,
                _ => {}
            }
            return Ok(false);
        }

        match action {
            Action::Quit => return Ok(true),
            Action::ToggleHelp => self.modal = Some(Modal::Help),

            // Zoom hides the navigation pane, so the motion keys fall through to
            // the preview: they must never move a cursor the reader cannot see.
            Action::CursorDown if self.zoomed => self.preview.viewer.scroll_down(1),
            Action::CursorUp if self.zoomed => self.preview.viewer.scroll_up(1),
            Action::CursorFirst if self.zoomed => self.preview.viewer.scroll_to_top(),
            Action::CursorLast if self.zoomed => self.preview.viewer.scroll_to_bottom(),
            Action::Descend | Action::Ascend if self.zoomed => {}

            Action::CursorDown => self.nav.cursor_down(),
            Action::CursorUp => self.nav.cursor_up(),
            Action::CursorFirst => self.nav.cursor_first(),
            Action::CursorLast => self.nav.cursor_last(),
            Action::Descend => {
                if self.nav.can_descend() {
                    let store = &self.store;
                    self.nav.descend(|level| data::rows(store, level))?;
                } else {
                    // Why nothing happened: the row has no level under it. The
                    // absent child count says so too, but only to a reader who
                    // was looking at that column.
                    self.flash("nothing to open here");
                }
            }
            Action::Ascend => self.nav.ascend(),

            Action::PreviewHalfDown => self.preview.viewer.scroll_down(self.half_page()),
            Action::PreviewHalfUp => self.preview.viewer.scroll_up(self.half_page()),
            Action::PreviewPageDown => self.preview.viewer.page_down(),
            Action::PreviewPageUp => self.preview.viewer.page_up(),
            Action::PreviewTop => self.preview.viewer.scroll_to_top(),
            Action::PreviewBottom => self.preview.viewer.scroll_to_bottom(),

            Action::ShrinkNav => self.set_nav_percent(self.nav_percent.saturating_sub(RESIZE_STEP)),
            Action::GrowNav => self.set_nav_percent(self.nav_percent + RESIZE_STEP),
            Action::ResetSplit => self.set_nav_percent(DEFAULT_NAV_PERCENT),
            Action::ToggleZoom => self.zoomed = !self.zoomed,

            Action::Reload => {
                let store = &self.store;
                self.nav.reload(|level| data::rows(store, level))?;
            }
        }
        Ok(false)
    }

    /// Set the divider, clamped so neither pane can be resized away.
    pub fn set_nav_percent(&mut self, percent: u16) {
        self.nav_percent = percent.clamp(MIN_NAV_PERCENT, MAX_NAV_PERCENT);
    }

    /// Record where the divider was drawn, so a drag can recognise it.
    pub fn set_divider_column(&mut self, column: Option<u16>) {
        self.divider_column = column;
    }

    /// Begin a drag if the press landed on the divider. Returns whether the
    /// divider was grabbed.
    pub fn press(&mut self, column: u16) -> bool {
        // A one-column border is hard to hit exactly, so the column either side
        // counts as the divider too.
        self.dragging_divider = self
            .divider_column
            .is_some_and(|d| column + 1 >= d && column <= d + 1);
        self.dragging_divider
    }

    /// Continue a drag: move the divider to the pointer, as a share of the
    /// total width. Ignored unless a drag began on the divider.
    pub fn drag(&mut self, column: u16, total_width: u16) {
        if !self.dragging_divider || total_width == 0 {
            return;
        }
        let percent = (u32::from(column) * 100 / u32::from(total_width)) as u16;
        self.set_nav_percent(percent);
    }

    /// End any drag in progress.
    pub fn release(&mut self) {
        self.dragging_divider = false;
    }

    /// Ask for a frame. Requests coalesce: many between two wakeups draw once.
    pub fn request_redraw(&mut self) {
        self.redraw = true;
    }

    /// Whether a frame is owed, clearing the request. The loop's draw gate is
    /// the only caller, so a request is honoured by exactly one frame.
    pub fn take_redraw_request(&mut self) -> bool {
        std::mem::take(&mut self.redraw)
    }

    /// Raise a notice on the hint strip's line, replacing any live one and
    /// restarting its clock.
    ///
    /// The channel carries warnings and non-critical notices only — why nothing
    /// happened, and what a write did. Anything the reader must act on is a
    /// dialog instead, so the absence of a notice after an accepted surface says
    /// nothing was written.
    pub fn flash(&mut self, message: impl Into<String>) {
        self.raise_flash(message.into(), Instant::now());
    }

    /// The live notice's message, or `None`. The deadline is honoured here as
    /// well as swept between frames, so a frame drawn after it passed can never
    /// show an expired notice whatever else did or did not run.
    pub fn flash_message(&self) -> Option<&str> {
        self.flash_at(Instant::now())
    }

    /// Retire a notice early. Every key press does this — the lifetime is a
    /// maximum, not a minimum — before the key is dispatched, so a key that
    /// raises a notice of its own still leaves that one standing.
    pub fn clear_flash(&mut self) {
        self.flash = None;
    }

    /// Drop a notice whose deadline has passed, asking for the frame that takes
    /// it off the screen.
    ///
    /// Must run on every pass of the event loop, never only when the wait for
    /// input timed out: that wait is re-armed by every event, so a sustained
    /// stream of them — a divider drag, a held scroll — would otherwise keep a
    /// notice on screen for as long as the reader keeps them coming.
    pub fn expire_flash(&mut self) {
        self.expire_flash_at(Instant::now());
    }

    fn raise_flash(&mut self, message: String, now: Instant) {
        self.flash = Some(Flash {
            message,
            deadline: now + FLASH_LIFETIME,
        });
        // A notice raised outside the input path — by a timer, or by a future
        // background reload — would otherwise sit unseen until the next event.
        self.request_redraw();
    }

    fn flash_at(&self, now: Instant) -> Option<&str> {
        self.flash
            .as_ref()
            .filter(|flash| now < flash.deadline)
            .map(|flash| flash.message.as_str())
    }

    fn expire_flash_at(&mut self, now: Instant) {
        if self.flash.is_some() && self.flash_at(now).is_none() {
            self.flash = None;
            self.request_redraw();
        }
    }

    /// Bring the preview in line with the highlighted row, rebuilding it when
    /// the target or the pane width changed. Called once per frame, before the
    /// panes are drawn.
    pub fn sync_preview(&mut self, width: u16) {
        let target = self.nav.preview_target();
        let width_changed = width != self.preview.width;
        if !width_changed && target == self.preview.shown {
            return;
        }
        if width_changed {
            // The wrap width is fixed at construction, so a resized pane needs a
            // new viewer rather than a re-render.
            self.preview.viewer = MarkdownViewer::new().with_max_width(width.max(1) as usize);
            self.preview.width = width;
        }
        let content = match &target {
            Some(selection) => data::preview(&self.store, selection).unwrap_or_else(|e| {
                // A target can vanish under a browser that only reloads on
                // request; say so in the pane instead of tearing the session
                // down over a stale row.
                format!("# unavailable\n\n> {e}\n")
            }),
            None => "# no epics\n\n> This store has no epics yet.\n".to_string(),
        };
        self.preview.viewer.set_content(&content, &self.theme);
        if target != self.preview.shown {
            self.preview.viewer.scroll_to_top();
            self.preview.shown = target;
        }
    }

    /// The preview widget, for drawing.
    pub fn preview_viewer(&mut self) -> &mut MarkdownViewer {
        &mut self.preview.viewer
    }

    /// The title the preview carries: the reference of what it shows.
    pub fn preview_title(&self) -> String {
        self.preview
            .shown
            .as_ref()
            .map(Selection::reference)
            .unwrap_or_default()
    }

    fn half_page(&self) -> u16 {
        // The viewer knows its own page; half of the last drawn one is close
        // enough and needs no extra state.
        (self.preview.width / 2).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::fixture::Fixture;
    use crate::data::RowKind;
    use crate::theme::Theme;

    /// The browser on the shared fixture store. The fixture is returned with it
    /// because the store is deleted when the fixture is dropped.
    fn app() -> (Fixture, App) {
        let fx = Fixture::build();
        let app = App::new(fx.store.clone(), Theme::with_color(false)).unwrap();
        (fx, app)
    }

    /// Put the cursor on the first row of the given kind on the level on screen.
    fn to_row(app: &mut App, wanted: impl Fn(&RowKind) -> bool) {
        let index = app
            .nav()
            .rows()
            .iter()
            .position(|r| wanted(&r.kind))
            .expect("the level has such a row");
        app.apply(Action::CursorFirst).unwrap();
        for _ in 0..index {
            app.apply(Action::CursorDown).unwrap();
        }
    }

    /// Put the cursor on the first work row. Every epic and node level leads with
    /// its collection rows, so reaching a ticket means walking past them.
    fn to_work_row(app: &mut App) {
        to_row(app, |kind| matches!(kind, RowKind::Work(_)));
    }

    #[test]
    fn resizing_never_collapses_a_pane() {
        let (_fx, mut app) = app();
        for _ in 0..20 {
            app.apply(Action::ShrinkNav).unwrap();
        }
        assert_eq!(app.nav_percent(), MIN_NAV_PERCENT);
        for _ in 0..40 {
            app.apply(Action::GrowNav).unwrap();
        }
        assert_eq!(app.nav_percent(), MAX_NAV_PERCENT);
        app.apply(Action::ResetSplit).unwrap();
        assert_eq!(app.nav_percent(), DEFAULT_NAV_PERCENT);
    }

    #[test]
    fn an_open_overlay_swallows_navigation_keys() {
        let (_fx, mut app) = app();
        app.apply(Action::ToggleHelp).unwrap();
        app.apply(Action::Descend).unwrap();
        assert_eq!(app.nav().crumbs(), vec!["epics"]);
        app.apply(Action::ToggleHelp).unwrap();
        app.apply(Action::Descend).unwrap();
        assert_eq!(app.nav().crumbs(), vec!["epics", "feature"]);
    }

    #[test]
    fn quitting_works_even_with_an_overlay_open() {
        let (_fx, mut app) = app();
        app.apply(Action::ToggleHelp).unwrap();
        assert!(app.apply(Action::Quit).unwrap());
    }

    #[test]
    fn zoom_keeps_the_motion_keys_off_the_hidden_cursor() {
        let (_fx, mut app) = app();
        app.apply(Action::ToggleZoom).unwrap();
        app.apply(Action::CursorDown).unwrap();
        app.apply(Action::Descend).unwrap();
        assert_eq!(app.nav().cursor(), 0);
        assert_eq!(app.nav().crumbs(), vec!["epics"]);
    }

    #[test]
    fn descending_walks_epic_then_ticket_then_subticket() {
        let (_fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        app.apply(Action::Descend).unwrap(); // into the ticket
        assert_eq!(app.nav().crumbs(), vec!["epics", "feature", "1 Parent"]);
        // The subticket carries no meta and no subtickets of its own, and is
        // still enterable: its collection rows are there whatever it holds.
        to_work_row(&mut app);
        app.apply(Action::Descend).unwrap();
        assert_eq!(
            app.nav().crumbs(),
            vec!["epics", "feature", "1 Parent", "2 Child"]
        );
    }

    #[test]
    fn a_collection_is_a_level_of_its_own_and_the_breadcrumb_names_it() {
        let (_fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_row(
            &mut app,
            |kind| matches!(kind, RowKind::Collection(c) if c.name() == "comments"),
        );
        app.apply(Action::Descend).unwrap();
        assert_eq!(app.nav().crumbs(), vec!["epics", "feature", "comments"]);
        assert!(app.nav().at_collection());

        // A member is a leaf: it is read where it stands.
        app.apply(Action::Descend).unwrap();
        assert_eq!(app.nav().crumbs().len(), 3);
        assert_eq!(app.flash_message(), Some("nothing to open here"));

        // Leaving lands back on the row the level was entered from.
        app.apply(Action::Ascend).unwrap();
        assert_eq!(
            app.nav().frame().current().map(|r| r.name.clone()),
            Some("comments".to_string())
        );
    }

    #[test]
    fn a_drag_only_moves_the_divider_when_it_grabbed_it() {
        let (_fx, mut app) = app();
        app.set_divider_column(Some(30));
        assert!(!app.press(5));
        app.drag(60, 100);
        assert_eq!(app.nav_percent(), DEFAULT_NAV_PERCENT);
        assert!(app.press(31));
        app.drag(60, 100);
        assert_eq!(app.nav_percent(), 60);
        app.release();
        app.drag(20, 100);
        assert_eq!(app.nav_percent(), 60);
    }

    #[test]
    fn a_redraw_request_is_owed_to_exactly_one_frame() {
        let (_fx, mut app) = app();
        // The opening frame is owed without anyone asking for it.
        assert!(app.take_redraw_request());
        assert!(!app.take_redraw_request());

        app.request_redraw();
        app.request_redraw();
        assert!(app.take_redraw_request());
        assert!(!app.take_redraw_request());
    }

    #[test]
    fn a_flash_lives_its_fixed_lifetime_and_then_goes() {
        let (_fx, mut app) = app();
        let raised = Instant::now();
        app.raise_flash("something to say".into(), raised);
        assert_eq!(app.flash_at(raised), Some("something to say"));
        assert_eq!(
            app.flash_at(raised + FLASH_LIFETIME - Duration::from_millis(1)),
            Some("something to say")
        );
        assert_eq!(app.flash_at(raised + FLASH_LIFETIME), None);
    }

    #[test]
    fn a_newer_flash_replaces_the_live_one_and_restarts_its_clock() {
        let (_fx, mut app) = app();
        let first = Instant::now();
        let second = first + Duration::from_secs(4);
        app.raise_flash("first".into(), first);
        app.raise_flash("second".into(), second);
        assert_eq!(app.flash_at(second), Some("second"));
        // Past the first one's deadline, which the replacement discarded.
        assert_eq!(app.flash_at(first + FLASH_LIFETIME), Some("second"));
        assert_eq!(app.flash_at(second + FLASH_LIFETIME), None);
    }

    #[test]
    fn clearing_retires_a_flash_before_its_deadline() {
        let (_fx, mut app) = app();
        let raised = Instant::now();
        app.raise_flash("gone on the next key".into(), raised);
        app.clear_flash();
        assert_eq!(app.flash_at(raised), None);
    }

    #[test]
    fn an_expired_flash_asks_for_the_frame_that_removes_it_exactly_once() {
        let (_fx, mut app) = app();
        let raised = Instant::now();
        app.raise_flash("timed".into(), raised);
        assert!(app.take_redraw_request(), "a raised flash owes a frame");

        app.expire_flash_at(raised + Duration::from_secs(1));
        assert!(!app.take_redraw_request(), "a live flash owes nothing");

        app.expire_flash_at(raised + FLASH_LIFETIME);
        assert!(app.take_redraw_request(), "the strip has to come back");

        // The sweep is not a standing request: an empty strip is drawn once.
        app.expire_flash_at(raised + FLASH_LIFETIME + Duration::from_secs(1));
        assert!(!app.take_redraw_request());
    }

    #[test]
    fn entering_a_row_with_nothing_under_it_says_why_nothing_happened() {
        let (_fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        app.apply(Action::Descend).unwrap(); // into the ticket
        assert_eq!(
            app.flash_message(),
            None,
            "a level opened, so nothing to say"
        );

        // An empty collection prints no count and has nothing to show.
        to_work_row(&mut app);
        app.apply(Action::Descend).unwrap(); // into the subticket, which has no meta
        let depth = app.nav().crumbs().len();
        app.apply(Action::Descend).unwrap();
        assert_eq!(app.flash_message(), Some("nothing to open here"));
        assert_eq!(
            app.nav().crumbs().len(),
            depth,
            "the level must not have moved"
        );
    }

    #[test]
    fn the_preview_titles_itself_with_the_reference_it_shows() {
        let (_fx, mut app) = app();
        app.sync_preview(60);
        assert_eq!(app.preview_title(), "feature");
        app.apply(Action::Descend).unwrap();
        to_work_row(&mut app);
        app.sync_preview(60);
        assert_eq!(app.preview_title(), "feature/1");
    }

    #[test]
    fn a_collection_row_keeps_the_containers_document_in_the_pane() {
        let (fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        app.sync_preview(60);
        // The pane title names the container, because the container's document is
        // what it shows: a collection has none of its own.
        assert_eq!(app.preview_title(), "feature");

        // And it does not change as the cursor moves down the collection rows,
        // nor when it reaches the labels themselves.
        to_row(
            &mut app,
            |kind| matches!(kind, RowKind::Collection(c) if c.name() == "labels"),
        );
        app.apply(Action::Descend).unwrap();
        for _ in 0..app.nav().rows().len() {
            app.sync_preview(60);
            assert_eq!(app.preview_title(), fx.epic);
            app.apply(Action::CursorDown).unwrap();
        }
    }
}
