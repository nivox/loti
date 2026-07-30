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

use crate::action::{Action, AnswerWords, Answers, EditingAction, Mode};
use crate::data::{self, Collection, Level, Selection};
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
/// See [`CONFIRM_TITLE`].
const REQUIRED_TITLE: &str = " a required field is empty ";
/// See [`CONFIRM_TITLE`]. The browser hands the terminal over for an external
/// editor, so an editor that will not run is the browser's failure to report and
/// not the store's.
const EDITOR_TITLE: &str = " the editor could not run ";

/// What a label field is called wherever it has to be named: on the surface that
/// fills it in, and in the warning that says it is empty.
const LABEL_FIELD: &str = "label";

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
    ///
    /// Behind a pointer because a dialog carries what its answer writes and what
    /// its answers are called, which is far more than the overlay carries, and only
    /// one modal is ever open: the indirection costs a pointer hop on a keypress
    /// and saves every browser state from carrying a dialog's worth of bytes.
    Dialog(Box<Dialog>),
}

/// What an editing action opens on the frozen row: something to answer, or
/// something to fill in.
///
/// The shape of the input decides which: a question with no text to write is a
/// dialog to answer, and anything the reader has to type is a surface. Both are
/// what one row offers, so the hint strip and the key consult the same answer.
enum Offer {
    /// A question, answered where it stands.
    Ask(Dialog),
    /// A surface to fill in and accept.
    Fill(Surface),
}

/// A dialog: what it says, how it may be answered, what its answers are called,
/// and what each of them performs.
///
/// Invariant: a dialog carries all of that itself, so the state machine that
/// answers one names no operation, no answer set and no wording of its own —
/// which is what makes a further kind of dialog a value built here rather than
/// another branch everywhere a dialog is routed or drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    title: &'static str,
    message: String,
    answers: Answers,
    /// The affirmative answer, and `None` for a dialog that only reports: such a
    /// dialog has a way out and no answer, so nothing it is shown for can be
    /// acted on by mistake.
    affirmative: Option<Answer>,
    /// The way out. Every dialog has one — dismissal is never refused — so this is
    /// never absent.
    dismissal: Dismissal,
}

/// A dialog's affirmative answer: what it is called, and what it performs.
///
/// The word travels with the answer because the answer set decides only which
/// keys are bound: the destructive letter removes a label on one dialog and
/// throws a buffer away on another, so a set of words fixed per key could not say
/// both.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Answer {
    word: &'static str,
    performs: Performs,
}

/// A dialog's way out: what it is called, and what the reader lands in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Dismissal {
    word: &'static str,
    /// What dismissal performs besides closing the dialog, and `None` where
    /// landing back exactly where the reader was is the whole of it.
    performs: Option<OnDismissal>,
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
    /// Throw the open surface away, the text in it included. Nothing reaches the
    /// store, which is why an answer that writes nothing is still an answer: the
    /// text is the only copy of what the reader typed.
    Discard,
}

/// What dismissing a dialog performs, over and above the dialog going away.
///
/// Invariant: dismissal is unconditional — a dialog can always be got out of — so
/// nothing here is a condition on getting out; it is only where the reader lands
/// once they have. This is the mirror of what an affirmative answer performs,
/// because a dialog that merely reports still has somewhere to put the reader
/// afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnDismissal {
    /// Land back in the field the dialog named, so warning, dismissal and typing
    /// the answer are a straight line with no hunting for which field was meant.
    Focus(usize),
}

impl Dialog {
    /// A destructive question: the word its answer goes by, what that answer
    /// performs, and the word for getting out instead.
    ///
    /// Every deletion is gated behind one of these, and so is throwing a buffer
    /// away: a label is trivially re-addable and the gate buys little there, but
    /// one rule a reader can predict beats a per-action judgement they have to
    /// remember.
    fn confirm(
        question: String,
        word: &'static str,
        performs: Performs,
        dismissal: &'static str,
    ) -> Self {
        Self {
            title: CONFIRM_TITLE,
            message: question,
            answers: Answers::Destructive,
            affirmative: Some(Answer { word, performs }),
            dismissal: Dismissal {
                word: dismissal,
                performs: None,
            },
        }
    }

    /// The warning the way out raises on a buffer with typing in it. It names the
    /// field, because the float covers the frozen row and a buffer carries no
    /// label near it.
    fn discard(field: &str) -> Self {
        Self::confirm(
            format!("Discard changes to {field}?"),
            "discard",
            Performs::Discard,
            "keep editing",
        )
    }

    /// A report with nothing to answer: the reader is being told, not asked, so
    /// the only way out is the way out, and it may carry where to land.
    fn report(title: &'static str, message: String, dismissal: Dismissal) -> Self {
        Self {
            title,
            message,
            answers: Answers::Acknowledge,
            affirmative: None,
            dismissal,
        }
    }

    /// A refusal, in the store's own words.
    fn refusal(message: String) -> Self {
        Self::report(
            REFUSAL_TITLE,
            message,
            Dismissal {
                word: "dismiss",
                performs: None,
            },
        )
    }

    /// The warning a surface accepted with an empty required field raises instead
    /// of writing. It names the field, and dismissing it lands there: nothing is
    /// being judged about the value, only that there is none to send.
    fn required(field: &str, index: usize) -> Self {
        Self::report(
            REQUIRED_TITLE,
            format!("{field} is required."),
            Dismissal {
                word: "back to the field",
                performs: Some(OnDismissal::Focus(index)),
            },
        )
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

    /// The set of answers it admits, which is also the set of keys it lists.
    pub fn answers(&self) -> Answers {
        self.answers
    }

    /// What this dialog calls its own answers, for the key map to pair with the
    /// letters. The affirmative word is present exactly when there is an answer to
    /// word, so a dialog cannot list a key it does not admit.
    pub fn words(&self) -> AnswerWords {
        AnswerWords {
            affirmative: self.affirmative.as_ref().map(|answer| answer.word),
            dismissal: self.dismissal.word,
        }
    }
}

/// One text field of an editing surface.
///
/// Invariant: the field holds one line. Whatever arrives with line breaks in it —
/// the external editor's result — has them dropped rather than turned into
/// spaces, because a space is content the reader did not type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// What the field is called wherever it has to be named: on the surface, and
    /// in a warning about it. A warning is raised over the surface and the frozen
    /// row is covered, so a warning that named no field would not say which.
    label: &'static str,
    value: String,
    /// Where the next character lands, counted in characters and not bytes, so a
    /// multi-byte character is never split.
    cursor: usize,
    /// Whether the store cannot be given this surface with the field left empty.
    required: bool,
    /// Whether a content-mutating keystroke has landed here.
    ///
    /// Invariant: a flag, never a comparison against what the field started from.
    /// It is sticky — typing a character and deleting it again leaves the field
    /// dirty — and cursor motion never sets it. So the way out warns about a field
    /// that would lose nothing, which is accepted deliberately: a spurious warning
    /// is cheap, and a flag costs no per-keystroke compare of a whole body against
    /// its original.
    dirty: bool,
}

impl Field {
    /// An empty field, which is where every surface starts: nothing the browser
    /// puts there itself could be text the reader meant to write.
    fn new(label: &'static str, required: bool) -> Self {
        Self {
            label,
            value: String::new(),
            cursor: 0,
            required,
            dirty: false,
        }
    }

    /// What the field is called.
    pub fn label(&self) -> &'static str {
        self.label
    }

    /// What it holds.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Where the cursor sits, in characters from the start.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether anything has been typed into it. See [`Field::dirty`].
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Whether a required field has nothing in it to send.
    ///
    /// A field holding only whitespace counts as empty: the reader cannot tell it
    /// from a blank one on screen, so warning is more honest than writing
    /// something invisible. What makes a *non-blank* value acceptable is the
    /// store's rule, and the browser reimplements none of those.
    fn unfilled(&self) -> bool {
        self.required && self.value.trim().is_empty()
    }

    /// Apply a key's intent to the field.
    ///
    /// Invariant: every intent that can change the content sets the dirty flag and
    /// no motion ever does — including an intent that happened to change nothing,
    /// a deletion with nothing left to delete among them, because dirty is what was
    /// *pressed* and not what differs.
    fn apply(&mut self, action: Action) {
        match action {
            Action::Insert(c) => {
                let at = self.byte_at(self.cursor);
                self.value.insert(at, c);
                self.cursor += 1;
                self.dirty = true;
            }
            Action::DeleteBefore => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    let at = self.byte_at(self.cursor);
                    self.value.remove(at);
                }
                self.dirty = true;
            }
            Action::DeleteAfter => {
                if self.cursor < self.len() {
                    let at = self.byte_at(self.cursor);
                    self.value.remove(at);
                }
                self.dirty = true;
            }
            Action::MoveLeft => self.cursor = self.cursor.saturating_sub(1),
            Action::MoveRight => self.cursor = (self.cursor + 1).min(self.len()),
            Action::MoveToStart => self.cursor = 0,
            Action::MoveToEnd => self.cursor = self.len(),
            // Everything else is the surface's business or nobody's: a key the
            // field does not answer must not silently change what it holds.
            _ => {}
        }
    }

    /// Take an external editor's result, which counts as content the reader typed:
    /// the way out warns about it exactly as it does about typing.
    ///
    /// The line breaks an editor leaves behind are dropped, because the field holds
    /// one line — see [`Field`].
    fn replace(&mut self, text: &str) {
        self.value = text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        self.cursor = self.len();
        self.dirty = true;
    }

    /// How many characters the field holds, which is where the cursor may go up to.
    fn len(&self) -> usize {
        self.value.chars().count()
    }

    /// The byte offset of a character offset, so an insertion or a removal never
    /// lands inside a multi-byte character.
    fn byte_at(&self, cursor: usize) -> usize {
        self.value
            .char_indices()
            .nth(cursor)
            .map(|(at, _)| at)
            .unwrap_or(self.value.len())
    }
}

/// An open editing surface: the fields the reader fills in, and what accepting it
/// writes.
///
/// Invariant: a surface is open only while editing mode is on, and a dialog about
/// it is laid over it rather than replacing it — so answering or dismissing one
/// lands back in the buffer it was raised about, with the text intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    /// What the float is titled: the action and the row it acts on, since the
    /// float covers the row it was opened from.
    title: String,
    fields: Vec<Field>,
    /// Which field the keyboard is in.
    focus: usize,
    commit: Commit,
}

/// What accepting a surface writes.
///
/// Invariant: named when the surface opens and built from the fields at the moment
/// it is accepted, so the write and the notice that reports it are one decision
/// and can never come to name different things.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Commit {
    /// Put the label the field holds on the set the frozen row names.
    AddLabel(Selection),
}

impl Surface {
    /// The surface that adds one label: a single short line, so it is a float and
    /// not the preview pane — keeping the row visible buys nothing for a field this
    /// size, and the pane is where the long-form text goes.
    fn add_label(set: Selection, container: String) -> Self {
        Self {
            title: format!(" new label on {container} "),
            fields: vec![Field::new(LABEL_FIELD, true)],
            focus: 0,
            commit: Commit::AddLabel(set),
        }
    }

    /// What the float is titled.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Its fields, in the order they are filled in.
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// Which of them the keyboard is in.
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// The field a warning about lost text names: the first one with typing in it,
    /// and `None` for a surface nothing has been typed into — which is what makes
    /// the way out ask on one and not on the other.
    fn dirtied(&self) -> Option<&Field> {
        self.fields.iter().find(|field| field.dirty)
    }

    /// The first required field left empty, which is the one an accept must warn
    /// about and land the reader back in.
    fn unfilled(&self) -> Option<usize> {
        self.fields.iter().position(Field::unfilled)
    }

    /// The field taking keystrokes.
    fn focused_mut(&mut self) -> &mut Field {
        // The focus is only ever set to a field this surface carries, so there is
        // always one to type into.
        &mut self.fields[self.focus]
    }

    /// The write accepting the surface performs, and what the notice says once it
    /// is committed.
    fn write(&self) -> (data::Write, String) {
        match &self.commit {
            Commit::AddLabel(set) => {
                let label = self.fields[0].value.clone();
                (
                    data::Write::AddLabel(set.clone(), label.clone()),
                    // The notice names the label, because by the time it is read
                    // the surface that held it is gone.
                    format!("label {label} added"),
                )
            }
        }
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
    /// The open editing surface, if any. Only ever open while editing mode is on:
    /// it is filled in for the frozen row, and a successful save closes both.
    surface: Option<Surface>,
    /// The text an external editor has been asked to take, until the loop that
    /// owns the terminal takes it.
    ///
    /// The browser cannot run an editor itself: the editor needs the screen, the
    /// alternate screen gone, raw mode off and mouse capture off, and none of those
    /// belong to the state machine. So the handoff is left here to be picked up.
    editor_handoff: Option<String>,
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
            surface: None,
            editor_handoff: None,
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

    /// The open editing surface, if any.
    pub fn surface(&self) -> Option<&Surface> {
        self.surface.as_ref()
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
            // An open surface takes every key, so it outranks the mode that opened
            // it: while a field is being typed into, the mode's own letters are
            // characters.
            Some(Modal::Help) | None => match (self.surface.is_some(), self.editing.is_some()) {
                (true, _) => Mode::Surface,
                (false, true) => Mode::Editing,
                (false, false) => Mode::Browse,
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

    /// What an editing action opens on the frozen row, or `None` where the row does
    /// not offer that action.
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
    fn offer(&self, action: EditingAction) -> Option<Offer> {
        let target = self.editing.as_ref()?;
        match action {
            EditingAction::Add => match target {
                // Creation acts on the container row the cursor stands on, so a
                // label is added from the label set's own row and nowhere else.
                Selection::Collection(container, Collection::Labels) => Some(Offer::Fill(
                    Surface::add_label(target.clone(), container.selection().reference()),
                )),
                _ => None,
            },
            EditingAction::Delete => match target {
                // A label set has no rename, so a label is only ever removed.
                Selection::Label(_, label) => Some(Offer::Ask(Dialog::confirm(
                    format!("Remove label {label}?"),
                    "remove",
                    Performs::Write {
                        write: data::Write::RemoveLabel(target.clone()),
                        done: format!("label {label} removed"),
                    },
                    "cancel",
                ))),
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
        // An open surface is the layer under an overlay and above the mode that
        // opened it: every key belongs to the field while it is open.
        if self.surface.is_some() {
            self.apply_to_surface(action)?;
            return Ok(false);
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

            // The letters of editing mode's actions, and the keys of an open
            // surface, are bound inside those layers only, so nothing carries
            // these intents while browsing.
            Action::Add
            | Action::Delete
            | Action::Accept
            | Action::ExternalEditor
            | Action::Insert(_)
            | Action::DeleteBefore
            | Action::DeleteAfter
            | Action::MoveLeft
            | Action::MoveRight
            | Action::MoveToStart
            | Action::MoveToEnd => {}

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
            Action::Unwind => self.dismiss(),
            _ => {}
        }
        Ok(false)
    }

    /// Get out of the open dialog, landing wherever it says the reader belongs.
    ///
    /// Dismissal is unconditional: the dialog always goes. What it may carry is
    /// only where the reader lands — a warning about an empty field puts them back
    /// in that field, so answering it is typing rather than hunting.
    fn dismiss(&mut self) {
        let performs = match &self.modal {
            Some(Modal::Dialog(dialog)) => dialog.dismissal.performs,
            Some(Modal::Help) | None => None,
        };
        self.modal = None;
        match performs {
            Some(OnDismissal::Focus(field)) => {
                if let Some(surface) = &mut self.surface {
                    // The buffer was never what the warning was about, so it is
                    // still open with its text: only the focus moves.
                    surface.focus = field.min(surface.fields.len().saturating_sub(1));
                }
            }
            None => {}
        }
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
        let Some(answer) = dialog.affirmative.clone() else {
            return Ok(());
        };
        self.modal = None;
        match answer.performs {
            Performs::Write { write, done } => self.commit(&write, done)?,
            // Nothing reaches the store, and the mode stays on its frozen row: the
            // way out unwinds one layer at a time, and the surface is the layer
            // that was asked about.
            Performs::Discard => self.surface = None,
        }
        Ok(())
    }

    /// Write, and report what happened.
    ///
    /// A successful write ends the editing session, the surface with it, and says
    /// what it did. A refused one keeps everything: the surface stays open with its
    /// text, so the reader can fix it or carry it out through the external editor —
    /// only a successful write ends the session.
    fn commit(&mut self, write: &data::Write, done: String) -> Result<()> {
        match data::perform(&self.store, write) {
            Ok(()) => {
                self.surface = None;
                self.editing = None;
                self.reload()?;
                self.flash(done);
            }
            // The store's own words, so the browser and the CLI teach the same
            // rule in the same words and the browser cannot go stale when a
            // store rule gains a nuance.
            Err(e) => self.modal = Some(Modal::Dialog(Box::new(Dialog::refusal(e.to_string())))),
        }
        Ok(())
    }

    /// Carry out an intent while a surface is open.
    ///
    /// Every key belongs to the field except the surface's own few: accept, the
    /// external editor, help, and the way out. There is no unknown-key notice here
    /// — in a field an unbound key is simply not a character, and the mode's notice
    /// belongs to the layer where letters are actions.
    fn apply_to_surface(&mut self, action: Action) -> Result<()> {
        match action {
            // The text is the only copy of what the reader wrote, so the way out of
            // a buffer with typing in it asks first; an untouched one is not worth
            // a question and goes at once.
            Action::Unwind => match self.surface.as_ref().and_then(Surface::dirtied) {
                Some(field) => {
                    self.modal = Some(Modal::Dialog(Box::new(Dialog::discard(field.label))));
                }
                None => self.surface = None,
            },
            Action::Accept => self.accept()?,
            Action::ExternalEditor => {
                if let Some(surface) = &self.surface {
                    self.editor_handoff = Some(surface.fields[surface.focus].value.clone());
                }
            }
            // The key list is reachable from inside a field, which is what the help
            // key that survives a text field is for: the list is a layer above the
            // surface, so closing it leaves the buffer exactly as it was.
            Action::ToggleHelp => self.modal = Some(Modal::Help),
            _ => {
                if let Some(surface) = &mut self.surface {
                    surface.focused_mut().apply(action);
                }
            }
        }
        Ok(())
    }

    /// Accept the open surface: check the store has something to be given, then
    /// write.
    ///
    /// The check is not a store rule reimplemented — what makes a value acceptable
    /// is the store's judgement and its refusal is shown verbatim — it is the
    /// browser refusing to send a field the reader never filled in, and saying
    /// which field that is rather than which rule was broken.
    fn accept(&mut self) -> Result<()> {
        let Some(surface) = &self.surface else {
            return Ok(());
        };
        if let Some(index) = surface.unfilled() {
            let dialog = Dialog::required(surface.fields[index].label, index);
            self.modal = Some(Modal::Dialog(Box::new(dialog)));
            return Ok(());
        }
        let (write, done) = surface.write();
        self.commit(&write, done)
    }

    /// The text an external editor has been asked to take, clearing the request.
    ///
    /// The loop that owns the terminal is the only caller: an editor needs the
    /// alternate screen gone, raw mode off and mouse capture off, and none of those
    /// are the state machine's to give.
    pub fn take_editor_handoff(&mut self) -> Option<String> {
        self.editor_handoff.take()
    }

    /// Take an external editor's result back into the field it came from.
    ///
    /// It counts as content the reader wrote, so the field is dirty afterwards and
    /// the way out warns about it exactly as it does about typing.
    pub fn editor_returned(&mut self, text: &str) {
        if let Some(surface) = &mut self.surface {
            surface.focused_mut().replace(text);
        }
    }

    /// Report that the external editor could not run, keeping the buffer.
    ///
    /// A failed and costly thing is a dialog rather than a transient notice, and
    /// the message is the failure's own: the browser cannot say more about somebody
    /// else's editor than the system already did.
    pub fn editor_failed(&mut self, message: String) {
        self.modal = Some(Modal::Dialog(Box::new(Dialog::report(
            EDITOR_TITLE,
            message,
            Dismissal {
                word: "back to the field",
                performs: None,
            },
        ))));
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
                Some(Offer::Ask(dialog)) => self.modal = Some(Modal::Dialog(Box::new(dialog))),
                Some(Offer::Fill(surface)) => self.surface = Some(surface),
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

    /// Walk back out to the epic roster, so a test that has been somewhere already
    /// can still say where it goes next from the top.
    fn to_the_roster(app: &mut App) {
        while app.nav().crumbs().len() > 1 {
            app.apply(Action::Ascend).unwrap();
        }
    }

    /// Stand on the epic's own `labels` row, which is where an addition is
    /// offered: creation acts on the container row the cursor stands on.
    fn to_the_labels_row(app: &mut App) {
        to_the_roster(app);
        app.apply(Action::Descend).unwrap(); // into the epic
        to_row(
            app,
            |kind| matches!(kind, RowKind::Collection(c) if c.name() == "labels"),
        );
    }

    /// Stand on the first label of the epic's own labels level, which is where a
    /// removal is offered.
    fn to_a_label_row(app: &mut App) {
        to_the_labels_row(app);
        app.apply(Action::Descend).unwrap();
    }

    /// Open the label surface, the way a reader does: freeze the label set's row
    /// and press the letter that adds a member to it.
    fn open_the_label_surface(app: &mut App) {
        to_the_labels_row(app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Add).unwrap();
        assert!(app.surface().is_some(), "the add key opened no surface");
    }

    /// Type into the open field, one keystroke per character, as a reader does.
    fn type_into(app: &mut App, text: &str) {
        for c in text.chars() {
            app.apply(Action::Insert(c)).unwrap();
        }
    }

    /// What the open surface's focused field holds.
    fn field_value(app: &App) -> String {
        let surface = app.surface().expect("a surface is open");
        surface.fields()[surface.focus()].value().to_string()
    }

    /// The answers the open dialog lists, as the float shows them: the key map's
    /// letters carrying the dialog's own words.
    fn listed_answers(app: &App) -> Vec<String> {
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("no dialog is open: {:?}", app.modal())
        };
        keymap::dialog_answers(dialog.answers(), dialog.words())
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
                        app.modal().is_some() || app.surface().is_some(),
                        "{action:?} is hinted but the key opened nothing"
                    );
                    // Back out of what it opened, leaving the mode standing. An
                    // untouched surface has nothing to lose, so it goes without a
                    // question.
                    app.apply(Action::Unwind).unwrap();
                    assert!(
                        app.modal().is_none() && app.surface().is_none(),
                        "{action:?} could not be backed out of"
                    );
                }
                false => {
                    assert_eq!(
                        app.modal(),
                        None,
                        "{action:?} is not hinted but the key acted"
                    );
                    assert!(
                        app.surface().is_none(),
                        "{action:?} is not hinted but the key opened a surface"
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
        assert_eq!(
            app.modal(),
            Some(&Modal::Dialog(Box::new(Dialog::refusal(refusal))))
        );
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
    fn the_add_key_opens_one_short_field_on_the_label_set_and_nowhere_else() {
        let (fx, mut app) = app();
        to_the_labels_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        // The row offers the addition, and the strip lists exactly what it offers.
        assert_eq!(app.editing_hints(), vec![hint_for(EditingAction::Add)]);
        hints_and_keys_agree(&mut app);

        app.apply(Action::Add).unwrap();
        let surface = app.surface().expect("the letter opened a surface");
        // One short field, empty and untouched: nothing the browser put there
        // itself could be text the reader meant to write.
        assert_eq!(surface.fields().len(), 1);
        assert_eq!(surface.focus(), 0);
        let field = &surface.fields()[0];
        assert_eq!(field.value(), "");
        assert!(!field.is_dirty());
        // The float says what is being added and to what, since it covers the row
        // it was opened from.
        assert!(surface.title().contains(&fx.epic), "{:?}", surface.title());
        // Every key now belongs to the field: the mode the keyboard is under is the
        // only bridge from this state to the key table.
        assert_eq!(app.mode(), Mode::Surface);
        assert!(fx.epic_labels().iter().all(|l| !l.is_empty()));

        // A row that is not a label set offers no addition: this surface writes a
        // label, so the row that opens it can only be the row a label belongs to.
        // Every other collection's member is a different shape of input, owned by
        // whichever surface writes it.
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();
        to_a_label_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Add).unwrap();
        assert!(app.surface().is_none(), "a label row offered an addition");
        assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION));
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Ascend).unwrap();

        for other in ["comments", "blocked-by", "assets"] {
            to_the_roster(&mut app);
            app.apply(Action::Descend).unwrap(); // into the epic
                                                 // `blocked-by` is a node's collection only, so it is reached one level
                                                 // further in; the epic's own level carries the other two.
            if other == "blocked-by" {
                to_work_row(&mut app);
                app.apply(Action::Descend).unwrap();
            }
            to_row(
                &mut app,
                |kind| matches!(kind, RowKind::Collection(c) if c.name() == other),
            );
            app.apply(Action::EnterEditing).unwrap();
            app.apply(Action::Add).unwrap();
            assert!(
                app.surface().is_none(),
                "the {other} row opened the label surface"
            );
            assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION), "{other}");
            assert!(app.editing_hints().is_empty(), "{other}");
            app.apply(Action::Unwind).unwrap();
        }
    }

    #[test]
    fn a_filled_field_is_written_says_what_it_did_and_ends_the_session() {
        let (fx, mut app) = app();
        let before = fx.epic_labels();
        open_the_label_surface(&mut app);
        type_into(&mut app, "a new label");

        app.apply(Action::Accept).unwrap();

        // Written once, exactly what was typed, and nothing else touched.
        let mut expected = before;
        expected.push("a new label".to_string());
        assert_eq!(fx.epic_labels(), expected);
        // A successful write ends the session, surface and all — the mode is one
        // edit long — and says what it did, naming the label.
        assert!(app.surface().is_none());
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.mode(), Mode::Browse);
        assert_eq!(app.modal(), None);
        let notice = app.flash_message().expect("every write says what it did");
        assert!(notice.contains("a new label"), "{notice:?}");
        // The store was re-read: the level under the row the reader is standing on
        // now holds one more member than it did.
        let row = app.nav().frame().current().expect("a highlighted row");
        assert_eq!(row.children, expected.len());
    }

    #[test]
    fn an_empty_required_field_warns_naming_it_writes_nothing_and_lands_back_in_it() {
        let (fx, mut app) = app();
        let before = fx.epic_labels();
        open_the_label_surface(&mut app);

        // Accepting an empty required field warns instead of writing. It is not a
        // store rule reimplemented: there is nothing to send, and the warning names
        // which field, because the float names no columns.
        app.apply(Action::Accept).unwrap();
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!(
                "an empty required field wrote or said nothing: {:?}",
                app.modal()
            )
        };
        assert!(dialog.message().contains(LABEL_FIELD), "{dialog:?}");
        assert!(dialog.message().contains("required"), "{dialog:?}");
        // Nothing is being asked — there is nothing to go ahead with — so it is
        // dismissed rather than answered.
        assert_eq!(dialog.answers(), Answers::Acknowledge);
        assert_eq!(fx.epic_labels(), before, "the warning wrote something");
        // A failure the reader must act on is a dialog and never a notice.
        assert_eq!(app.flash_message(), None);
        // What dismissing does is on the dialog itself, so the float says it.
        assert!(
            listed_answers(&app).iter().any(|a| a.contains("field")),
            "{:?}",
            listed_answers(&app)
        );

        // The dialog carries where dismissal lands, and it is the field it named:
        // that is what makes "warn, dismiss, type" a straight line rather than a
        // hunt, and it is the mirror of what an affirmative answer carries.
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            unreachable!("the warning is open")
        };
        assert_eq!(
            dialog.dismissal.performs,
            Some(OnDismissal::Focus(0)),
            "the warning does not say where acknowledging lands"
        );

        // Acknowledging lands back in the offending field: the buffer was never
        // what the warning was about, so typing the answer is a straight line.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert_eq!(app.mode(), Mode::Surface);
        assert_eq!(app.surface().map(Surface::focus), Some(0));
        type_into(&mut app, "ui-2");
        assert_eq!(field_value(&app), "ui-2");

        // A field holding only whitespace is empty too: a reader cannot tell it
        // from a blank one, so writing something invisible would be worse.
        app.apply(Action::MoveToStart).unwrap();
        for _ in 0.."ui-2".len() {
            app.apply(Action::DeleteAfter).unwrap();
        }
        type_into(&mut app, "   ");
        app.apply(Action::Accept).unwrap();
        assert!(matches!(app.modal(), Some(Modal::Dialog(_))));
        assert_eq!(fx.epic_labels(), before);
    }

    #[test]
    fn the_way_out_of_a_clean_field_cancels_at_once_and_of_a_dirty_one_asks_first() {
        let (fx, mut app) = app();
        let before = fx.epic_labels();
        open_the_label_surface(&mut app);

        // An untouched surface has nothing to lose, so the way out is silent — and
        // it unwinds one layer: the mode stays on its frozen row.
        app.apply(Action::Unwind).unwrap();
        assert!(app.surface().is_none());
        assert_eq!(app.modal(), None);
        assert_eq!(app.mode(), Mode::Editing);
        assert_eq!(app.flash_message(), None, "nothing happened worth saying");

        // With typing in it, the way out asks: the text is the only copy of what
        // the reader wrote.
        app.apply(Action::Add).unwrap();
        type_into(&mut app, "half a thought");
        app.apply(Action::Unwind).unwrap();
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("a dirty field was thrown away unasked: {:?}", app.modal())
        };
        assert!(dialog.message().contains("Discard"), "{dialog:?}");
        assert!(dialog.message().contains(LABEL_FIELD), "{dialog:?}");
        assert_eq!(dialog.answers(), Answers::Destructive);

        // The answer that is never destructive lands back in the buffer, with the
        // text exactly as it was: a warning is not a way of losing it.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert_eq!(field_value(&app), "half a thought");
        assert_eq!(app.mode(), Mode::Surface);

        // The destructive letter throws the buffer away, and only the buffer: the
        // mode stays on the row, and nothing reached the store.
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Delete).unwrap();
        assert!(app.surface().is_none());
        assert_eq!(app.modal(), None);
        assert_eq!(app.mode(), Mode::Editing);
        assert_eq!(fx.epic_labels(), before, "discarding wrote something");
    }

    #[test]
    fn dirty_is_a_sticky_flag_and_never_a_comparison() {
        let (_fx, mut app) = app();

        // Cursor motion never dirties a field, whatever it does to the cursor:
        // moving is not writing, so the way out stays silent.
        open_the_label_surface(&mut app);
        for action in [
            Action::MoveRight,
            Action::MoveLeft,
            Action::MoveToEnd,
            Action::MoveToStart,
        ] {
            app.apply(action).unwrap();
            assert!(
                !app.surface().unwrap().fields()[0].is_dirty(),
                "{action:?} dirtied a field it did not write to"
            );
        }
        // And a cursor cannot be moved off the end of what the field holds: what is
        // to the right of the last character is not a position to type at.
        for _ in 0..3 {
            app.apply(Action::MoveRight).unwrap();
        }
        assert_eq!(app.surface().unwrap().fields()[0].cursor(), 0);
        type_into(&mut app, "ab");
        for _ in 0..3 {
            app.apply(Action::MoveRight).unwrap();
        }
        assert_eq!(app.surface().unwrap().fields()[0].cursor(), 2);
        app.apply(Action::MoveToStart).unwrap();
        for _ in 0..3 {
            app.apply(Action::MoveLeft).unwrap();
        }
        assert_eq!(app.surface().unwrap().fields()[0].cursor(), 0);

        // Back to a field nothing was typed into, which is what the way out is
        // silent about.
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Delete).unwrap();
        app.apply(Action::Add).unwrap();
        app.apply(Action::Unwind).unwrap();
        assert!(app.surface().is_none(), "a clean field asked a question");

        // Typing and then deleting it all again leaves the field dirty: the flag is
        // sticky and is never compared against what the field started from. So the
        // way out warns about a field that would lose nothing, which is accepted —
        // a spurious warning is cheap.
        app.apply(Action::Add).unwrap();
        type_into(&mut app, "x");
        app.apply(Action::DeleteBefore).unwrap();
        assert_eq!(field_value(&app), "");
        assert!(app.surface().unwrap().fields()[0].is_dirty());
        app.apply(Action::Unwind).unwrap();
        assert!(
            matches!(app.modal(), Some(Modal::Dialog(_))),
            "a deletion cleared the flag typing set"
        );
        app.apply(Action::Unwind).unwrap();

        // And a deleting keystroke with nothing to delete dirties it too: what is
        // recorded is that the reader pressed a key that writes, not that anything
        // came out different.
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Delete).unwrap();
        app.apply(Action::Add).unwrap();
        app.apply(Action::DeleteBefore).unwrap();
        assert!(app.surface().unwrap().fields()[0].is_dirty());
    }

    #[test]
    fn two_dialogs_share_the_destructive_letter_and_word_it_for_themselves() {
        let (_fx, mut app) = app();
        // The letter that removes a label and the letter that throws a buffer away
        // are one key, learned once — and mean different things, so the words are
        // the dialog's own rather than the answer set's.
        to_a_label_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Delete).unwrap();
        let removing = listed_answers(&app);
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Ascend).unwrap();

        open_the_label_surface(&mut app);
        type_into(&mut app, "x");
        app.apply(Action::Unwind).unwrap();
        let discarding = listed_answers(&app);

        let letters = |answers: &[String]| -> Vec<String> {
            answers
                .iter()
                .map(|a| a.split_whitespace().next().unwrap().to_string())
                .collect()
        };
        assert_eq!(letters(&removing), letters(&discarding));
        assert_ne!(removing, discarding);
        assert!(
            discarding.iter().any(|a| a.contains("discard")),
            "{discarding:?}"
        );
        assert!(
            removing.iter().any(|a| a.contains("remove")),
            "{removing:?}"
        );
    }

    #[test]
    fn the_external_editor_is_handed_the_field_and_its_result_comes_back_as_one_line() {
        let (_fx, mut app) = app();
        open_the_label_surface(&mut app);
        type_into(&mut app, "a start");

        // The browser cannot run an editor itself — the editor needs the terminal —
        // so the field's content is left for the loop that owns it to hand over.
        app.apply(Action::ExternalEditor).unwrap();
        assert_eq!(app.take_editor_handoff().as_deref(), Some("a start"));
        // Taken once: a request honoured twice would open two editors.
        assert_eq!(app.take_editor_handoff(), None);

        // The field holds one line, so the editor's line breaks are dropped rather
        // than turned into spaces — a space is content the reader did not write.
        app.editor_returned("first\nsecond\r\n");
        assert_eq!(field_value(&app), "firstsecond");
        // Coming back from the editor counts as writing, so the way out warns about
        // it exactly as it does about typing.
        assert!(app.surface().unwrap().fields()[0].is_dirty());
        // The cursor is left where the text ends, which is where typing continues.
        assert_eq!(
            app.surface().unwrap().fields()[0].cursor(),
            "firstsecond".chars().count()
        );

        // An editor that cannot be run is a failure, so it is a dialog rather than a
        // notice — and the buffer is kept: it is still the reader's only copy.
        app.editor_failed("no editor is set".into());
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("a failed editor said nothing: {:?}", app.modal())
        };
        assert!(dialog.message().contains("no editor is set"), "{dialog:?}");
        assert_eq!(dialog.answers(), Answers::Acknowledge);
        assert_eq!(app.flash_message(), None);
        app.apply(Action::Unwind).unwrap();
        assert_eq!(field_value(&app), "firstsecond");
    }

    #[test]
    fn a_field_nothing_was_typed_into_is_dirty_once_the_editor_has_written_it() {
        let (fx, mut app) = app();
        let before = fx.epic_labels();
        open_the_label_surface(&mut app);

        // Straight out to the editor from an untouched field, so the return is the
        // only thing that can have dirtied it — and it is the one path where the
        // reader's whole text exists nowhere else.
        app.apply(Action::ExternalEditor).unwrap();
        assert_eq!(app.take_editor_handoff().as_deref(), Some(""));
        app.editor_returned("written in the editor");
        assert!(app.surface().unwrap().fields()[0].is_dirty());

        // So the way out asks instead of throwing it away silently.
        app.apply(Action::Unwind).unwrap();
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!(
                "what the editor wrote was thrown away unasked: {:?}",
                app.modal()
            )
        };
        assert!(dialog.message().contains(LABEL_FIELD), "{dialog:?}");
        assert_eq!(dialog.answers(), Answers::Destructive);

        // And the answer that is never destructive lands back on the text intact.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(field_value(&app), "written in the editor");
        assert_eq!(app.mode(), Mode::Surface);
        assert_eq!(fx.epic_labels(), before, "the round-trip wrote something");
    }

    #[test]
    fn a_refused_addition_keeps_the_buffer_and_the_session() {
        let (fx, mut app) = app();
        open_the_label_surface(&mut app);
        type_into(&mut app, "never written");

        // Only the store can judge a write, so the browser offers the action and
        // shows what comes back: here the entity goes between offer and accept.
        fx.remove_the_epics_file();
        app.apply(Action::Accept).unwrap();

        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("a refusal was swallowed: {:?}", app.modal())
        };
        assert_eq!(dialog.answers(), Answers::Acknowledge);
        assert_eq!(dialog.title(), REFUSAL_TITLE);

        // A refused save keeps everything: the buffer with its text, and the
        // session — only a successful write ends it. So the reader can fix what
        // they wrote, or carry it out through the external editor.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert_eq!(field_value(&app), "never written");
        assert_eq!(app.mode(), Mode::Surface);
        assert!(app.editing_target().is_some());
    }

    #[test]
    fn an_open_surface_takes_every_key_and_lets_none_reach_what_is_under_it() {
        let (_fx, mut app) = app();
        open_the_label_surface(&mut app);
        type_into(&mut app, "held");
        let (cursor, crumbs) = (app.nav().cursor(), app.nav().crumbs().join("/"));

        // No key typed into a field can move, reload, zoom or end the session. The
        // key map admits none of these here, but a mouse wheel reaches the state
        // machine without passing the key map at all.
        for action in [
            Action::Quit,
            Action::CursorDown,
            Action::CursorUp,
            Action::Descend,
            Action::Ascend,
            Action::Reload,
            Action::ToggleZoom,
            Action::EnterEditing,
            Action::Delete,
            Action::Add,
        ] {
            assert!(!app.apply(action).unwrap(), "{action:?} left the browser");
            assert_eq!(app.nav().cursor(), cursor, "{action:?} moved the cursor");
            assert_eq!(
                app.nav().crumbs().join("/"),
                crumbs,
                "{action:?} changed the level"
            );
            assert!(!app.zoomed(), "{action:?} rearranged the screen");
            assert_eq!(app.modal(), None, "{action:?} raised something");
            assert_eq!(field_value(&app), "held", "{action:?} changed the field");
        }
    }

    #[test]
    fn the_key_list_is_reachable_from_a_field_and_closing_it_leaves_the_buffer() {
        let (_fx, mut app) = app();
        open_the_label_surface(&mut app);
        type_into(&mut app, "kept");

        // Inside a field `?` is a character, so the key list has its own key there;
        // it is a layer above the buffer, not a way out of it.
        app.apply(Action::ToggleHelp).unwrap();
        assert_eq!(app.modal(), Some(&Modal::Help));
        assert_eq!(app.mode(), Mode::Surface);

        // One layer at a time: the overlay goes and the buffer stays, text and all.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert_eq!(field_value(&app), "kept");
        assert_eq!(app.mode(), Mode::Surface);
    }

    #[test]
    fn every_dialog_the_browser_raises_words_the_answers_its_set_admits() {
        // A dialog's affirmative word exists exactly when there is an answer to
        // word: a set that binds a letter with nothing to call it would list a key
        // the reader cannot use, and a word with no key behind it teaches a key that
        // does nothing.
        let dialogs = [
            Dialog::confirm(
                "Remove label ui?".into(),
                "remove",
                Performs::Write {
                    write: data::Write::RemoveLabel(Selection::Label(
                        data::Container::Epic("e".into()),
                        "ui".into(),
                    )),
                    done: "label ui removed".into(),
                },
                "cancel",
            ),
            Dialog::discard(LABEL_FIELD),
            Dialog::required(LABEL_FIELD, 0),
            Dialog::refusal("the store said no".into()),
        ];
        for dialog in dialogs {
            let words = dialog.words();
            assert_eq!(
                words.affirmative.is_some(),
                dialog.answers() == Answers::Destructive,
                "{dialog:?}"
            );
            assert!(!words.dismissal.is_empty(), "{dialog:?} has no way out");
            let listed = keymap::dialog_answers(dialog.answers(), words);
            assert!(!listed.is_empty(), "{dialog:?} lists no answer");
            for answer in &listed {
                // Nothing a dialog lists spells out its own key: the letters come
                // from the key map and the words from the dialog.
                let word = answer
                    .split_whitespace()
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(
                    !word.is_empty(),
                    "{answer:?} is a key with nothing to call it"
                );
            }
        }
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
