//! The whole browser state, and the one place an [`Action`] is carried out.
//!
//! This module owns no terminal and draws nothing: it holds the position, the
//! layout and the preview, and applies intents to them. That keeps the state
//! machine testable without a terminal, and means a future write path is an
//! extra arm in [`App::apply`] rather than a change to the event loop.

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
                let store = &self.store;
                self.nav.descend(|level| data::rows(store, level))?;
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
    use crate::theme::Theme;

    /// A store with one epic, one ticket and one subticket.
    pub(crate) fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".loti");
        loti_core::store::init(dir.path(), &root).unwrap();
        let store = Store::at(&root);
        loti_core::ops::create_epic(
            &store,
            loti_core::ops::NewEpic {
                epic_id: "feature".into(),
                name: "A feature".into(),
                summary: "Scope".into(),
                body: String::new(),
                labels: vec![],
            },
        )
        .unwrap();
        let parent = loti_core::ops::create_node(
            &store,
            loti_core::ops::NewNode {
                epic_id: "feature".into(),
                parent: None,
                name: "Parent".into(),
                summary: "s".into(),
                body: String::new(),
                labels: vec![],
            },
        )
        .unwrap();
        loti_core::ops::create_node(
            &store,
            loti_core::ops::NewNode {
                epic_id: "feature".into(),
                parent: Some(loti_core::domain::NodeRef {
                    epic_id: "feature".into(),
                    number: parent.frontmatter.number,
                }),
                name: "Child".into(),
                summary: "s".into(),
                body: String::new(),
                labels: vec![],
            },
        )
        .unwrap();
        (dir, store)
    }

    fn app() -> (tempfile::TempDir, App) {
        let (dir, store) = fixture();
        let app = App::new(store, Theme::with_color(false)).unwrap();
        (dir, app)
    }

    #[test]
    fn resizing_never_collapses_a_pane() {
        let (_dir, mut app) = app();
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
        let (_dir, mut app) = app();
        app.apply(Action::ToggleHelp).unwrap();
        app.apply(Action::Descend).unwrap();
        assert_eq!(app.nav().crumbs(), vec!["epics"]);
        app.apply(Action::ToggleHelp).unwrap();
        app.apply(Action::Descend).unwrap();
        assert_eq!(app.nav().crumbs(), vec!["epics", "feature"]);
    }

    #[test]
    fn quitting_works_even_with_an_overlay_open() {
        let (_dir, mut app) = app();
        app.apply(Action::ToggleHelp).unwrap();
        assert!(app.apply(Action::Quit).unwrap());
    }

    #[test]
    fn zoom_keeps_the_motion_keys_off_the_hidden_cursor() {
        let (_dir, mut app) = app();
        app.apply(Action::ToggleZoom).unwrap();
        app.apply(Action::CursorDown).unwrap();
        app.apply(Action::Descend).unwrap();
        assert_eq!(app.nav().cursor(), 0);
        assert_eq!(app.nav().crumbs(), vec!["epics"]);
    }

    #[test]
    fn descending_walks_epic_then_ticket_then_subticket() {
        let (_dir, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        app.apply(Action::Descend).unwrap(); // into the ticket
        assert_eq!(app.nav().crumbs(), vec!["epics", "feature", "1 Parent"]);
        // The subticket is a leaf, so entering it does nothing.
        app.apply(Action::Descend).unwrap();
        assert_eq!(app.nav().crumbs().len(), 3);
    }

    #[test]
    fn a_drag_only_moves_the_divider_when_it_grabbed_it() {
        let (_dir, mut app) = app();
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
    fn the_preview_titles_itself_with_the_reference_it_shows() {
        let (_dir, mut app) = app();
        app.sync_preview(60);
        assert_eq!(app.preview_title(), "feature");
        app.apply(Action::Descend).unwrap();
        app.sync_preview(60);
        assert_eq!(app.preview_title(), "feature/1");
    }
}
