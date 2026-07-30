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

use crate::action::{Action, Answers, EditingAction, Mode};
use crate::data::{self, Level, Selection};
use crate::keymap;
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

/// What a key editing mode does not admit says. It carries the way out itself,
/// because a notice covers the whole hint strip — the essential pair with it —
/// for as long as it is up.
const NOT_AN_EDITING_ACTION: &str = "not an editing action — Esc to leave";

/// The title a question about the frozen row carries. Fixed, so what a dialog is
/// stays legible when its text is the store's own and no browser word introduces
/// it.
const CONFIRM_TITLE: &str = " confirm ";
/// See [`CONFIRM_TITLE`].
const REFUSAL_TITLE: &str = " the store refused the change ";

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

/// An overlay that takes the keyboard while it is open.
///
/// One at a time, and nesting stops here: a dialog is always answer-or-dismiss
/// and can never itself raise another, so a refusal that answers a question
/// replaces it rather than stacking on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    /// The key-binding overlay.
    Help,
    /// A dialog. One widget carries every question and every report, because each
    /// of them is a critical interruption laid over the screen — the transient
    /// notice channel carries only what a reader need not act on.
    Dialog(Dialog),
}

/// A dialog: what it says, how it may be answered, and what answering it
/// performs.
///
/// Invariant: a dialog carries all three itself, so the state machine that
/// answers one names no operation and no answer set of its own — which is what
/// makes a further kind of dialog a value built here rather than another branch
/// everywhere a dialog is routed or drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    title: &'static str,
    message: String,
    answers: Answers,
    /// What the affirmative answer performs, and `None` for a dialog that only
    /// reports: such a dialog has a way out and no answer, so nothing it is shown
    /// for can be acted on by mistake.
    performs: Option<Performs>,
}

/// What a dialog's affirmative answer performs.
///
/// Invariant: every consequence an answer can have is a variant here, so the one
/// place a dialog is answered performs whatever the dialog carried and names no
/// operation itself — an answer that changes no store state is a variant rather
/// than another branch there.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Performs {
    /// A change to the store, with what the notice says once it is committed. The
    /// two are built with the question, so a question and the notice that follows
    /// it can never name different things.
    Write { write: data::Write, done: String },
}

impl Dialog {
    /// A destructive question, carrying the write its answer performs and what
    /// that write says once it has happened.
    ///
    /// Every deletion is gated behind one of these: a label is trivially
    /// re-addable and the gate buys little there, but one rule a reader can
    /// predict beats a per-action judgement they have to remember.
    fn confirm(question: String, write: data::Write, done: String) -> Self {
        Self {
            title: CONFIRM_TITLE,
            message: question,
            answers: Answers::Destructive,
            performs: Some(Performs::Write { write, done }),
        }
    }

    /// A refusal, in the store's own words and with nothing to answer: the reader
    /// is being told, not asked.
    fn refusal(message: String) -> Self {
        Self {
            title: REFUSAL_TITLE,
            message,
            answers: Answers::Acknowledge,
            performs: None,
        }
    }

    /// The fixed title that says what kind of dialog this is, since the text in it
    /// may be the store's own and introduces itself with nothing.
    pub fn title(&self) -> &str {
        self.title
    }

    /// What the dialog asks or reports.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The set of answers it admits, which is also the set it lists.
    pub fn answers(&self) -> Answers {
        self.answers
    }
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
    /// The row editing mode was entered on, for as long as the mode is on.
    ///
    /// Invariant: while this is `Some` the selection is frozen — neither the
    /// cursor nor the level moves — so the row held here is always the
    /// highlighted row, which is what lets the screen show one row being acted
    /// on instead of a list being browsed. A reload that leaves the two apart
    /// ends the mode rather than letting them drift.
    editing: Option<Selection>,
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
            editing: None,
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

    /// The row editing mode is acting on, or `None` while browsing.
    pub fn editing_target(&self) -> Option<&Selection> {
        self.editing.as_ref()
    }

    /// Which set of bindings the keyboard is under.
    ///
    /// A dialog is a mode of its own, because a question must admit its own
    /// answers and nothing else. The key overlay is not: it is a layer above
    /// whichever mode raised it, and unwinding takes one layer at a time, so the
    /// key that closes it is that mode's own way out.
    pub fn mode(&self) -> Mode {
        match &self.modal {
            // The answers belong to the dialog, so the set the keyboard is under is
            // read off the open dialog rather than off a mode per kind of dialog.
            Some(Modal::Dialog(dialog)) => Mode::Dialog(dialog.answers),
            Some(Modal::Help) | None => match self.editing.is_some() {
                true => Mode::Editing,
                false => Mode::Browse,
            },
        }
    }

    /// The hints editing mode's strip may drop: the actions the frozen row
    /// offers, in the key map's own order.
    ///
    /// Only actions the browser believes it can perform are ever listed, and one
    /// predicate decides both what is listed and what the key does — so the strip
    /// can never name a letter the row ignores, nor hide one it answers.
    pub fn editing_hints(&self) -> Vec<&'static str> {
        keymap::FOOTER_HINTS_EDITING
            .iter()
            .filter(|(action, _)| self.offers(*action))
            .map(|(_, hint)| *hint)
            .collect()
    }

    /// Whether the frozen row offers an editing action. Derived from the offer
    /// itself, so a hint and the key it names cannot disagree about a row.
    fn offers(&self, action: EditingAction) -> bool {
        self.offer(action).is_some()
    }

    /// The dialog an editing action raises on the frozen row, or `None` where the
    /// row does not offer that action.
    ///
    /// Invariant: this is the whole of what a row offers — both the hint strip and
    /// the key ask it — and it is exhaustive over the editing actions, so an
    /// action added without deciding which rows offer it does not compile rather
    /// than showing a hint no key answers.
    ///
    /// A question names the object, because the frozen row is dimmed and the
    /// members of a collection read alike: an unnamed question would not say what
    /// is about to go. The notice names it for the same reason — by the time it
    /// is read the row is gone.
    fn offer(&self, action: EditingAction) -> Option<Dialog> {
        let target = self.editing.as_ref()?;
        match action {
            EditingAction::Delete => match target {
                // A label set has no rename, so a label is only ever removed.
                Selection::Label(_, label) => Some(Dialog::confirm(
                    format!("Remove label {label}?"),
                    data::Write::RemoveLabel(target.clone()),
                    format!("label {label} removed"),
                )),
                _ => None,
            },
        }
    }

    /// Carry out an intent. Returns whether the browser should exit.
    ///
    /// While an overlay is open it takes every key, so a keypress can never move
    /// an unseen cursor. Editing mode is the layer under it, and unwinding takes
    /// one layer at a time: closing the overlay leaves the mode standing.
    pub fn apply(&mut self, action: Action) -> Result<bool> {
        if self.modal.is_some() {
            return self.apply_to_modal(action);
        }
        if self.editing.is_some() {
            self.apply_editing(action)?;
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
            Action::Descend | Action::Ascend | Action::Unwind if self.zoomed => {}
            // The same rule, said out loud because nothing on screen would say
            // it: an action that needs a visible cursor does nothing while there
            // is none. Editing mode needs one twice over — to freeze it, and to
            // show which row is frozen — and none of the marks it would show for
            // that exist without the navigation pane. The screen is the reader's
            // choice, so the refusal leaves it as it is rather than un-zooming.
            Action::EnterEditing if self.zoomed => {
                self.flash("nothing to edit while the preview fills the width")
            }

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
            // Nothing is open above the level, so unwinding is leaving it.
            Action::Ascend | Action::Unwind => self.nav.ascend(),

            // The letters of editing mode's actions are bound inside the mode
            // only, so nothing carries this intent while browsing.
            Action::Delete => {}

            Action::EnterEditing => match self.nav.frame().current() {
                Some(row) => self.editing = Some(row.selection.clone()),
                // The roster of an empty store is the browser's one screen with
                // no selection, and the mode acts on a row.
                None => self.flash("nothing to edit: this store has no epics"),
            },

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

            Action::Reload => self.reload()?,
        }
        Ok(false)
    }

    /// Carry out an intent while an overlay is open.
    ///
    /// The key overlay is a layer above whichever mode raised it, so it is closed
    /// by that mode's own way out and quitting gets through it. A dialog is not:
    /// it admits the answers it lists and nothing else, because a question is
    /// raised only for something failed or costly, and it must be answered rather
    /// than escaped past.
    fn apply_to_modal(&mut self, action: Action) -> Result<bool> {
        if matches!(self.modal, Some(Modal::Help)) {
            match action {
                Action::Quit => return Ok(true),
                Action::ToggleHelp | Action::Unwind | Action::Ascend => self.modal = None,
                _ => {}
            }
            return Ok(false);
        }
        // A dialog admits its listed answers and nothing else, quitting included:
        // a question this critical must be answered rather than escaped past, and
        // nothing underneath it may move while it is open.
        match action {
            // The affirmative answer performs whatever the dialog carries, so no
            // one operation is named here and a further kind of question needs no
            // arm of its own.
            Action::Delete => self.answer()?,
            // A refused write leaves the editing session standing: only a
            // successful write ends it, so dismissing lands back in the mode.
            Action::Unwind => self.modal = None,
            _ => {}
        }
        Ok(false)
    }

    /// Carry out what the open dialog's answer performs.
    ///
    /// A successful write ends the editing session and says what it did: the
    /// session is one edit long, and the mode indicator going as the notice
    /// arrives is what reads as "that finished". The store is re-read with it,
    /// because the row just removed must not stay on screen.
    fn answer(&mut self) -> Result<()> {
        // A dialog that only reports has a way out and no answer, so nothing it
        // was raised for can be performed: it stands until it is dismissed.
        let Some(Modal::Dialog(dialog)) = &self.modal else {
            return Ok(());
        };
        let Some(performs) = dialog.performs.clone() else {
            return Ok(());
        };
        self.modal = None;
        match performs {
            Performs::Write { write, done } => match data::perform(&self.store, &write) {
                Ok(()) => {
                    self.editing = None;
                    self.reload()?;
                    self.flash(done);
                }
                // The store's own words, so the browser and the CLI teach the same
                // rule in the same words and the browser cannot go stale when a
                // store rule gains a nuance.
                Err(e) => self.modal = Some(Modal::Dialog(Dialog::refusal(e.to_string()))),
            },
        }
        Ok(())
    }

    /// Carry out an intent while editing mode is on.
    ///
    /// The mode admits the actions the frozen row offers, the way out, help and a
    /// reload, and ignores everything else with a notice naming the way out. With
    /// the selection frozen there is nothing left for a motion, level or layout
    /// key to do, and an unknown key is deliberately not an implicit exit: a typo
    /// must not silently drop the reader out of a mode whose indicator is at the
    /// top of the screen while their eyes are on the row.
    ///
    /// Quitting is one of the keys the mode does not admit, so no key reaching
    /// this far can end the session. The overlay is the exception, and a layer
    /// above: a key that opens it is answered there, and quitting gets through
    /// that layer whether or not the mode is on.
    fn apply_editing(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Unwind => self.editing = None,
            Action::ToggleHelp => self.modal = Some(Modal::Help),
            // Nothing is pending at this layer, so a reload is safe — and it is
            // the natural move when the preview looks stale before committing to
            // an edit.
            Action::Reload => {
                self.reload()?;
                let target = self.nav.frame().current().map(|row| &row.selection);
                if target != self.editing.as_ref() {
                    // The mode acts on one row, so a row that is gone ends it.
                    // Where the cursor lands instead is the ordinary reload
                    // fallback's business: the mode invents no second recovery.
                    self.editing = None;
                    self.flash("the row you were editing is gone");
                }
            }
            // Everything else is an editing action or nothing. A letter is listed
            // only where the row offers it, so a letter this row does not offer is
            // as unknown here as any key the mode never binds — and the offer that
            // decides it is the one the hint strip asked.
            _ => match EditingAction::for_intent(action).and_then(|a| self.offer(a)) {
                Some(dialog) => self.modal = Some(Modal::Dialog(dialog)),
                None => self.flash(NOT_AN_EDITING_ACTION),
            },
        }
        Ok(())
    }

    /// Re-read every level from the store.
    fn reload(&mut self) -> Result<()> {
        let store = &self.store;
        self.nav.reload(|level| data::rows(store, level))
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

    /// Stand on the first label of the epic's own labels level, which is where a
    /// removal is offered.
    fn to_a_label_row(app: &mut App) {
        app.apply(Action::Descend).unwrap(); // into the epic
        to_row(
            app,
            |kind| matches!(kind, RowKind::Collection(c) if c.name() == "labels"),
        );
        app.apply(Action::Descend).unwrap();
    }

    /// The token the highlighted row is addressed by, which for a label row is
    /// the label itself.
    fn row_label(app: &App) -> String {
        app.nav()
            .frame()
            .current()
            .expect("a highlighted row")
            .label
            .clone()
    }

    /// The hint the strip carries for an editing action, so a test names the
    /// action and leaves the wording to the key map.
    fn hint_for(action: EditingAction) -> &'static str {
        keymap::FOOTER_HINTS_EDITING
            .iter()
            .find(|(bound, _)| *bound == action)
            .map(|(_, hint)| *hint)
            .expect("the action has a hint")
    }

    /// The strip and the keys agree about the frozen row, action by action: every
    /// editing action is either hinted and answered, or unhinted and as unknown as
    /// any key the mode never binds. Nothing else may happen on the way — the
    /// browser is left as it was found.
    fn hints_and_keys_agree(app: &mut App) {
        for action in EditingAction::ALL.iter().copied() {
            let hinted = app.editing_hints().contains(&hint_for(action));
            app.clear_flash();
            assert!(!app.apply(action.intent()).unwrap(), "{action:?} quit");
            match hinted {
                true => {
                    assert!(
                        matches!(app.modal(), Some(Modal::Dialog(_))),
                        "{action:?} is hinted but the key answers nothing"
                    );
                    // Back out of the question, leaving the mode standing.
                    app.apply(Action::Unwind).unwrap();
                }
                false => {
                    assert_eq!(
                        app.modal(),
                        None,
                        "{action:?} is not hinted but the key acted"
                    );
                    assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION));
                }
            }
            app.clear_flash();
            assert!(
                app.editing_target().is_some(),
                "{action:?} left the editing mode"
            );
        }
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
        // Standing inside a level, so a level key has somewhere to go if it is
        // wrongly honoured: at the roster it would be a no-op either way.
        app.apply(Action::Descend).unwrap();
        app.apply(Action::ToggleZoom).unwrap();

        // With the navigation pane gone there is no cursor to move and no level
        // to leave: the motions fall through to the preview, and every intent
        // that would change the level — unwinding included, since with nothing
        // over the level that is what it unwinds — does nothing at all.
        for action in [
            Action::CursorDown,
            Action::CursorUp,
            Action::Descend,
            Action::Ascend,
            Action::Unwind,
        ] {
            app.apply(action).unwrap();
            assert_eq!(app.nav().cursor(), 0, "{action:?} moved a hidden cursor");
            assert_eq!(
                app.nav().crumbs(),
                vec!["epics", "feature"],
                "{action:?} changed the level"
            );
        }
    }

    #[test]
    fn unwinding_leaves_a_level_and_never_the_application() {
        let (_fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        assert_eq!(app.nav().crumbs(), vec!["epics", "feature"]);

        // Nothing is open over the level, so unwinding is leaving it — and it is
        // never the way out of the browser, so a mis-hit cannot discard the
        // session even at the root, where there is nothing left to unwind.
        assert!(!app.apply(Action::Unwind).unwrap());
        assert_eq!(app.nav().crumbs(), vec!["epics"]);
        assert!(!app.apply(Action::Unwind).unwrap());
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
    fn editing_mode_is_entered_on_the_highlighted_row_and_left_by_the_way_out() {
        let (_fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        let row = app.nav().frame().current().unwrap().selection.clone();

        assert_eq!(app.mode(), Mode::Browse);
        app.apply(Action::EnterEditing).unwrap();
        assert_eq!(app.editing_target(), Some(&row));
        // The bindings the keyboard is under are derived from the frozen row and
        // nothing else: it is the only bridge from this state to the key table,
        // so a mode that did not follow the row would silently hand the mode
        // browse's meanings for the same keys.
        assert_eq!(app.mode(), Mode::Editing);
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.mode(), Mode::Browse);
        // The way out of the mode is not the way out of the level: leaving the
        // mode leaves the reader exactly where they were.
        assert_eq!(app.nav().crumbs(), vec!["epics", "feature"]);
        assert_eq!(
            app.nav().frame().current().map(|r| r.selection.clone()),
            Some(row)
        );

        // And the same key, once the mode is off, is the level's way out again:
        // the mode borrowed it for as long as it was on, and no longer.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.nav().crumbs(), vec!["epics"]);
    }

    #[test]
    fn the_selection_is_frozen_while_the_mode_is_on() {
        let (_fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        let (cursor, crumbs, split) = (
            app.nav().cursor(),
            app.nav().crumbs().join("/"),
            app.nav_percent(),
        );

        // Not the motion keys, not the level keys, and not the layout keys: with
        // one row frozen as the target there is nothing left for them to do.
        for action in [
            Action::CursorDown,
            Action::CursorUp,
            Action::CursorFirst,
            Action::CursorLast,
            Action::Descend,
            Action::Ascend,
            Action::ShrinkNav,
            Action::GrowNav,
            Action::ResetSplit,
            Action::ToggleZoom,
        ] {
            assert!(!app.apply(action).unwrap(), "{action:?} left the browser");
            assert!(app.editing_target().is_some(), "{action:?} left the mode");
            assert_eq!(app.nav().cursor(), cursor, "{action:?} moved the cursor");
            assert_eq!(
                app.nav().crumbs().join("/"),
                crumbs,
                "{action:?} changed the level"
            );
            assert_eq!(app.nav_percent(), split, "{action:?} moved the divider");
            assert!(!app.zoomed(), "{action:?} rearranged the screen");
        }
    }

    #[test]
    fn an_ignored_key_says_how_to_leave_and_does_not_leave() {
        let (_fx, mut app) = app();
        app.apply(Action::EnterEditing).unwrap();

        // Quitting is not an editing action either: a stray key must not end the
        // session from inside a mode whose indicator is at the top of the screen.
        assert!(!app.apply(Action::Quit).unwrap());
        assert!(app.editing_target().is_some());
        let notice = app.flash_message().expect("an ignored key says why");
        assert!(
            notice.contains("Esc"),
            "{notice:?} does not name the way out"
        );
    }

    #[test]
    fn help_is_reachable_from_the_mode_and_closing_it_leaves_the_mode_standing() {
        let (_fx, mut app) = app();
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::ToggleHelp).unwrap();
        assert_eq!(app.modal(), Some(&Modal::Help));
        // The overlay is a layer above the mode, not a way out of it, so the
        // keyboard is still under the mode's bindings while it is open.
        assert_eq!(app.mode(), Mode::Editing);

        // One layer at a time: the overlay goes and the mode stays.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert!(app.editing_target().is_some());
        assert_eq!(app.mode(), Mode::Editing);
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.editing_target(), None);
    }

    #[test]
    fn a_reload_that_changes_nothing_leaves_the_mode_on() {
        let (_fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        let target = app.editing_target().cloned();

        // Nothing is pending at this layer, so a reload is an ordinary reload.
        app.apply(Action::Reload).unwrap();
        assert_eq!(app.editing_target(), target.as_ref());
        assert_eq!(app.flash_message(), None, "nothing happened worth saying");
    }

    #[test]
    fn a_reload_that_removes_the_frozen_row_ends_the_mode_and_says_so() {
        let (fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_row(
            &mut app,
            |kind| matches!(kind, RowKind::Collection(c) if c.name() == "labels"),
        );
        app.apply(Action::Descend).unwrap(); // into the labels
        app.apply(Action::EnterEditing).unwrap();
        assert!(app.editing_target().is_some());

        // A reload that removes nothing has to leave the mode standing.
        app.apply(Action::Reload).unwrap();
        assert!(app.editing_target().is_some());

        // Now the frozen row goes, and with the last member the level goes too.
        fx.strip_the_epics_labels();
        app.apply(Action::Reload).unwrap();
        assert_eq!(app.editing_target(), None, "the frozen row is gone");
        let notice = app.flash_message().expect("a mode that ends says why");
        assert!(notice.contains("gone"), "{notice:?}");
        // The browser's own reload fallback took over: no second recovery story.
        assert_eq!(app.nav().crumbs(), vec!["epics", "feature"]);
    }

    #[test]
    fn the_mode_is_refused_while_the_preview_fills_the_width() {
        let (_fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        app.apply(Action::ToggleZoom).unwrap();

        // None of the mode's marks exist without the navigation pane — no gutter
        // bar, no dimming, no framed pane — and the frozen row is off screen, so
        // the indicator could not say which row is the target. Refused with a
        // notice, on the rule that already leaves the level keys nothing to do.
        app.apply(Action::EnterEditing).unwrap();
        assert_eq!(app.editing_target(), None);
        assert!(app.flash_message().is_some(), "a refusal has to say why");
        // And refused, not worked around: the screen is the reader's choice.
        assert!(app.zoomed(), "the mode must not un-zoom the screen");

        // Once the row list is back, so is the mode.
        app.apply(Action::ToggleZoom).unwrap();
        app.apply(Action::EnterEditing).unwrap();
        assert!(app.editing_target().is_some());
    }

    #[test]
    fn the_roster_of_an_empty_store_has_nothing_to_edit() {
        let (_dir, store) = crate::data::fixture::empty_store();
        let mut app = App::new(store, Theme::with_color(false)).unwrap();
        assert!(app.nav().frame().current().is_none());

        app.apply(Action::EnterEditing).unwrap();
        // The one screen with no selection at all: the mode acts on a row, so it
        // cannot be entered, and saying nothing would look like a broken key.
        assert_eq!(app.editing_target(), None);
        assert!(app.flash_message().is_some());
    }

    #[test]
    fn only_a_row_that_offers_a_deletion_answers_the_removal_key() {
        let (_fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();

        // A ticket cannot be deleted at all — the store has no such operation, so
        // resolution is `closed` — which makes the letter as unknown on this row
        // as any key the mode does not admit, and the strip lists nothing.
        assert!(app.editing_hints().is_empty());
        app.apply(Action::Delete).unwrap();
        assert_eq!(app.modal(), None, "a row that offers nothing asked nothing");
        assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION));
        // And no editing action at all disagrees with the strip on this row: the
        // row's own offer is what lists a hint and what answers the key, so a hint
        // that appears without one would be a letter nothing answers.
        hints_and_keys_agree(&mut app);

        // On a label row it is offered, and the strip lists exactly that.
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Ascend).unwrap();
        to_a_label_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        assert_eq!(app.editing_hints(), vec![hint_for(EditingAction::Delete)]);
        hints_and_keys_agree(&mut app);
    }

    /// Walk every row of every level below where the cursor stands, collecting the
    /// editing actions the strip lists on each. Depth-first, and it puts the
    /// browser back where it found it.
    fn collect_offers(app: &mut App, found: &mut Vec<EditingAction>) {
        let depth = app.nav().crumbs().len();
        for index in 0..app.nav().rows().len() {
            app.apply(Action::CursorFirst).unwrap();
            for _ in 0..index {
                app.apply(Action::CursorDown).unwrap();
            }
            app.apply(Action::EnterEditing).unwrap();
            for action in EditingAction::ALL.iter().copied() {
                if app.editing_hints().contains(&hint_for(action)) && !found.contains(&action) {
                    found.push(action);
                }
            }
            app.apply(Action::Unwind).unwrap();
            app.apply(Action::Descend).unwrap();
            if app.nav().crumbs().len() > depth {
                collect_offers(app, found);
                app.apply(Action::Ascend).unwrap();
            }
        }
    }

    #[test]
    fn every_editing_hint_is_one_some_row_actually_offers() {
        let (_fx, mut app) = app();
        let mut offered = Vec::new();
        collect_offers(&mut app, &mut offered);
        // The strip lists the subset of the editing actions the frozen row offers,
        // so an action no row offers is a hint that can never appear: a letter
        // added to the strip without deciding which rows offer it is silently
        // invisible, and that is what this catches. The fixture carries a row of
        // every kind the browser can stand on.
        for action in EditingAction::ALL {
            assert!(
                offered.contains(action),
                "no row offers {action:?}, so its hint can never be shown"
            );
        }
    }

    #[test]
    fn a_removal_asks_before_it_writes_and_a_cancel_changes_nothing() {
        let (fx, mut app) = app();
        to_a_label_row(&mut app);
        let label = row_label(&app);
        app.apply(Action::EnterEditing).unwrap();

        app.apply(Action::Delete).unwrap();
        // The question names the object: the frozen row is dimmed and the members
        // of a collection read alike, so an unnamed question would not say what.
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!(
                "a deletion is gated behind a confirmation: {:?}",
                app.modal()
            )
        };
        assert!(dialog.message().contains(&label), "{dialog:?}");
        // A dialog carries its own answers, and the keyboard is under exactly the
        // set it lists: a destructive question, not something to dismiss.
        assert_eq!(dialog.answers(), Answers::Destructive);
        assert_eq!(app.mode(), Mode::Dialog(Answers::Destructive));
        assert!(fx.epic_labels().contains(&label), "asking wrote something");

        // The answer that is never destructive, here as everywhere else.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert!(
            fx.epic_labels().contains(&label),
            "a cancel wrote something"
        );
        // One layer at a time: the question goes and the mode stays, on the row it
        // was asked about.
        assert_eq!(app.mode(), Mode::Editing);
        assert_eq!(row_label(&app), label);
        assert_eq!(app.flash_message(), None, "nothing happened worth saying");
    }

    #[test]
    fn a_confirmed_removal_writes_says_what_it_did_and_ends_the_session() {
        let (fx, mut app) = app();
        to_a_label_row(&mut app);
        let label = row_label(&app);
        let before = fx.epic_labels();
        app.apply(Action::EnterEditing).unwrap();

        // The letter that asks is the letter that answers: one key for everything
        // destructive, learned once.
        app.apply(Action::Delete).unwrap();
        app.apply(Action::Delete).unwrap();

        assert_eq!(app.modal(), None);
        // The row's own label and no other: an editing session is one edit, and
        // the row under the bar is what it edits.
        let survivors: Vec<String> = before.into_iter().filter(|l| *l != label).collect();
        assert_eq!(fx.epic_labels(), survivors);
        // A successful write ends the session — the mode is one edit long — and
        // says what it did, naming the label, because by the time the notice is
        // read the row it named is gone.
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.mode(), Mode::Browse);
        let notice = app.flash_message().expect("every write says what it did");
        assert!(notice.contains(&label), "{notice:?}");
        // The store is re-read with it: a row that no longer exists must not stay
        // on screen for the next keypress to act on.
        assert!(app.nav().rows().iter().all(|r| r.label != label));
    }

    #[test]
    fn a_refused_removal_carries_the_stores_own_words_and_keeps_the_session_on() {
        let (fx, mut app) = app();
        to_a_label_row(&mut app);
        let target = app.nav().frame().current().unwrap().selection.clone();
        app.apply(Action::EnterEditing).unwrap();

        // Only the store can judge a write, so the browser offers the action and
        // shows what comes back: here the entity goes between offer and answer.
        fx.remove_the_epics_file();
        app.apply(Action::Delete).unwrap();
        app.apply(Action::Delete).unwrap();

        // Verbatim, so the browser and the CLI teach the same rule in the same
        // words: compared against the message the seam itself produces, never a
        // string spelled out here, which is what a reworded refusal would pass.
        let refusal = data::perform(&fx.store, &data::Write::RemoveLabel(target))
            .expect_err("the store refuses a label removal on a missing entity")
            .to_string();
        assert_eq!(app.modal(), Some(&Modal::Dialog(Dialog::refusal(refusal))));
        // Nothing is at stake in reading it, so it is dismissed rather than
        // answered — and a failure is never a transient notice.
        assert_eq!(app.mode(), Mode::Dialog(Answers::Acknowledge));
        assert_eq!(app.flash_message(), None);

        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        // Only a successful write ends the session, so dismissing lands back in it.
        assert!(app.editing_target().is_some());
        assert_eq!(app.mode(), Mode::Editing);
    }

    #[test]
    fn a_question_is_answered_rather_than_escaped_past() {
        let (fx, mut app) = app();
        to_a_label_row(&mut app);
        let label = row_label(&app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Delete).unwrap();
        let cursor = app.nav().cursor();
        let asked = app.modal().cloned().expect("a question is open");

        // A dialog admits its listed answers and nothing else. Quitting least of
        // all: the browser must not exit with an unanswered question on screen.
        // Not every one of these can arrive from a key while a dialog is open, but
        // a mouse wheel reaches the state machine without passing the key map.
        for action in [
            Action::Quit,
            Action::CursorDown,
            Action::Descend,
            Action::Ascend,
            Action::Reload,
            Action::ToggleHelp,
            Action::ToggleZoom,
            Action::EnterEditing,
        ] {
            assert!(!app.apply(action).unwrap(), "{action:?} left the browser");
            // The same question, still unanswered: neither dismissed, nor replaced
            // by the report of something one of these got through and did.
            assert_eq!(
                app.modal(),
                Some(&asked),
                "{action:?} got past the question"
            );
            assert_eq!(app.nav().cursor(), cursor, "{action:?} moved the cursor");
            assert!(!app.zoomed(), "{action:?} rearranged the screen");
        }
        assert!(fx.epic_labels().contains(&label), "something was written");
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
