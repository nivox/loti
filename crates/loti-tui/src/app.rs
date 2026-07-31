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

use crate::action::{
    Action, AnswerWords, Answers, EditingAction, FieldKind, Fields, Lines, Mode, Shape,
};
use crate::data::{self, Collection, Container, Level, ReadOnly, Selection};
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

/// What entering editing mode says while the preview fills the width. It names
/// the remedy rather than the obstacle: there *is* something to edit, and what is
/// missing is the navigation pane, so the notice carries the key that brings the
/// pane back. The refusal never un-zooms by itself — the screen is the reader's.
const EDITING_NEEDS_THE_NAV_PANE: &str = "editing needs the navigation pane — z brings it back";

/// What a reload that finds the store may no longer be written says, on the one
/// reload that finds it.
///
/// It reports the transition and nothing more: the condition itself is durable,
/// so it is named in the breadcrumb's state slot for as long as it holds, and a
/// notice repeated on every later reload would say a thing the screen is already
/// saying. No way out is named because there is nothing left to get out of — the
/// editing session ended with the store's writability.
const EDITING_STOPPED_READ_ONLY: &str = "the store can no longer be written — editing stopped";

/// What the key that creates an epic says where an epic is not created from. It
/// names where it is, because the key is the browser's own rather than a letter a
/// row offers, and silence would read as a broken key.
const EPICS_ARE_MADE_FROM_THE_EPICS_LIST: &str =
    "a new epic is made from the epics list, not from inside one";

/// What the key that ends a browsing session says while a buffer holds text the
/// store has never been given.
///
/// A buffer of its own, rather than editing mode's notice: an epic is created
/// outside the mode, so there is no frozen row and "not an editing action" would
/// name a mode the reader is not in. It carries the way out itself, because a
/// notice covers the whole hint strip for as long as it is up.
const NOT_A_WAY_OUT_OF_A_BUFFER: &str = "nothing is written yet — Esc leaves this buffer";

/// The title a question about the frozen row carries. Fixed, so what a dialog is
/// stays legible when its text is the store's own and no browser word introduces
/// it.
const CONFIRM_TITLE: &str = " confirm ";
/// See [`CONFIRM_TITLE`].
const REFUSAL_TITLE: &str = " the store refused the change ";
/// See [`CONFIRM_TITLE`]. A conflict is a question rather than a report: the
/// entity moved on under the open buffer, and only the reader can say whether
/// what they wrote should replace what landed.
const CONFLICT_TITLE: &str = " the entity changed while you were editing ";
/// See [`CONFIRM_TITLE`].
const REQUIRED_TITLE: &str = " a required field is empty ";
/// See [`CONFIRM_TITLE`]. A value the *browser* will not send, as against one the
/// store refuses: the titles differ because the two are different answers from
/// different places, and a reader who cannot tell them apart cannot tell whose
/// rule they have run into.
const REJECTED_TITLE: &str = " that value cannot be used ";
/// See [`CONFIRM_TITLE`]. The browser hands the terminal over for an external
/// editor, so an editor that will not run is the browser's failure to report and
/// not the store's.
const EDITOR_TITLE: &str = " the editor could not run ";
/// See [`CONFIRM_TITLE`]. A store this binary opened and then could not read a
/// part of is reported and browsed on: the reader is told what could not be read,
/// and keeps whatever was on screen.
const UNREADABLE_TITLE: &str = " part of the store could not be read ";

/// What a label field is called wherever it has to be named: on the surface that
/// fills it in, and in the warning that says it is empty.
///
/// A field that replaces one of an entity's own is named by the store's word for
/// that field instead, so no constant here can come to call it something else
/// than the command line does.
const LABEL_FIELD: &str = "label";
/// See [`LABEL_FIELD`]. A blocker is named by a reference rather than written
/// out, so the field says so: what the reader types is a token the store
/// resolves, not prose.
const BLOCKER_FIELD: &str = "blocker reference (number or epic/number)";
/// See [`LABEL_FIELD`]. The store's own word for the field a state is held in, on
/// an epic as on a unit of work.
const STATE_FIELD: &str = "status";
/// See [`LABEL_FIELD`]. What a state that says why says it in. Revealed by the
/// states that carry one and required while it is on screen, so the accept-time
/// check that guards every other required field guards this one too.
const REASON_FIELD: &str = "reason";
/// See [`LABEL_FIELD`]. What a new comment is written into. A comment's text is
/// the whole of it, so the field is named for the comment itself.
const COMMENT_FIELD: &str = "comment";
/// See [`LABEL_FIELD`]. A claim's holder is freeform text and is not attribution:
/// it says who is on the work rather than who wrote the change, so the field is
/// named for the holder and never for an author.
const CLAIM_FIELD: &str = "claim holder";
/// See [`LABEL_FIELD`]. The id a new epic is created under. Named for the epic
/// rather than called `id` alone, because the warning about it is read over a
/// float that covers everything else on screen.
const EPIC_ID_FIELD: &str = "epic id";

/// What the field offering a cascade is called: the question it asks, with the
/// number of nodes it would close in it.
///
/// The count is in the label rather than in a notice, because it is the whole of
/// what the reader is deciding about — and it is what the plan said when the surface
/// opened, which the store may recompute under the lock.
fn cascade_label(open_descendants: usize) -> String {
    match open_descendants {
        1 => "also close 1 open descendant".to_string(),
        count => format!("also close {count} open descendants"),
    }
}

/// What a notice says once the write it reports has run: the words the surface
/// chose, finished with whatever only the store could say.
///
/// A cascade's size is not knowable before the write — the count the field named was
/// the plan as it stood when the surface opened, and the store recomputes it under
/// the lock — so the notice is completed here rather than fixed with the question.
/// That is also why the cascade is asked on a surface's field: a dialog's answer
/// carries a notice worded before its write runs, which could not name this count.
///
/// It names the row and how many went with it rather than every reference: the strip
/// holds one line, and the reloaded tree already shows which rows moved.
fn reported(done: String, effect: data::Effect) -> String {
    match effect {
        data::Effect::AsAsked => done,
        data::Effect::AlreadyListed(reference) => format!("blocker {reference} is already listed"),
        data::Effect::AlsoClosed(1) => format!("{done}, and 1 descendant with it"),
        data::Effect::AlsoClosed(count) => format!("{done}, and {count} descendants with it"),
        // A comment is addressed by its number for the rest of its life, and the
        // store assigns it under the lock, so the reader is told which comment they
        // now have rather than merely that they have one.
        data::Effect::Commented(id) => format!("{done}, numbered {id}"),
        // A ticket is addressed by the reference its number makes, and the number
        // comes out of its epic's pool under the lock: the reader is told which
        // ticket theirs became rather than merely that there is one.
        data::Effect::Created(reference) => format!("{done} as {reference}"),
    }
}

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

/// What an editing action does on the frozen row: opens something to answer,
/// opens something to fill in, or performs nothing and says why.
///
/// The shape of the input decides between the first two: a question with no text
/// to write is a dialog to answer, and anything the reader has to type is a
/// surface. The third is neither, because one action the browser is asked for is
/// outside what it does at all.
///
/// Invariant: this is the whole of what a letter does on a row, so the hint strip
/// and the key consult one answer — and only the two that perform something are
/// hinted, because a letter that writes nothing must not be taught as an action.
enum Offer {
    /// A question, answered where it stands.
    Ask(Dialog),
    /// A surface to fill in and accept.
    Fill(Surface),
    /// A surface whose starting text is the entity's own field, so it cannot be
    /// built without reading the entity.
    ///
    /// A variant of its own because this offer is asked on every frame — the hint
    /// strip asks it — and a read per frame would be a read the reader never asked
    /// for. So the offer says what is wanted and the read happens when the letter is
    /// pressed, which is also the moment the freshness rule names: the buffer starts
    /// from the current text and the stamp is as fresh as the edit.
    Compose(Composed),
    /// A write with nothing to fill in and nothing to ask, because the row carries
    /// everything the write needs: the letter performs it.
    ///
    /// A variant of its own rather than a surface with no fields or a question with
    /// one answer, because both of those would put something on screen for a reader
    /// to dismiss. What it carries is what a dialog's answer carries, so the write
    /// and the notice that reports it are still one decision.
    Perform { write: data::Write, done: String },
    /// Nothing for the browser to do, and the notice that says so — naming the
    /// command line, where the job the browser does not do is done instead.
    ///
    /// Not a hint and not a write: the row genuinely offers no action, and a
    /// reader who presses the letter anyway gets an answer better than "not an
    /// editing action" rather than being left to guess where else to look.
    Signpost(String),
}

/// What a surface has to be told by the store before it can open.
///
/// Invariant: every one of these is read when the letter is pressed and never
/// earlier, because a surface must start from what the store holds now: a buffer
/// opened on a stale preview writes back text nobody is looking at, and a picker
/// opened on a stale state marks a state the row has already left.
enum Composed {
    /// One of an entity's whole fields, and the stamp it was read at.
    Field(data::FreeForm),
    /// One comment's text, and the stamp its container was read at.
    CommentText,
    /// The state the row is in, the states it may be put into, and how many of its
    /// descendants are still open — the state to mark, and what a cascade would have
    /// to close.
    State,
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

    /// The question a write refused for a stale precondition raises: the entity
    /// moved on since the buffer was opened, so writing now replaces whatever
    /// landed.
    ///
    /// A question rather than the store's own words, unlike every other refusal:
    /// what the store reports is two stamps failing to match, and what the reader
    /// has to decide is whose text survives. Both answers lose something, so
    /// neither is a default — the way out keeps the buffer and decides nothing,
    /// which is the one answer that is never destructive anywhere in the browser.
    ///
    /// It names the entity, because the buffer covers the pane and the frozen row
    /// is dim beside it.
    fn conflict(reference: &str, write: data::Write, done: String) -> Self {
        Self {
            title: CONFLICT_TITLE,
            message: format!(
                "{reference} changed since this buffer was opened. \
                 Writing now replaces that change; nothing has been written yet."
            ),
            answers: Answers::Conflict,
            affirmative: Some(Answer {
                word: "overwrite anyway",
                // Without the precondition it was refused for: the reader has seen
                // that the entity moved on and said to write anyway.
                performs: Performs::Write {
                    write: write.overwriting(),
                    done,
                },
            }),
            dismissal: Dismissal {
                word: "back to the buffer",
                performs: None,
            },
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

    /// The warning a surface accepted with a value the browser itself will not
    /// send raises instead of writing. It carries the browser's own rule in the
    /// browser's own words, under a title of its own, so a reader can tell it from
    /// a store refusal shown verbatim — and dismissing it lands in the offending
    /// field, exactly as an empty required one does.
    fn rejected(why: String, index: usize) -> Self {
        Self::report(
            REJECTED_TITLE,
            why,
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

/// One value a picker offers.
///
/// Invariant: a picker's value is one of these rather than a word read back out of
/// a field, so the write carries the value the reader marked and never a string
/// parsed into a meaning — two pickers offer the word `closed` and mean different
/// things by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    /// A state the frozen row is put into.
    State(data::State),
    /// Whether closing the frozen row takes its open descendants with it.
    Cascade(bool),
}

impl Choice {
    /// The word the reader picks it by. A state's word is the store's own; the
    /// cascade's two are a plain answer to the question its label asks.
    fn word(self) -> &'static str {
        match self {
            Choice::State(state) => state.wire_name(),
            Choice::Cascade(true) => "yes",
            Choice::Cascade(false) => "no",
        }
    }
}

/// What one field of a surface holds.
///
/// Invariant: the kinds are exclusive by construction rather than by a flag beside
/// a shared value, so a picker has no cursor to move and no line break to drop,
/// and a field of text has no highlight anything could be marked with. A field
/// holding one line never holds a line break, whichever door text arrives through
/// — a keystroke or an external editor's result — and breaks are dropped rather
/// than turned into spaces, because a space is content the reader did not type. A
/// field holding many keeps them, and its cursor moves between lines as well as
/// along one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Content {
    /// Text the reader types, and where in it the next character lands — counted
    /// in characters and not bytes, so a multi-byte character is never split.
    ///
    /// How many lines it holds is not a property of what is in it: an empty field
    /// that holds many lines is still a text area, and a one-line field stays one
    /// however long its value grows.
    Text {
        value: String,
        cursor: usize,
        lines: Lines,
    },
    /// The values on offer, in the order the vertical keys move through them, and
    /// which of them is marked.
    ///
    /// The marked one *is* the value: a picker has no confirming key, so moving
    /// the mark is itself the change, and what is on screen is what a save writes.
    Pick { options: Vec<Choice>, at: usize },
}

/// What the browser itself insists on about a field's value, over and above there
/// being one at all.
///
/// Invariant: this is never a store rule reimplemented. What makes a value
/// acceptable is the store's judgement and its refusal is shown verbatim, so the
/// only thing here is a value the store does not judge — an epic's id is used as
/// a plain name, and a value that is no name addresses something other than what
/// the reader asked for. Whether an id is *free* stays the store's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    /// Nothing: the store is the only judge of this value.
    Store,
    /// A usable epic id.
    EpicId,
}

impl Check {
    /// Why the browser will not send this value, and `None` where it will.
    fn objection(self, value: &str) -> Option<String> {
        match self {
            Check::Store => None,
            // An epic id is one plain name. It is the whole left-hand side of every
            // `<epic-id>/<number>` reference, which is read by splitting on the
            // separator, and it is the name the epic is kept under — so a separator
            // in it names something other than the epic asked for, and the two names
            // that mean "a directory" rather than a name name no epic at all.
            Check::EpicId => (value.contains('/') || value == "." || value == "..").then(|| {
                "an epic id is one name: it cannot contain / and cannot be . or ..".to_string()
            }),
        }
    }
}

/// One field of an editing surface: something to type into, or something to pick
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// What the field is called wherever it has to be named: on the surface, and
    /// in a warning about it. A warning is raised over the surface and the frozen
    /// row is covered, so a warning that named no field would not say which.
    ///
    /// Owned rather than borrowed, because a field's name may carry something read
    /// from the store — how many descendants a cascade would close.
    label: String,
    /// Whether the store cannot be given this surface with the field left empty.
    required: bool,
    /// What the browser itself insists on about the value, over and above there
    /// being one; see [`Check`].
    check: Check,
    /// What it holds, which is the whole of the difference between the kinds.
    content: Content,
    /// Whether a content-mutating keystroke has landed here.
    ///
    /// Invariant: a flag, never a comparison against what the field started from.
    /// It is sticky — typing a character and deleting it again leaves the field
    /// dirty, as does marking a value and marking the first one back — and no
    /// motion within a field ever sets it. So the way out warns about a field that
    /// would lose nothing, which is accepted deliberately: a spurious warning is
    /// cheap, and a flag costs no per-keystroke compare of a whole body against
    /// its original.
    dirty: bool,
}

impl Field {
    /// An empty field, which is where every surface starts: nothing the browser
    /// puts there itself could be text the reader meant to write.
    fn new(label: impl Into<String>, required: bool, lines: Lines) -> Self {
        Self::filled(label, required, lines, String::new())
    }

    /// An empty required line the browser has a rule of its own about; see
    /// [`Check`]. Required, because a check on a value only means anything where
    /// there has to be one.
    fn checked(label: impl Into<String>, check: Check) -> Self {
        Self {
            check,
            ..Self::new(label, true, Lines::One)
        }
    }

    /// A field the reader picks a value in, starting on the value given — which is
    /// the value the store holds, so a save that changes nothing writes what is
    /// there rather than whatever happened to be listed first.
    ///
    /// Never required: a picker always holds one of its values, so there is no
    /// state of it the store could be given nothing from.
    fn pick(label: impl Into<String>, options: Vec<Choice>, on: Choice) -> Self {
        Self {
            label: label.into(),
            required: false,
            // A picker offers only values the browser put there, so there is
            // nothing about one for the browser to object to.
            check: Check::Store,
            content: Content::Pick {
                // A value the list does not offer marks the first one instead: a
                // picker with nothing marked would have no value at all.
                at: options.iter().position(|o| *o == on).unwrap_or(0),
                options,
            },
            dirty: false,
        }
    }

    /// A field starting from text the store holds, which is where a field that
    /// replaces an existing value starts: a buffer opened on an empty body would
    /// make every save a deletion.
    ///
    /// The text goes through the same sieve a keystroke does, so a one-line field
    /// cannot be seeded with a line break either, and the field is not dirty: the
    /// reader has typed nothing, so the way out has nothing to warn about. The
    /// cursor starts at the top rather than at the end, because the reader reads
    /// the document before changing it and a cursor at the end of a long body would
    /// open the buffer scrolled past everything in it.
    fn filled(label: impl Into<String>, required: bool, lines: Lines, value: String) -> Self {
        let mut field = Self {
            label: label.into(),
            required,
            check: Check::Store,
            content: Content::Text {
                value: String::new(),
                cursor: 0,
                lines,
            },
            dirty: false,
        };
        let kept: String = value.chars().filter(|c| field.accepts(*c)).collect();
        field.content = Content::Text {
            value: kept,
            cursor: 0,
            lines,
        };
        field
    }

    /// What the field is called.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// What kind of field it is, which is what decides whether a line break in it
    /// is content, what the vertical keys reach, and whether an external editor has
    /// anything to take.
    pub fn kind(&self) -> FieldKind {
        match &self.content {
            Content::Text { lines, .. } => FieldKind::text(*lines),
            Content::Pick { .. } => FieldKind::Pick,
        }
    }

    /// What the reader is looking at in it, for the drawing. Derived from what the
    /// field holds, so a picker cannot be drawn as a line of text nor a line of
    /// text as a list of values.
    pub fn shown(&self) -> Shown<'_> {
        match &self.content {
            Content::Text { value, cursor, .. } => Shown::Text {
                value,
                cursor: *cursor,
            },
            Content::Pick { options, at } => Shown::Pick {
                options: options.iter().map(|o| o.word()).collect(),
                at: *at,
            },
        }
    }

    /// Whether anything has been typed into it, or marked in it. See
    /// [`Field::dirty`].
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The text it holds, and `None` for a picker — which holds a value rather
    /// than text, so there is nothing in it to hand to an editor or to send as a
    /// reason.
    fn text(&self) -> Option<&str> {
        match &self.content {
            Content::Text { value, .. } => Some(value),
            Content::Pick { .. } => None,
        }
    }

    /// The value a picker holds, and `None` for a field of text.
    fn chosen(&self) -> Option<Choice> {
        match &self.content {
            Content::Pick { options, at } => options.get(*at).copied(),
            Content::Text { .. } => None,
        }
    }

    /// Whether a required field has nothing in it to send.
    ///
    /// A field holding only whitespace counts as empty: the reader cannot tell it
    /// from a blank one on screen, so warning is more honest than writing
    /// something invisible. What makes a *non-blank* value acceptable is the
    /// store's rule, and the browser reimplements none of those.
    ///
    /// A picker is never unfilled — it always holds one of its values — which is
    /// why it is never required.
    fn unfilled(&self) -> bool {
        self.required && self.text().is_some_and(|text| text.trim().is_empty())
    }

    /// Why the browser itself will not send what this field holds, and `None`
    /// where it will — which is every field but the one the browser has a rule of
    /// its own about; see [`Check`].
    ///
    /// Asked of a field that has something in it: an empty one is answered for by
    /// being required or not, and a rule about the shape of a value has nothing to
    /// say about the absence of one.
    fn rejected(&self) -> Option<String> {
        self.check.objection(self.text()?)
    }

    /// Whether a character may land in a field of text at all.
    ///
    /// A line break is content where the field holds many lines and is dropped
    /// where it holds one. A carriage return is never content: nothing types one,
    /// and an editor that hands back CRLF means one line break rather than a break
    /// and a character no terminal shows.
    fn accepts(&self, c: char) -> bool {
        match &self.content {
            Content::Text { lines, .. } => accepts(c, *lines),
            // A picker holds no text, so no character is content in it.
            Content::Pick { .. } => false,
        }
    }

    /// Apply a key's intent to the field.
    ///
    /// Invariant: every intent that changes what the field would save sets the
    /// dirty flag and no motion within the field ever does. In a field of text that
    /// includes an intent which happened to change nothing — a deletion with
    /// nothing left to delete — because dirty there is what was *pressed* and not
    /// what differs. In a picker the vertical keys are not motion at all: the mark
    /// is the value, so moving it is the change, and a key that finds no value that
    /// way has changed nothing to warn about.
    fn apply(&mut self, action: Action) {
        match &mut self.content {
            Content::Text {
                value,
                cursor,
                lines,
            } => {
                let lines = *lines;
                match action {
                    Action::Insert(c) => {
                        // A character the field does not hold is dropped rather than
                        // refused: the keystroke still counts as typing, because
                        // dirty is what was pressed and not what differs.
                        if accepts(c, lines) {
                            let at = byte_at(value, *cursor);
                            value.insert(at, c);
                            *cursor += 1;
                        }
                        self.dirty = true;
                    }
                    Action::DeleteBefore => {
                        if *cursor > 0 {
                            *cursor -= 1;
                            let at = byte_at(value, *cursor);
                            value.remove(at);
                        }
                        self.dirty = true;
                    }
                    Action::DeleteAfter => {
                        if *cursor < char_count(value) {
                            let at = byte_at(value, *cursor);
                            value.remove(at);
                        }
                        self.dirty = true;
                    }
                    Action::MoveLeft => *cursor = cursor.saturating_sub(1),
                    Action::MoveRight => *cursor = (*cursor + 1).min(char_count(value)),
                    // The line's ends, which in a field holding one line are the
                    // value's: one rule covers both kinds rather than one rule per
                    // kind that could come to disagree.
                    Action::MoveToStart => *cursor = line_bounds(value, *cursor).0,
                    Action::MoveToEnd => *cursor = line_bounds(value, *cursor).1,
                    // Vertical motion keeps the column, clamped to what the line it
                    // lands on has: a short line puts the cursor at its end rather
                    // than past it. A field with no line that way leaves the cursor
                    // alone — a field holding one line has none in either direction.
                    Action::MoveUp => {
                        let (start, _) = line_bounds(value, *cursor);
                        let column = *cursor - start;
                        if start > 0 {
                            let (above, above_end) = line_bounds(value, start - 1);
                            *cursor = (above + column).min(above_end);
                        }
                    }
                    Action::MoveDown => {
                        let (start, end) = line_bounds(value, *cursor);
                        let column = *cursor - start;
                        if end < char_count(value) {
                            let (below, below_end) = line_bounds(value, end + 1);
                            *cursor = (below + column).min(below_end);
                        }
                    }
                    // Everything else is the surface's business or nobody's: a key
                    // the field does not answer must not silently change what it
                    // holds.
                    _ => {}
                }
            }
            Content::Pick { options, at } => {
                // The list has ends rather than wrapping round, exactly as a text
                // cursor does at the first and last line: the values are drawn as a
                // list, so a mark that leapt from the bottom to the top would be a
                // screen arguing with the keyboard.
                let moved = match action {
                    Action::MoveUp => at.checked_sub(1),
                    Action::MoveDown => Some(*at + 1).filter(|next| *next < options.len()),
                    // Nothing else reaches a picker. There is no text in it to type
                    // into, delete from or move along, so the keys that would do
                    // those leave it untouched — and leave it clean, because nothing
                    // in it could have changed.
                    _ => None,
                };
                if let Some(next) = moved {
                    *at = next;
                    self.dirty = true;
                }
            }
        }
    }

    /// Take an external editor's result, which counts as content the reader typed:
    /// the way out warns about it exactly as it does about typing.
    ///
    /// It goes through the same sieve a keystroke does, so a field holding one line
    /// cannot be given a line break by the editor either — see [`Content`]. A
    /// picker is left exactly as it was: it holds no text, so there was nothing to
    /// hand over and there is nothing to take back.
    fn replace(&mut self, text: &str) {
        let kept: String = text.chars().filter(|c| self.accepts(*c)).collect();
        if let Content::Text { value, cursor, .. } = &mut self.content {
            *cursor = char_count(&kept);
            *value = kept;
            self.dirty = true;
        }
    }
}

/// What the reader is looking at in one field, for the drawing.
///
/// Invariant: derived from what the field holds rather than chosen by the drawing,
/// so a picker is never drawn as a line of text — which would show one of its
/// values and hide the rest — and a line of text is never drawn as a list.
pub enum Shown<'a> {
    /// Text, and where in it the next character lands.
    Text {
        /// What the field holds.
        value: &'a str,
        /// Where the next character lands, in characters from the start.
        cursor: usize,
    },
    /// The values on offer, in the order the vertical keys move through them, and
    /// which of them the field holds.
    Pick {
        /// The words the values go by.
        options: Vec<&'static str>,
        /// Which of them is marked, which is the field's value.
        at: usize,
    },
}

/// Whether a character may land in a field of text holding this many lines; see
/// [`Field::accepts`].
fn accepts(c: char, lines: Lines) -> bool {
    match c {
        '\r' => false,
        '\n' => matches!(lines, Lines::Many),
        _ => true,
    }
}

/// The line a character offset is on: where it starts, and the offset just past
/// its last character. A field holding one line has exactly one, spanning the
/// whole value.
fn line_bounds(value: &str, cursor: usize) -> (usize, usize) {
    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());
    let start = chars[..cursor]
        .iter()
        .rposition(|c| *c == '\n')
        .map(|at| at + 1)
        .unwrap_or(0);
    let end = chars[cursor..]
        .iter()
        .position(|c| *c == '\n')
        .map(|at| cursor + at)
        .unwrap_or(chars.len());
    (start, end)
}

/// How many characters a value holds, which is where its cursor may go up to.
fn char_count(value: &str) -> usize {
    value.chars().count()
}

/// The byte offset of a character offset, so an insertion or a removal never
/// lands inside a multi-byte character.
fn byte_at(value: &str, cursor: usize) -> usize {
    value
        .char_indices()
        .nth(cursor)
        .map(|(at, _)| at)
        .unwrap_or(value.len())
}

/// An open editing surface: the fields the reader fills in, and what accepting it
/// writes.
///
/// Invariant: a surface is open only while editing mode is on, and a dialog about
/// it is laid over it rather than replacing it — so answering or dismissing one
/// lands back in the buffer it was raised about, with the text intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    /// What the surface is titled: the action and the row it acts on, since a
    /// float covers the row it was opened from and the pane is not the row either.
    title: String,
    fields: Vec<Field>,
    /// Which field the keyboard is in.
    focus: usize,
    /// Where it renders. Carried by the surface rather than worked out by the
    /// drawing from what the surface happens to contain — see [`Placement`].
    placement: Placement,
    commit: Commit,
}

/// Where an open surface renders.
///
/// Invariant: the surface says where, and the drawing obeys it. So a surface whose
/// shape a centred float does not suit is a value here rather than a second
/// drawing path, and the drawing never guesses from a surface's field count or
/// field kinds where that surface belongs.
///
/// Whether an open surface may fill the width is a separate question and not
/// settled here: a surface that renders in the pane renders wherever the pane is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Centred over everything and as tall as its fields, covering the row it was
    /// opened from — which is why its title names that row. The answer for a short
    /// field, where keeping the row visible buys nothing.
    Float,
    /// In the preview pane, so the navigation pane keeps the frozen row visible
    /// beside it. The answer for text long enough that a reader needs to see it
    /// against what it belongs to.
    Pane,
}

impl Placement {
    /// Everywhere a surface may render, so the drawing that has to place them all
    /// cannot then miss one.
    pub const ALL: &'static [Placement] = &[Placement::Float, Placement::Pane];
}

/// What accepting a surface writes.
///
/// Invariant: named when the surface opens and built from the fields at the moment
/// it is accepted, so the write and the notice that reports it are one decision
/// and can never come to name different things.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Commit {
    /// Create an epic under the id the first field holds.
    ///
    /// It names no row: an epic has no container to be added to, which is why the
    /// key that opens this surface is the browser's own rather than a letter a row
    /// offers.
    CreateEpic,
    /// Create a unit of work in the container the frozen row names: a top-level
    /// ticket of an epic, or a subticket of a ticket. Which of the two is the row's
    /// answer, so one commit covers both.
    CreateNode(Selection),
    /// Put the label the field holds on the set the frozen row names.
    AddLabel(Selection),
    /// Put the node the field's reference names on the dependency list the frozen
    /// row names. The browser judges nothing about the reference: what it names,
    /// and whether that may block this, come back from the store.
    AddBlocker(Selection),
    /// Put the comment the field holds on the list the frozen row names, authored
    /// by the human — the browser writes as the human and only the human.
    ///
    /// No stamp: an append takes a slot of its own, so it discards nobody's text.
    /// Which number the comment takes is the store's answer, so the notice about it
    /// is finished once the write has run.
    AddComment(Selection),
    /// Take the claim on the node the frozen row names, for the holder the field
    /// holds. No stamp: a claim is not a free-form replacement, and a claim has one
    /// holder, so taking an already-held one reassigns it.
    TakeClaim(Selection),
    /// Put the epic or node the frozen row names into the state its picker holds,
    /// with the reason and the cascade the fields that state revealed hold.
    ///
    /// No stamp: a state pick is not a free-form replacement, and its conflict is
    /// the later of two deliberate choices rather than text silently lost.
    SetState {
        /// The epic or node whose state is set.
        target: Selection,
        /// The state the row was in when the surface opened.
        ///
        /// Not the state being written — the picker holds that, and it is read from
        /// the picker so that what the reader has marked is what goes. This is what
        /// a surface that had somehow lost its picker would write instead: the state
        /// the row already has, rather than one nobody chose.
        current: data::State,
        /// How many of the row's descendants were still open when the surface
        /// opened, which is what the cascade field names and offers.
        ///
        /// Advisory: the store recomputes the plan under the lock, so it may close
        /// more or fewer than the field promised. Kept as read rather than
        /// re-asked, because a count that changed under the reader would move a
        /// field in and out from under the keyboard.
        open_descendants: usize,
    },
    /// Replace one whole field of what the frozen row names, guarded by the stamp
    /// that field's text was read at.
    ///
    /// The stamp is captured when the surface opens and travels unread from there to
    /// the write: the window it guards is the edit itself, which is exactly the
    /// window the browser cannot see into.
    Replace {
        /// The epic, node or comment the field belongs to.
        target: Selection,
        /// Which of its fields the surface replaces.
        field: data::Replaceable,
        /// The stamp that field's text was read at.
        stamp: data::Stamp,
    },
}

/// What a new member of a container is called: an epic's is a ticket, and a
/// ticket's is a subticket.
///
/// One answer, so the form's title and the notice its write raises cannot come to
/// call the same thing two things. Only a container the offer table admits ever
/// reaches it; every other kind of row holds no units of work at all.
fn node_noun(parent: &Selection) -> &'static str {
    match parent {
        Selection::Node(_) => "subticket",
        Selection::Epic(_)
        | Selection::Collection(..)
        | Selection::Label(..)
        | Selection::Comment(..)
        | Selection::Asset(..)
        | Selection::Blocker(..) => "ticket",
    }
}

/// The pair of fields every creation form ends with: what the new entity is
/// called, and its one-line summary.
///
/// Named by the store's own words for those fields, so a form that fills one in
/// and the surface that later replaces it call it the same thing — and required
/// exactly as they are there. A name is how every row addresses what it names, so
/// there is no creating something without one; a summary is a line a reader may
/// leave for later.
///
/// A body is not among them: a creation form asks for what a row cannot be read
/// without, and the long-form text has a letter of its own the moment the row
/// exists.
fn naming_fields() -> Vec<Field> {
    vec![
        Field::new(data::FreeForm::Name.noun(), true, Lines::One),
        Field::new(data::FreeForm::Summary.noun(), false, Lines::One),
    ]
}

impl Surface {
    /// The surface that creates an epic: the id it will be addressed by, and the
    /// pair every creation form ends with.
    ///
    /// A float, like every other short form: none of its fields holds prose, and
    /// there is no row underneath for the reader to keep in view — an epic has no
    /// container, which is why this one is opened by a key of the browser's own
    /// rather than by a letter a row offers.
    ///
    /// The id leads because everything else about the epic can be changed later and
    /// it cannot: it is the address, and the browser has a rule of its own about
    /// the shape of one.
    fn create_epic() -> Self {
        let mut fields = vec![Field::checked(EPIC_ID_FIELD, Check::EpicId)];
        fields.extend(naming_fields());
        Self {
            title: " new epic ".to_string(),
            fields,
            focus: 0,
            placement: Placement::Float,
            commit: Commit::CreateEpic,
        }
    }

    /// The surface that creates a unit of work in the container the frozen row
    /// names, which is a ticket on an epic's row and a subticket on a ticket's.
    ///
    /// One form for both, because what is created is decided by the row the cursor
    /// stands on and not by anything the reader fills in — and what it is called
    /// follows the same answer, so the title cannot name one and the write make the
    /// other.
    fn create_node(parent: Selection) -> Self {
        Self {
            title: format!(" new {} on {} ", node_noun(&parent), parent.reference()),
            fields: naming_fields(),
            focus: 0,
            placement: Placement::Float,
            commit: Commit::CreateNode(parent),
        }
    }

    /// The surface that adds one label: a single short line, so it is a float and
    /// not the preview pane — keeping the row visible buys nothing for a field this
    /// size, and the pane is where the long-form text goes.
    fn add_label(set: Selection, container: String) -> Self {
        Self {
            title: format!(" new label on {container} "),
            fields: vec![Field::new(LABEL_FIELD, true, Lines::One)],
            focus: 0,
            placement: Placement::Float,
            commit: Commit::AddLabel(set),
        }
    }

    /// The surface that adds one blocker: one short field holding a reference, so
    /// it is a float for the same reason the label surface is — there is no
    /// long-form text here for the pane to hold.
    fn add_blocker(list: Selection, container: String) -> Self {
        Self {
            title: format!(" new blocker on {container} "),
            fields: vec![Field::new(BLOCKER_FIELD, true, Lines::One)],
            focus: 0,
            placement: Placement::Float,
            commit: Commit::AddBlocker(list),
        }
    }

    /// The surface that adds one comment: a buffer in the preview pane, because a
    /// comment is prose and the row it is being written about should stay visible
    /// beside it — the same reason a body is edited there.
    ///
    /// It starts empty and is required: a comment is one remark and its text is the
    /// whole of it, so a comment with nothing in it says nothing.
    fn add_comment(list: Selection, container: String) -> Self {
        Self {
            title: format!(" new comment on {container} "),
            fields: vec![Field::new(COMMENT_FIELD, true, Lines::Many)],
            focus: 0,
            placement: Placement::Pane,
            commit: Commit::AddComment(list),
        }
    }

    /// The surface that takes a claim: one short field holding the holder, so it is
    /// a float for the same reason the label surface is.
    ///
    /// It starts empty rather than from the holder already on the row, even though
    /// taking an already-held claim reassigns it: a claim names who is picking the
    /// work up, and text the browser put there itself is text nobody chose.
    fn take_claim(node: Selection) -> Self {
        Self {
            title: format!(" claim on {} ", node.reference()),
            fields: vec![Field::new(CLAIM_FIELD, true, Lines::One)],
            focus: 0,
            placement: Placement::Float,
            commit: Commit::TakeClaim(node),
        }
    }

    /// The surface that picks a row's state: a picker, and after it whatever the
    /// picked state needs — a reason for a state that says why, and, on a unit of
    /// work being closed with open descendants below it, whether to close those too.
    ///
    /// A float rather than the preview pane: what a reader needs in front of them is
    /// the states on offer, and none of the fields holds prose.
    ///
    /// The picker leads and the conditional fields follow, which is the order they
    /// depend in: the reader picks, and the surface then asks for whatever that pick
    /// needs. See [`Surface::revise`].
    fn set_state(target: data::StateTarget) -> Self {
        let options: Vec<Choice> = target.offered.iter().copied().map(Choice::State).collect();
        let mut surface = Self {
            title: format!(" status of {} ", target.selection.reference()),
            fields: vec![Field::pick(
                STATE_FIELD,
                options,
                Choice::State(target.current),
            )],
            focus: 0,
            placement: Placement::Float,
            commit: Commit::SetState {
                target: target.selection,
                current: target.current,
                open_descendants: target.open_descendants,
            },
        };
        // The state the row is already in may itself be one that says why, so the
        // surface opens with whatever that state asks for rather than only revealing
        // it once the reader has moved the mark.
        surface.revise();
        surface
    }

    /// The surface that replaces one whole field: one field, starting from the text
    /// the read returned and carrying that read's stamp, so it is the current value
    /// rather than a rendered preview of an older one.
    ///
    /// One surface for every replaceable field, because everything but its shape is
    /// the same: the read, the stamp, the conflict it may be refused for. The shape
    /// is the field's own answer — the long-form text is many lines in the preview
    /// pane, so the frozen row stays visible beside the prose being rewritten, while
    /// a short line is a float, where keeping the row visible buys nothing.
    fn replace(
        field: data::Replaceable,
        target: Selection,
        value: String,
        stamp: data::Stamp,
    ) -> Self {
        // A name may not be emptied: it is how every row addresses what it names,
        // so a row with none is a row a reader cannot pick out. Nor may a comment,
        // whose text is the whole of it and which is taken back by being withdrawn
        // rather than by being emptied. A summary and a body may — emptying either
        // is a thing a reader may mean — and what makes a non-empty value
        // acceptable is the store's rule and not this surface's.
        let (placement, lines, required) = match field {
            data::Replaceable::Field(data::FreeForm::Name) => (Placement::Float, Lines::One, true),
            data::Replaceable::Field(data::FreeForm::Summary) => {
                (Placement::Float, Lines::One, false)
            }
            data::Replaceable::Field(data::FreeForm::Body) => (Placement::Pane, Lines::Many, false),
            data::Replaceable::CommentText => (Placement::Pane, Lines::Many, true),
        };
        Self {
            title: format!(" {} of {} ", field.noun(), target.reference()),
            fields: vec![Field::filled(field.noun(), required, lines, value)],
            focus: 0,
            placement,
            commit: Commit::Replace {
                target,
                field,
                stamp,
            },
        }
    }

    /// What the surface is titled.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Where it renders, which is the surface's own say and not the drawing's.
    pub fn placement(&self) -> Placement {
        self.placement
    }

    /// Its fields, in the order they are filled in.
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// Which of them the keyboard is in.
    ///
    /// Invariant: always a field this surface actually holds. A conditional field
    /// that disappears can leave the stored index past the end of what is left, so
    /// the clamp lives here, where the focus is read, rather than in each place the
    /// fields may change — which is what makes it something no later conditional
    /// field has to remember.
    pub fn focus(&self) -> usize {
        self.focus.min(self.fields.len().saturating_sub(1))
    }

    /// Its shape, to the precision a key's meaning turns on: how many fields it
    /// holds, and what kind of field the focused one is. This is the whole of what
    /// the key map is told about a surface, so the map decides which keys have
    /// somewhere to go and which of them writes a line break, rather than the
    /// surface deciding it after the fact.
    ///
    /// Recomputed on every ask rather than captured when the surface opens: the
    /// focus moves and a field may come and go, so a captured shape is a key map
    /// and a strip waiting to disagree with the surface they describe.
    pub fn shape(&self) -> Shape {
        Shape {
            fields: Fields::of(self.fields.len()),
            // A surface with no fields is not a surface, and the one-line kind is
            // the answer that holds no line break: a keyboard with nowhere to type
            // must not be the keyboard a line break is content in.
            kind: self
                .fields
                .get(self.focus())
                .map(Field::kind)
                .unwrap_or(FieldKind::OneLine),
        }
    }

    /// Put the keyboard in the next or the previous field, wrapping round at
    /// either end.
    ///
    /// Wrapping because the forward key alone must reach every field: a walk that
    /// stopped at the last one would leave that key dead there, and nothing about
    /// arriving at the last field means anything — accepting is the save key's, so
    /// the end of the fields is not the end of filling them in.
    fn move_focus(&mut self, forwards: bool) {
        let Some(last) = self.fields.len().checked_sub(1) else {
            return;
        };
        self.focus = match forwards {
            true if self.focus >= last => 0,
            true => self.focus + 1,
            false if self.focus == 0 => last,
            false => self.focus - 1,
        };
    }

    /// The field a warning about lost text names: the first one with typing in it,
    /// and `None` for a surface nothing has been typed into — which is what makes
    /// the way out ask on one and not on the other.
    fn dirtied(&self) -> Option<&Field> {
        self.fields.iter().find(|field| field.dirty)
    }

    /// The first required field left empty, which is the one an accept must warn
    /// about and land the reader back in.
    ///
    /// Every field is looked at, not only the one the keyboard is in: a form is
    /// accepted from wherever the reader happens to be standing, so a check that
    /// stopped at the field in front of them would send a form with a later one
    /// blank.
    fn unfilled(&self) -> Option<usize> {
        self.fields.iter().position(Field::unfilled)
    }

    /// The first field whose value the browser itself will not send, and why; see
    /// [`Check`]. Every field, for the same reason every one is checked for being
    /// empty.
    fn rejected(&self) -> Option<(usize, String)> {
        self.fields
            .iter()
            .enumerate()
            .find_map(|(index, field)| field.rejected().map(|why| (index, why)))
    }

    /// Give the focused field a key's intent, and bring the field list back in line
    /// with what the fields now hold.
    ///
    /// The one door into a field: every key that belongs to a field arrives here, so
    /// a pick that reveals or hides a conditional field cannot land without the
    /// surface being revised for it.
    fn apply(&mut self, action: Action) {
        self.focused_mut().apply(action);
        self.revise();
    }

    /// Bring the fields in line with what its picker now holds: a state that says
    /// why reveals a reason, and closing a unit of work that still has open
    /// descendants offers to close them too, naming how many.
    ///
    /// Invariant: a surface holds exactly the fields on screen. A field appears the
    /// moment the mark lands on a value that wants it and goes the moment the mark
    /// moves off, so it can never hold text the reader cannot see — coming back asks
    /// afresh. That is the accepted cost of a picker with no confirming key, where
    /// looking at another value is choosing it.
    ///
    /// A field that is still wanted is carried over rather than rebuilt, because this
    /// runs after every keystroke: a list rebuilt from the mark alone would swallow
    /// each character as the reader typed it.
    ///
    /// Only a state surface has anything to revise; every other surface's fields are
    /// fixed when it opens.
    fn revise(&mut self) {
        let Commit::SetState {
            current,
            open_descendants,
            ..
        } = &self.commit
        else {
            return;
        };
        let (current, open_descendants) = (*current, *open_descendants);
        let state = self.picked_state(current);
        // Carried over rather than rebuilt, so a reason survives a pick that still
        // wants one. Found by being the surface's field of text, which the picker
        // and the cascade field are not.
        let reason = self
            .fields
            .iter()
            .find(|field| field.text().is_some())
            .cloned();
        let cascade = self.cascade_field();
        // The picker leads and is never rebuilt: it is the surface's own value, and
        // rebuilding it would drop where its mark is and that the reader moved it.
        self.fields.truncate(1);
        if state.needs_reason() {
            self.fields
                .push(reason.unwrap_or_else(|| Field::new(REASON_FIELD, true, Lines::One)));
        }
        // Only where there is something for it to close: a leaf, or a row whose
        // descendants are all resolved already, is closed without a question. The
        // count is what the plan said when the surface opened and the store may
        // recompute it, so the field asks for a cascade rather than for these nodes.
        if state.cascades() && open_descendants > 0 {
            self.fields.push(cascade.unwrap_or_else(|| {
                Field::pick(
                    cascade_label(open_descendants),
                    vec![Choice::Cascade(false), Choice::Cascade(true)],
                    // No by default, which is the store's own default: closing a row
                    // resolves that row, and taking a subtree with it is the wider
                    // thing a reader has to ask for.
                    Choice::Cascade(false),
                )
            }));
        }
        // The list may be shorter than it was, so the stored focus is brought back
        // inside it here as well as clamped where it is read: a field that vanished
        // must not leave the keyboard pointing past the end, nor jump back to a field
        // that reappears later.
        self.focus = self.focus();
    }

    /// The state its picker holds.
    ///
    /// Invariant: a state surface leads with its picker and keeps it there while the
    /// fields after it come and go, so there is always a marked state to read. A
    /// surface that had lost its picker falls back to the state the row is already
    /// in — writing what is there rather than a state nobody chose — and any other
    /// kind of surface has no state at all.
    fn picked_state(&self, current: data::State) -> data::State {
        match self.fields.first().and_then(Field::chosen) {
            Some(Choice::State(state)) => state,
            Some(Choice::Cascade(_)) | None => current,
        }
    }

    /// Its cascade field, if it has one; see [`Surface::revise`].
    fn cascade_field(&self) -> Option<Field> {
        self.fields
            .iter()
            .find(|field| matches!(field.chosen(), Some(Choice::Cascade(_))))
            .cloned()
    }

    /// Whether its cascade field says to close the row's open descendants too. No
    /// wherever the field is not revealed, which is the store's own default.
    fn cascading(&self) -> bool {
        self.cascade_field().and_then(|field| field.chosen()) == Some(Choice::Cascade(true))
    }

    /// What its one field of text holds, and nothing where it has none revealed —
    /// which is what a state that says nothing about why sends as its reason.
    ///
    /// Every surface but the state picker holds exactly one field and it is text, so
    /// this is that field; on a state picker it is the reason, which is the only text
    /// there is to type.
    fn typed(&self) -> String {
        self.fields
            .iter()
            .find_map(Field::text)
            .unwrap_or_default()
            .to_string()
    }

    /// What the field in this position holds, and nothing where the surface has no
    /// such field or that field is not made of text.
    fn text_at(&self, index: usize) -> String {
        self.fields
            .get(index)
            .and_then(Field::text)
            .unwrap_or_default()
            .to_string()
    }

    /// The name and the summary a creation form ends with, whatever it asks for in
    /// front of them.
    ///
    /// Read from the end rather than from a position counted per form, so a form
    /// that leads with something of its own — an epic's id — and one that does not
    /// read the same pair, and neither can be handed the other's field by
    /// miscounting. See [`naming_fields`], which is what puts them there.
    fn named(&self) -> (String, String) {
        let at = self.fields.len().saturating_sub(2);
        (self.text_at(at), self.text_at(at + 1))
    }

    /// The field taking keystrokes.
    fn focused_mut(&mut self) -> &mut Field {
        // The focus is read clamped to the fields the surface holds, so there is
        // always one to type into.
        let focus = self.focus();
        &mut self.fields[focus]
    }

    /// The write accepting the surface performs, and what the notice says once it
    /// is committed.
    fn write(&self) -> (data::Write, String) {
        match &self.commit {
            Commit::CreateEpic => {
                // The id is the form's own leading field; the pair after it is the
                // pair every creation form ends with.
                let id = self.text_at(0);
                let (name, summary) = self.named();
                (
                    data::Write::CreateEpic {
                        epic: Selection::Epic(id.clone()),
                        name,
                        summary,
                    },
                    // The notice names the epic by the id it now has: the reader
                    // typed it, so nothing about the write is needed to say it, and
                    // by the time the notice is read the form is gone.
                    format!("epic {id} created"),
                )
            }
            Commit::CreateNode(parent) => {
                let (name, summary) = self.named();
                (
                    data::Write::CreateNode {
                        parent: parent.clone(),
                        name,
                        summary,
                    },
                    // The notice names what was made and what it was made on. The
                    // reference it took is added once the write has run: only the
                    // store knows the number, and that reference is the only name
                    // the new ticket has.
                    format!("{} on {} added", node_noun(parent), parent.reference()),
                )
            }
            Commit::AddLabel(set) => {
                let label = self.typed();
                (
                    data::Write::AddLabel(set.clone(), label.clone()),
                    // The notice names the label, because by the time it is read
                    // the surface that held it is gone.
                    format!("label {label} added"),
                )
            }
            Commit::AddBlocker(list) => {
                let reference = self.typed();
                (
                    data::Write::AddBlocker(list.clone(), reference.clone()),
                    // The notice names the blocker as the store records it, which a
                    // bare number is not: by the time it is read the surface that
                    // held the reference is gone.
                    format!("blocker {} added", data::blocker_name(list, &reference)),
                )
            }
            Commit::AddComment(list) => (
                data::Write::AddComment(list.clone(), self.typed()),
                // The notice names the container, and the number the store gave the
                // comment is added once the write has run: only the store knows it,
                // and it is how the comment is addressed from then on.
                format!("comment on {} added", list.reference()),
            ),
            Commit::TakeClaim(node) => {
                let holder = self.typed();
                let reference = node.reference();
                (
                    data::Write::TakeClaim(node.clone(), holder.clone()),
                    // The notice names the ticket and the holder: the surface that
                    // held the holder is gone by the time it is read, and the row it
                    // was taken on is one of several that look alike.
                    format!("claim on {reference} taken by {holder}"),
                )
            }
            Commit::SetState {
                target,
                current,
                open_descendants: _,
            } => {
                let state = self.picked_state(*current);
                let reference = target.reference();
                (
                    data::Write::SetState {
                        target: target.clone(),
                        state,
                        reason: self.typed(),
                        cascade: self.cascading(),
                    },
                    // The notice names the row and the state it is now in, because by
                    // the time it is read the surface is gone and the row it was
                    // opened on is one of several that look alike. What a cascade
                    // took with it is added once the write has run: the count is the
                    // store's answer and not the plan's.
                    format!("{reference} is now {}", state.wire_name()),
                )
            }
            Commit::Replace {
                target,
                field,
                stamp,
            } => {
                let reference = target.reference();
                (
                    data::Write::Replace {
                        target: target.clone(),
                        field: *field,
                        value: self.typed(),
                        expect: Some(*stamp),
                    },
                    // The notice names the field and the entity: the surface that
                    // held the text is gone by the time it is read, and the row it
                    // belonged to is one of several that look alike.
                    format!("{} of {reference} saved", field.noun()),
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
    /// The document identity currently rendered — [`Selection::document`], not
    /// the raw row selection — so a cursor move across rows that share a
    /// document (a container's collection rows, and the label rows inside a
    /// collection) is never mistaken for a change of what is shown.
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
    /// Why the store may not be written, or `None` while it may be.
    ///
    /// Invariant: this is the store's own answer, asked at startup and again on
    /// every reload — never a launch-time verdict, because an agent can migrate
    /// a store while the browser is open, and never a flag, because read-only is
    /// a state of the store and not an option of the browser's. While it holds,
    /// no editing action is offered and the state slot names it.
    read_only: Option<ReadOnly>,
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
        // Asked before the first frame, beside the readability check the store
        // was opened with: a session that cannot write must not offer to.
        let read_only = data::read_only(&store);
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
            read_only,
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

    /// Why the store may not be written, or `None` while it may be.
    pub fn read_only(&self) -> Option<ReadOnly> {
        self.read_only
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
            Some(Modal::Help) | None => match (&self.surface, self.editing.is_some()) {
                // Its shape travels with the mode, because the key map decides from
                // it which keys move between fields and which key writes a line
                // break — never how the surface is accepted, which is one key
                // wherever the reader is standing.
                (Some(surface), _) => Mode::Surface(surface.shape()),
                (None, true) => Mode::Editing,
                (None, false) => Mode::Browse,
            },
        }
    }

    /// The hints editing mode's strip may drop: the actions the frozen row
    /// offers, in the key map's own order.
    ///
    /// Only actions the browser believes it can perform are ever listed, and one
    /// predicate decides both what is listed and what the key does — so the strip
    /// can never name a letter that performs nothing on the row, nor hide one
    /// that performs something.
    pub fn editing_hints(&self) -> Vec<&'static str> {
        keymap::FOOTER_HINTS_EDITING
            .iter()
            .filter(|(action, _)| self.offers(*action))
            .map(|(_, hint)| *hint)
            .collect()
    }

    /// Whether the frozen row offers an editing action, which is exactly when the
    /// strip lists its hint. Derived from the offer itself, so a hint and the key
    /// it names cannot disagree about a row.
    ///
    /// A signpost is not an offer: it performs nothing, so a hint naming its
    /// letter would teach a key that writes nothing — and a letter that performs
    /// nothing raises a notice saying why, which is what nothing happening looks
    /// like everywhere else in the mode.
    fn offers(&self, action: EditingAction) -> bool {
        matches!(
            self.offer(action),
            Some(Offer::Ask(_) | Offer::Fill(_) | Offer::Compose(_) | Offer::Perform { .. })
        )
    }

    /// Who holds the claim on the frozen row, or `None` while nobody does — which
    /// includes a row that cannot be claimed at all.
    ///
    /// Taken off the row the mode froze, which is the same value the marker beside
    /// that row is drawn from, so an offer and the mark the reader is looking at
    /// cannot disagree. Nothing is read from the store for it: a row is rebuilt
    /// from the store on every listing, and the offer is asked on every frame.
    ///
    /// The row is matched against what the mode is acting on rather than trusted to
    /// be it, so a holder can never arrive from a row the mode is not on — and only
    /// a row that stands for work carries one at all.
    fn frozen_holder(&self, target: &Selection) -> Option<&str> {
        let row = self.nav.frame().current()?;
        if row.selection != *target {
            return None;
        }
        match &row.kind {
            data::RowKind::Work { claimed_by, .. } => claimed_by.as_deref(),
            data::RowKind::Collection(_)
            | data::RowKind::Member
            | data::RowKind::Comment { .. }
            | data::RowKind::Withdrawn
            | data::RowKind::Unreadable => None,
        }
    }

    /// Whether the frozen row is a live comment the human wrote, which is the whole
    /// of whether a comment may be rewritten or withdrawn from here.
    ///
    /// Three rules meet in it: a comment is its author's alone to change, the
    /// browser writes as the human and only the human, and a comment already
    /// withdrawn has no text to rewrite and cannot be withdrawn twice. Where the
    /// answer is no the letters are simply absent — the reader is never told the
    /// rule by the browser, which is the accepted cost of offering only what it
    /// believes it can perform.
    ///
    /// Read off the row the mode froze and matched against what the mode is acting
    /// on, exactly as a claim's holder is: an author can never arrive from a row the
    /// mode is not on, and the reader is looking at the same value the offer turns
    /// on — the row names its own author.
    fn frozen_comment_is_the_humans(&self, target: &Selection) -> bool {
        let Some(row) = self.nav.frame().current() else {
            return false;
        };
        if row.selection != *target {
            return false;
        }
        match &row.kind {
            data::RowKind::Comment { by_the_human } => *by_the_human,
            data::RowKind::Work { .. }
            | data::RowKind::Collection(_)
            | data::RowKind::Member
            | data::RowKind::Withdrawn
            | data::RowKind::Unreadable => false,
        }
    }

    /// What an editing action opens on the frozen row, or `None` where the row does
    /// not offer that action.
    ///
    /// Invariant: this is the whole of what a letter does on a row — both the hint
    /// strip and the key ask it — and it is exhaustive over the editing actions, so
    /// an action added without deciding which rows offer it does not compile rather
    /// than showing a hint no key answers. A row that performs nothing but has
    /// something to say says it here too, so the wording of a letter's outcome and
    /// the decision that it performs nothing cannot be made in two places.
    ///
    /// A question names the object, because the frozen row is dimmed and the
    /// members of a collection read alike: an unnamed question would not say what
    /// is about to go. The notice names it for the same reason — by the time it
    /// is read the row is gone.
    fn offer(&self, action: EditingAction) -> Option<Offer> {
        // A store the format gate will not let this binary write offers no row
        // anything, the rows that would only signpost the command line included:
        // what is offered is what the browser believes it can perform, and the
        // store has already said it cannot. Decided here rather than beside the
        // key or the hint, because this is the whole of what a letter does on a
        // row: one answer decides what the letter performs and whether it is
        // hinted, so the two cannot come to disagree about a store that refuses
        // every write.
        if self.read_only.is_some() {
            return None;
        }
        let target = self.editing.as_ref()?;
        match action {
            EditingAction::Add => match target {
                // Creation acts on the container row the cursor stands on, and an
                // epic and a ticket are containers of units of work: an epic's row
                // makes a top-level ticket of that epic, a ticket's row a subticket
                // of that ticket. There is no "new member of this level" action
                // beside it, because leaving a level lands the cursor back on the
                // row that contains it.
                Selection::Epic(_) | Selection::Node(_) => {
                    Some(Offer::Fill(Surface::create_node(target.clone())))
                }
                // Creation acts on the container row the cursor stands on, so a
                // label is added from the label set's own row and nowhere else.
                Selection::Collection(container, Collection::Labels) => Some(Offer::Fill(
                    Surface::add_label(target.clone(), container.selection().reference()),
                )),
                // A comment is prose, so it is written in the pane where the row it
                // is about stays visible. The container's row is where it is added,
                // for the same reason a label is added from the label set's row.
                Selection::Collection(container, Collection::Comments) => Some(Offer::Fill(
                    Surface::add_comment(target.clone(), container.selection().reference()),
                )),
                // A dependency list belongs to a node: an epic is not a unit of
                // work that can be blocked, so it carries no such list and is
                // never offered one.
                Selection::Collection(container @ Container::Node(_), Collection::BlockedBy) => {
                    Some(Offer::Fill(Surface::add_blocker(
                        target.clone(),
                        container.selection().reference(),
                    )))
                }
                // Attaching a payload is picking a file and carrying bytes about,
                // which the command line does and the browser does not do at all,
                // so this row performs nothing and names the command that does the
                // job — the container's own, since an epic's assets and a node's
                // assets are different commands.
                //
                // The command comes first and the prose after it, because a notice
                // is one line and clipping eats the tail: a reader who has to retype
                // something needs all of it, and losing the words in front of it
                // costs them nothing. A long reference on a narrow terminal would
                // otherwise take the flag off the end.
                Selection::Collection(container, Collection::Assets) => {
                    Some(Offer::Signpost(format!(
                        "loti {} asset add {} --file <path> — assets are attached from the command line",
                        container.cli_noun(),
                        container.selection().reference(),
                    )))
                }
                _ => None,
            },
            // Only an epic and a node have a name, a summary and a body of their
            // own: a collection and its members are edited by their own operations,
            // so no row of one offers these letters. The text itself is not fetched
            // here — the hint strip asks this on every frame, and a read belongs to
            // the keypress.
            //
            // The letter that names a row's long-form text is the exception: on a
            // comment row it reaches the comment's own text, so which field it means
            // is decided here, by the row, rather than by the key. It is offered only
            // on a live comment the human wrote — a comment is its author's alone to
            // rewrite — so on anyone else's the letter is simply not there.
            EditingAction::Edit(field) => match (field, target) {
                (_, Selection::Epic(_) | Selection::Node(_)) => {
                    Some(Offer::Compose(Composed::Field(field)))
                }
                (data::FreeForm::Body, Selection::Comment(..))
                    if self.frozen_comment_is_the_humans(target) =>
                {
                    Some(Offer::Compose(Composed::CommentText))
                }
                _ => None,
            },
            // Only an epic and a node have a state of their own: a collection is
            // structure and its members carry no state, so no row of one offers the
            // letter. What the states are, which one the row is in, and how much a
            // cascade would close are all read when the letter is pressed — the hint
            // strip asks this offer on every frame.
            EditingAction::SetState => match target {
                Selection::Epic(_) | Selection::Node(_) => Some(Offer::Compose(Composed::State)),
                _ => None,
            },
            // A claim is taken on a unit of work, so an epic's row is never offered
            // one and neither is a collection or one of its members. Taking is
            // reassigning — a claim has one holder — so a held claim is offered the
            // same letter as an unheld one, and the surface asks who is taking it.
            EditingAction::TakeClaim => match target {
                Selection::Node(_) => Some(Offer::Fill(Surface::take_claim(target.clone()))),
                _ => None,
            },
            // The other half of the same noun, offered only while a claim is held:
            // there is nothing to give up on a row nobody is on. Nothing is typed
            // and nothing is asked — the row says who holds it, so the letter writes.
            EditingAction::ReleaseClaim => match target {
                Selection::Node(_) => {
                    let holder = self.frozen_holder(target)?;
                    let reference = target.reference();
                    Some(Offer::Perform {
                        write: data::Write::ReleaseClaim(target.clone()),
                        // The notice names the ticket and who was on it, because by
                        // the time it is read the mark it removed is gone from the
                        // row.
                        done: format!("claim on {reference} released by {holder}"),
                    })
                }
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
                // A dependency list has no rename either, so an entry is only ever
                // removed. The row names both nodes, so removing one asks for no
                // reference: the entry that goes is the entry under the bar.
                Selection::Blocker(..) => {
                    let blocker = target.reference();
                    Some(Offer::Ask(Dialog::confirm(
                        format!("Remove blocker {blocker}?"),
                        "remove",
                        Performs::Write {
                            write: data::Write::RemoveBlocker(target.clone()),
                            done: format!("blocker {blocker} removed"),
                        },
                        "cancel",
                    )))
                }
                // An asset cannot be added or replaced from the browser, so it is
                // only ever deleted — and the deletion is hard: the bytes go with
                // the entry, and this question is the only thing in front of them.
                Selection::Asset(_, name) => Some(Offer::Ask(Dialog::confirm(
                    format!("Delete asset {name}?"),
                    "delete",
                    Performs::Write {
                        write: data::Write::DeleteAsset(target.clone()),
                        done: format!("asset {name} deleted"),
                    },
                    "cancel",
                ))),
                // A comment is withdrawn rather than removed — the store keeps the
                // slot, so the number stays taken — and only its author may withdraw
                // it, so the letter is absent on anyone else's and on one already
                // withdrawn. It is named by its number, which is the only name it
                // has and the one it keeps.
                Selection::Comment(_, id) if self.frozen_comment_is_the_humans(target) => {
                    Some(Offer::Ask(Dialog::confirm(
                        format!("Delete comment {id}?"),
                        "delete",
                        Performs::Write {
                            write: data::Write::DeleteComment(target.clone()),
                            done: format!("comment {id} deleted"),
                        },
                        "cancel",
                    )))
                }
                _ => None,
            },
        }
    }

    /// Carry out an intent. Returns whether the browser should exit.
    ///
    /// While an overlay is open it takes every key, so a keypress can never move
    /// an unseen cursor. Editing mode is the layer under it, and unwinding takes
    /// one layer at a time: closing the overlay leaves the mode standing.
    ///
    /// Ending the session is decided before any layer is consulted, because it is
    /// the one intent no layer may answer for itself: see [`App::quit`].
    ///
    /// The one failure this returns is a level that could not be listed at all.
    /// It is not the session's end: the caller reports it through
    /// [`App::store_unreadable`] and the browser goes on showing what it has. Every
    /// other layer here is infallible, so no store failure can reach a caller
    /// from a path that has already written something.
    pub fn apply(&mut self, action: Action) -> Result<bool> {
        if matches!(action, Action::Quit) {
            return Ok(self.quit());
        }
        if self.modal.is_some() {
            self.apply_to_modal(action);
            return Ok(false);
        }
        // An open surface is the layer under an overlay and above the mode that
        // opened it: every key belongs to the field while it is open.
        if self.surface.is_some() {
            self.apply_to_surface(action);
            return Ok(false);
        }
        if self.editing.is_some() {
            self.apply_editing(action);
            return Ok(false);
        }

        match action {
            Action::ToggleHelp => self.modal = Some(Modal::Help),

            // The key map found nothing to bind this key to, in any mode: it
            // reaches this layer rather than being dropped by the loop, and
            // browsing answers it exactly as it answers a recognised action it
            // has nothing to do with — silence. See [`Action::Unbound`] for the
            // rule this is one instance of, and for where the instances that
            // answer differently live.
            Action::Unbound => {}

            // Zoom hides the navigation pane, so the motion keys fall through to
            // the preview: they must never move a cursor the reader cannot see. The
            // wheel shares every arm with the key it stands in for here: browsing
            // and zoom never tell the two apart, only editing mode does.
            Action::CursorDown | Action::WheelDown if self.zoomed => {
                self.preview.viewer.scroll_down(1)
            }
            Action::CursorUp | Action::WheelUp if self.zoomed => self.preview.viewer.scroll_up(1),
            Action::CursorFirst if self.zoomed => self.preview.viewer.scroll_to_top(),
            Action::CursorLast if self.zoomed => self.preview.viewer.scroll_to_bottom(),
            // Silent, deliberately, and named in [`Action::Unbound`] as one of
            // the two places that look like the surprise a notice exists for
            // and are not: entering, leaving and unwinding a level normally
            // change what is on screen, but with the navigation pane hidden
            // there is no visible cursor for a reader to expect them to move —
            // unlike `EnterEditing` below, which opens a mode a reader has
            // every reason to expect works wherever they are.
            Action::Descend | Action::Ascend | Action::Unwind if self.zoomed => {}
            // The same rule, said out loud because nothing on screen would say
            // it: an action that needs a visible cursor does nothing while there
            // is none. Editing mode needs one twice over — to freeze it, and to
            // show which row is frozen — and none of the marks it would show for
            // that exist without the navigation pane. The screen is the reader's
            // choice, so the refusal leaves it as it is rather than un-zooming.
            Action::EnterEditing if self.zoomed => self.flash(EDITING_NEEDS_THE_NAV_PANE),

            Action::CursorDown | Action::WheelDown => self.nav.cursor_down(),
            Action::CursorUp | Action::WheelUp => self.nav.cursor_up(),
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
            | Action::Edit(_)
            | Action::SetState
            | Action::TakeClaim
            | Action::ReleaseClaim
            | Action::Overwrite
            | Action::Accept
            | Action::ExternalEditor
            | Action::Insert(_)
            | Action::DeleteBefore
            | Action::DeleteAfter
            | Action::MoveLeft
            | Action::MoveRight
            | Action::MoveToStart
            | Action::MoveToEnd
            | Action::MoveUp
            | Action::MoveDown
            | Action::NextField
            | Action::PreviousField => {}

            // Ending the session was answered before this layer was reached, so
            // no layer states the rule a second time. See [`App::quit`].
            Action::Quit => {}

            // An epic is created from the epics list and nowhere else: it has no
            // container row to be added from, which is why this is a key of the
            // browser's own rather than a letter a row offers.
            //
            // Being outside editing mode, it does not pass the offer table where
            // every other write's availability is decided — so it asks the store's
            // own state for itself. A store the format gate will not let this binary
            // write offers no write anywhere, this one included.
            //
            // Pressed anywhere but the roster it flashes rather than staying quiet:
            // the third case [`Action::Unbound`] names, where a bound key answers
            // for itself instead of following the default, because making an epic
            // is exactly what this key is expected to do everywhere else on that
            // list.
            Action::CreateEpic => match self.read_only {
                Some(reason) => self.flash(reason.refusal()),
                None => match self.nav.at_roster() {
                    true => self.surface = Some(Surface::create_epic()),
                    false => self.flash(EPICS_ARE_MADE_FROM_THE_EPICS_LIST),
                },
            },

            Action::EnterEditing => match self.read_only {
                // A mode whose every action is unavailable is not entered at
                // all: the key is as unknown as any other action the browser
                // cannot perform. The store's own words say why, because the
                // remedy is a store rule and a browser paraphrase of one goes
                // stale — and the state slot goes on saying the condition long
                // after this notice has gone.
                Some(reason) => self.flash(reason.refusal()),
                None => match self.nav.frame().current() {
                    Some(row) => self.editing = Some(row.selection.clone()),
                    // The roster of an empty store is the browser's one screen
                    // with no selection, and the mode acts on a row.
                    None => self.flash("nothing to edit: this store has no epics"),
                },
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

            Action::Reload => self.reload(),
        }
        Ok(false)
    }

    /// Whether the session ends where the reader is standing, and the notice for
    /// where it does not.
    ///
    /// Invariant: quitting is decided here for every layer and answered nowhere
    /// else, so no layer can let it through on behalf of the layer beneath — an
    /// overlay is drawn over a mode, not a hole in it.
    ///
    /// The rule the layers are checked against is about unsaved work rather than
    /// about which key is live: **quitting never discards text the store has not
    /// been given.** Two layers can hold some — an open buffer, and editing mode,
    /// which is where all but one buffer is opened from — so while either stands
    /// the session stays and the notice names the way out of that layer, whatever
    /// is layered over it.
    ///
    /// A dialog refuses it in silence: the question is on screen listing its own
    /// answers, it is raised only for something failed or costly, and it must be
    /// answered rather than escaped past.
    fn quit(&mut self) -> bool {
        if matches!(self.modal, Some(Modal::Dialog(_))) {
            return false;
        }
        // A buffer holds text the store has never been given, and one of them — the
        // form that creates an epic — is open outside editing mode, so the buffer
        // is checked for itself rather than through the mode that usually holds it.
        if self.surface.is_some() {
            self.flash(NOT_A_WAY_OUT_OF_A_BUFFER);
            return false;
        }
        if self.editing.is_some() {
            self.flash(NOT_AN_EDITING_ACTION);
            return false;
        }
        true
    }

    /// Carry out an intent while an overlay is open.
    ///
    /// The key overlay is a layer above whichever mode raised it, so it is closed
    /// by that mode's own way out. A dialog is not: it admits the answers it lists
    /// and nothing else, because a question is raised only for something failed or
    /// costly, and it must be answered rather than escaped past. Neither of them
    /// ends the session — that is decided before an overlay is consulted, so this
    /// cannot return an exit.
    fn apply_to_modal(&mut self, action: Action) {
        if matches!(self.modal, Some(Modal::Help)) {
            match action {
                Action::ToggleHelp | Action::Unwind | Action::Ascend => self.modal = None,
                _ => {}
            }
            return;
        }
        // A dialog admits its listed answers and nothing else: nothing underneath
        // it may move while it is open.
        //
        // Which intent goes ahead is the open dialog's answer set to say, so no
        // intent is named here: a second question with a second affirmative answer
        // is a set that names its own intent, and one dialog's answer can never
        // perform another's — the letter that throws a label away is not the letter
        // that overwrites somebody else's text.
        if self.dialog_answers().and_then(Answers::affirmative) == Some(action) {
            self.answer();
            return;
        }
        // A refused write leaves the editing session standing: only a successful
        // write ends it, so dismissing lands back in the mode.
        if matches!(action, Action::Unwind) {
            self.dismiss();
        }
    }

    /// The answers the open dialog admits, or `None` when what is open is not a
    /// dialog.
    fn dialog_answers(&self) -> Option<Answers> {
        match &self.modal {
            Some(Modal::Dialog(dialog)) => Some(dialog.answers),
            Some(Modal::Help) | None => None,
        }
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
    fn answer(&mut self) {
        // A dialog that only reports has a way out and no answer, so nothing it
        // was raised for can be performed: it stands until it is dismissed.
        let Some(Modal::Dialog(dialog)) = &self.modal else {
            return;
        };
        let Some(answer) = dialog.affirmative.clone() else {
            return;
        };
        self.modal = None;
        match answer.performs {
            Performs::Write { write, done } => self.commit(&write, done),
            // Nothing reaches the store, and the mode stays on its frozen row: the
            // way out unwinds one layer at a time, and the surface is the layer
            // that was asked about.
            Performs::Discard => self.surface = None,
        }
    }

    /// Write, and report what happened.
    ///
    /// A successful write ends the editing session, the surface with it, and says
    /// what it did. A refused one keeps everything: the surface stays open with its
    /// text, so the reader can fix it or carry it out through the external editor —
    /// only a successful write ends the session.
    ///
    /// Invariant: every changed outcome reloads before the result is shown. Most
    /// changed writes succeed, but a cascade can close descendants and then refuse;
    /// keeping that decision with the typed outcome prevents success and partial
    /// failure from acquiring separate refresh rules.
    fn commit(&mut self, write: &data::Write, done: String) {
        let outcome = data::perform(&self.store, write);
        let changed = match &outcome {
            Ok(_) => true,
            Err(refusal) => refusal.changed(),
        };
        if changed {
            self.reload();
        }
        let refusal = match outcome {
            Ok(effect) => {
                self.surface = None;
                self.editing = None;
                self.flash(reported(done, effect));
                return;
            }
            Err(refusal) => refusal,
        };
        let dialog = match refusal {
            // The one refusal the reader is asked about rather than told: the entity
            // moved on under the buffer, and which text survives is theirs to
            // decide. Told apart by what the seam classified, never by reading a
            // message.
            data::Refusal::Conflict => {
                Dialog::conflict(&write.target().reference(), write.clone(), done)
            }
            // The store's own words, so the browser and the CLI teach the same
            // rule in the same words and the browser cannot go stale when a
            // store rule gains a nuance.
            //
            // A rule refusal can be the version gate itself — a migration can
            // start or finish while the mode is open — and the writability
            // marker is a snapshot from whenever it was last asked, not a
            // licence that stays valid until the next reload. So an unchanged
            // refusal is asked again here rather than left to go on claiming a
            // write is possible until something else happens to reload.
            data::Refusal::Rule(message) => {
                self.read_only = data::read_only(&self.store);
                Dialog::refusal(message)
            }
            // Reloading partial progress happened with every other changed
            // outcome above; the critical refusal still wins over a notice.
            data::Refusal::Partial(message) => Dialog::refusal(message),
        };
        self.modal = Some(Modal::Dialog(Box::new(dialog)));
    }

    /// Carry out an intent while a surface is open.
    ///
    /// Every key belongs to the field except the surface's own few: accept, the
    /// external editor, help, and the way out. There is no unknown-key notice here
    /// — in a field an unbound key is simply not a character, and the mode's notice
    /// belongs to the layer where letters are actions. The reflex key is the named
    /// exception in [`Action::Unbound`]: outside a field that holds many lines it
    /// carries no action and lands in the field's own catch-all with everything
    /// else this layer ignores, silent so the notice it would otherwise deserve
    /// cannot cover the hint strip's save key.
    fn apply_to_surface(&mut self, action: Action) {
        match action {
            // The text is the only copy of what the reader wrote, so the way out of
            // a buffer with typing in it asks first; an untouched one is not worth
            // a question and goes at once.
            Action::Unwind => match self.surface.as_ref().and_then(Surface::dirtied) {
                Some(field) => {
                    self.modal = Some(Modal::Dialog(Box::new(Dialog::discard(&field.label))));
                }
                None => self.surface = None,
            },
            Action::Accept => self.accept(),
            // Only a field made of text has anything to hand over. The key is not
            // bound in a picker at all, so this is the same answer said twice rather
            // than a rule the field has to enforce.
            Action::ExternalEditor => {
                if let Some(text) = self
                    .surface
                    .as_ref()
                    .and_then(|surface| surface.fields[surface.focus()].text())
                {
                    self.editor_handoff = Some(text.to_string());
                }
            }
            // Moving between fields is the surface's business rather than a field's,
            // and it is not a content key: the field it leaves is no dirtier for
            // having been left.
            Action::NextField | Action::PreviousField => {
                if let Some(surface) = &mut self.surface {
                    surface.move_focus(matches!(action, Action::NextField));
                }
            }
            // The key list is reachable from inside a field, which is what the help
            // key that survives a text field is for: the list is a layer above the
            // surface, so closing it leaves the buffer exactly as it was.
            Action::ToggleHelp => self.modal = Some(Modal::Help),
            _ => {
                if let Some(surface) = &mut self.surface {
                    surface.apply(action);
                }
            }
        }
    }

    /// Accept the open surface: check the store has something to be given, then
    /// write.
    ///
    /// The check is not a store rule reimplemented — what makes a value acceptable
    /// is the store's judgement and its refusal is shown verbatim — it is the
    /// browser refusing to send a field the reader never filled in, and saying
    /// which field that is rather than which rule was broken.
    fn accept(&mut self) {
        let Some(surface) = &self.surface else {
            return;
        };
        if let Some(index) = surface.unfilled() {
            let dialog = Dialog::required(&surface.fields[index].label, index);
            self.modal = Some(Modal::Dialog(Box::new(dialog)));
            return;
        }
        // Emptiness first, then the shape of what is there: a field with nothing in
        // it is not a field with the wrong thing in it, and a rule about a value has
        // nothing to say where there is none.
        if let Some((index, why)) = surface.rejected() {
            self.modal = Some(Modal::Dialog(Box::new(Dialog::rejected(why, index))));
            return;
        }
        let (write, done) = surface.write();
        self.commit(&write, done);
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
    /// reload. A key bound to some other action — a motion, a letter this row
    /// does not offer — is answered with a notice naming the way out, because with
    /// the selection frozen there is nothing left for it to do and an unknown key
    /// is deliberately not an implicit exit: a typo must not silently drop the
    /// reader out of a mode whose indicator is at the top of the screen while
    /// their eyes are on the row. A key bound to nothing at all, in any mode,
    /// stays silent instead — see [`Action::Unbound`] — rather than raising the
    /// same notice for a stray keystroke that could never have started anything
    /// here.
    ///
    /// Quitting never reaches this layer: it is answered for every layer at once,
    /// and the mode holds text the store has not been given, so it is refused
    /// while the mode is on whatever is layered over it. See [`App::quit`].
    fn apply_editing(&mut self, action: Action) {
        match action {
            Action::Unwind => self.editing = None,
            Action::ToggleHelp => self.modal = Some(Modal::Help),
            Action::Unbound => {}
            // Nothing is pending at this layer, so a reload is safe — and it is
            // the natural move when the preview looks stale before committing to
            // an edit.
            Action::Reload => {
                self.reload();
                // The reload may have ended the mode already, because it found
                // the store may no longer be written. The row is beside the
                // point then, and the notice that said so must not be replaced
                // by one about a row: the reader's next question is about the
                // store, not about where the cursor went.
                if self.editing.is_some() {
                    let target = self.nav.frame().current().map(|row| &row.selection);
                    if target != self.editing.as_ref() {
                        // The mode acts on one row, so a row that is gone ends
                        // it. Where the cursor lands instead is the ordinary
                        // reload fallback's business: the mode invents no second
                        // recovery.
                        self.editing = None;
                        self.flash("the row you were editing is gone");
                    }
                }
            }
            // A wheel event is not a key: the notice below is worded for one
            // ("not an editing action") and would be wrong for a scroll, so the
            // mode answers it exactly as it answers being zoomed with no cursor to
            // move — silence, because the mode having frozen the selection is not
            // something the reader did anything wrong to run into.
            Action::WheelDown | Action::WheelUp => {}
            // Everything else is an editing action or nothing. A letter is listed
            // only where the row offers it, so a letter this row does not offer is
            // unknown here — and the offer that decides it is the one the hint strip
            // asked — but it is a key the mode does bind, just not to something this
            // row can do, which is what tells it apart from [`Action::Unbound`]
            // above and earns it the notice that names the way out.
            _ => match EditingAction::for_intent(action).and_then(|a| self.offer(a)) {
                Some(Offer::Ask(dialog)) => self.modal = Some(Modal::Dialog(Box::new(dialog))),
                Some(Offer::Fill(surface)) => self.surface = Some(surface),
                Some(Offer::Compose(composed)) => self.compose(composed),
                // Nothing to fill in and nothing to ask: the row carried the whole
                // write, so the letter is the whole interaction.
                Some(Offer::Perform { write, done }) => self.commit(&write, done),
                // Nothing is written and nothing opens: the row said where the job
                // is done instead, which is the same channel as any other reason
                // nothing happened.
                Some(Offer::Signpost(notice)) => self.flash(notice),
                None => self.flash(NOT_AN_EDITING_ACTION),
            },
        }
    }

    /// Open a surface on one field's text as the store holds it, re-read at this
    /// instant.
    ///
    /// The read happens here rather than when the mode was entered or when the
    /// cursor last moved: the surface must start from the current text, and the stamp
    /// it carries has to be as fresh as the edit is, or the window a conflict is
    /// reported for would be "since you last pressed a motion key" — minutes of
    /// browsing before any typing began.
    ///
    /// A read that fails leaves the mode standing and says what could not be read:
    /// the entity may have gone between the letter being offered and pressed, which
    /// is the same class of thing as any other part of a store the browser cannot
    /// read.
    fn compose(&mut self, composed: Composed) {
        let Some(target) = self.editing.clone() else {
            return;
        };
        let opened = match composed {
            Composed::Field(field) => data::edit_target(&self.store, &target).map(|read| {
                Surface::replace(
                    data::Replaceable::Field(field),
                    read.selection.clone(),
                    field.of(&read).to_string(),
                    read.stamp,
                )
            }),
            Composed::CommentText => data::comment_target(&self.store, &target).map(|read| {
                Surface::replace(
                    data::Replaceable::CommentText,
                    read.selection,
                    read.text,
                    read.stamp,
                )
            }),
            Composed::State => data::state_target(&self.store, &target).map(Surface::set_state),
        };
        match opened {
            Ok(surface) => self.surface = Some(surface),
            Err(e) => self.store_unreadable(e.to_string()),
        }
    }

    /// Re-read every level from the store, and ask again whether it may be
    /// written.
    ///
    /// The writability question belongs here rather than at startup: an agent can
    /// migrate a store, or begin migrating one, while the browser is open, so
    /// read-only is a state a session enters and leaves rather than a verdict
    /// reached once. A reload that finds it entered ends the editing session,
    /// because nothing the mode could do is offered any more, and says so once —
    /// the state slot then carries the condition for as long as it holds.
    ///
    /// Re-reading cannot fail: a level the store could not be read for keeps the
    /// rows it has. That is what makes this safe to run straight after a write has
    /// committed — no outcome of it can stand between the write and the notice
    /// that tells the reader what was written.
    fn reload(&mut self) {
        let store = &self.store;
        self.nav.reload(|level| data::rows(store, level));
        self.read_only = data::read_only(&self.store);
        if self.read_only.is_some() && self.editing.is_some() {
            self.editing = None;
            self.flash(EDITING_STOPPED_READ_ONLY);
        }
    }

    /// Report a part of the store that could not be read, keeping the session.
    ///
    /// A browser is most useful on a store that is not entirely readable, so
    /// nothing the store does to a read ends a session: what could not be read is
    /// reported and whatever was already on screen stays there. A dialog rather
    /// than the transient notice channel, because corruption is a thing the reader
    /// has to act on, and it carries the whole failure chain — the outermost words
    /// name what was attempted and the cause under them is the part that can be
    /// acted on.
    pub fn store_unreadable(&mut self, message: String) {
        self.modal = Some(Modal::Dialog(Box::new(Dialog::report(
            UNREADABLE_TITLE,
            message,
            Dismissal {
                word: "dismiss",
                performs: None,
            },
        ))));
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
    ///
    /// Refused only while a dialog is open: a question is the one case where
    /// "nothing beneath it moves" is a real guarantee, because a dialog is a
    /// question the reader must answer before anything else proceeds. The split
    /// itself is neither what the mode has frozen nor what a question is waiting
    /// on — it is the reader's own furniture — so the drag stays live through
    /// editing mode and through an open surface alike, including a surface that
    /// draws its buffer over the very pane the drag resizes.
    pub fn press(&mut self, column: u16) -> bool {
        if matches!(self.mode(), Mode::Dialog(_)) {
            return false;
        }
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
    ///
    /// Whether to keep the scroll position is decided on document identity —
    /// [`Selection::document`] — not on the row's own selection: a cursor move
    /// between rows that share a document (a container's collection rows, and
    /// the label rows inside a collection) cannot change what is shown, so it
    /// must not move the reader either.
    pub fn sync_preview(&mut self, width: u16) {
        let target = self.nav.preview_target();
        let document = target.as_ref().map(Selection::document);
        let width_changed = width != self.preview.width;
        if !width_changed && document == self.preview.shown {
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
        if document != self.preview.shown {
            self.preview.viewer.scroll_to_top();
            self.preview.shown = document;
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
    use crate::data::{FreeForm, RowKind};
    use crate::theme::Theme;
    use loti_core::lock::{self, LockConfig};
    use loti_core::ops::{self, NewNode};
    use loti_core::NodeState;
    use std::time::Duration;

    /// The browser on the shared fixture store. The fixture is returned with it
    /// because the store is deleted when the fixture is dropped.
    fn app() -> (Fixture, App) {
        let fx = Fixture::build();
        let app = App::new(fx.store.clone(), Theme::with_color(false)).unwrap();
        (fx, app)
    }

    #[test]
    fn a_partial_refusal_reloads_the_rows_held_behind_its_dialog() {
        let fx = Fixture::build();
        let store = Store::at(fx.store.root()).with_lock_config(LockConfig {
            stale_threshold: Duration::from_millis(80),
            retry_interval: Duration::from_millis(5),
        });
        // Keep a second target locked so the first descendant commits before the
        // cascade reaches its controlled failure.
        let tail = ops::create_node(
            &store,
            NewNode {
                epic_id: fx.epic.clone(),
                parent: Some(fx.subnode.clone()),
                name: "cascade tail".into(),
                summary: String::new(),
                labels: Vec::new(),
                body: String::new(),
            },
        )
        .expect("the tail can be created");
        let held = lock::try_acquire(&store.node_path(&fx.epic, tail.frontmatter.number))
            .expect("the tail lock can be taken")
            .expect("the tail was unlocked");
        let mut app = App::new(store, Theme::with_color(false)).unwrap();

        // The level behind the dialog is already open. Calling the write boundary
        // here pins its obligation to refresh every held level, not a later descent
        // that would load the changed descendant anew.
        app.apply(Action::Descend).unwrap();
        to_work_row(&mut app);
        app.apply(Action::Descend).unwrap();
        app.commit(
            &data::Write::SetState {
                target: fx.node_selection(),
                state: data::State::Work(NodeState::Closed),
                reason: "obsolete".into(),
                cascade: true,
            },
            "ignored".into(),
        );
        // Release before any assertion so a failure cannot leave fixture cleanup
        // waiting on the controlled refusal.
        drop(held);

        assert!(
            matches!(app.modal(), Some(Modal::Dialog(_))),
            "no refusal dialog opened"
        );
        let child = app
            .nav()
            .rows()
            .iter()
            .find(|row| row.selection == fx.subnode_selection())
            .expect("the first cascade descendant row behind the dialog");
        assert!(
            matches!(&child.kind, RowKind::Work { status, .. } if status == "closed"),
            "the held rows still show the state before the partial cascade: {child:?}"
        );
    }

    #[test]
    fn a_single_descendant_is_named_in_the_singular_on_every_surface() {
        assert_eq!(cascade_label(1), "also close 1 open descendant");
        assert_eq!(
            reported(
                "browser/1 is now closed".to_string(),
                data::Effect::AlsoClosed(1)
            ),
            "browser/1 is now closed, and 1 descendant with it"
        );
        assert_eq!(cascade_label(2), "also close 2 open descendants");
        assert_eq!(
            reported(
                "browser/1 is now closed".to_string(),
                data::Effect::AlsoClosed(2)
            ),
            "browser/1 is now closed, and 2 descendants with it"
        );
    }

    #[test]
    fn a_destructive_question_carries_its_own_title() {
        // The question's message names what would go, but the fixed title names
        // what kind of interruption this is. A confirmation must not borrow the
        // refusal title just because both are centred dialogs.
        let dialog = Dialog::confirm(
            "Remove label ui?".to_string(),
            "remove",
            Performs::Discard,
            "cancel",
        );
        assert_eq!(dialog.title(), CONFIRM_TITLE);
        assert_ne!(dialog.title(), REFUSAL_TITLE);
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
        to_row(app, |kind| matches!(kind, RowKind::Work { .. }));
    }

    /// Walk back out to the epic roster, so a test that has been somewhere already
    /// can still say where it goes next from the top.
    ///
    /// Bounded, because repeating an intent until a condition holds is a hang when
    /// the intent stops moving the reader: the bound is one level per crumb there
    /// were to leave, so an intent that leaves none turns the whole suite from a
    /// silent spin into one failure that says what it was waiting for.
    fn to_the_roster(app: &mut App) {
        for _ in 0..app.nav().crumbs().len() {
            if app.nav().crumbs().len() == 1 {
                return;
            }
            app.apply(Action::Ascend).unwrap();
        }
        assert_eq!(
            app.nav().crumbs().len(),
            1,
            "leaving a level stopped reaching the roster: {:?}",
            app.nav().crumbs()
        );
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

    /// Stand on the ticket's own `blocked-by` row, which is where an addition is
    /// offered. A dependency list belongs to a node — an epic is not a unit of work
    /// that can be blocked — so it is a level deeper than the epic's collections.
    fn to_the_blocked_by_row(app: &mut App) {
        to_the_roster(app);
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(app);
        app.apply(Action::Descend).unwrap(); // into the ticket
        to_row(
            app,
            |kind| matches!(kind, RowKind::Collection(c) if c.name() == "blocked-by"),
        );
    }

    /// Stand on the first entry of that list, which is where a removal is offered.
    fn to_a_blocker_row(app: &mut App) {
        to_the_blocked_by_row(app);
        app.apply(Action::Descend).unwrap();
    }

    /// Stand on the epic's own `assets` row, which is the row an addition would be
    /// offered on if the browser attached payloads at all.
    fn to_the_assets_row(app: &mut App) {
        to_the_roster(app);
        app.apply(Action::Descend).unwrap(); // into the epic
        to_row(
            app,
            |kind| matches!(kind, RowKind::Collection(c) if c.name() == "assets"),
        );
    }

    /// Stand on the first asset of the epic's own assets level, which is where a
    /// deletion is offered.
    fn to_an_asset_row(app: &mut App) {
        to_the_assets_row(app);
        app.apply(Action::Descend).unwrap();
    }

    /// Open the blocker surface, the way a reader does: freeze the dependency
    /// list's row and press the letter that adds a member to it.
    fn open_the_blocker_surface(app: &mut App) {
        to_the_blocked_by_row(app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Add).unwrap();
        assert!(app.surface().is_some(), "the add key opened no surface");
    }

    /// Add one blocker the way a reader does, by typing a reference into the
    /// surface the dependency list's row opens.
    fn add_a_blocker(app: &mut App, reference: &str) {
        open_the_blocker_surface(app);
        type_into(app, reference);
        app.apply(Action::Accept).unwrap();
        assert_eq!(app.modal(), None, "the store refused {reference:?}");
    }

    /// Freeze the epic's own row, on the roster, which is where its body is
    /// edited: the body of the row the mode acts on, not of a collection under it.
    fn freeze_the_epics_row(app: &mut App) {
        to_the_roster(app);
        app.apply(Action::CursorFirst).unwrap();
        app.apply(Action::EnterEditing).unwrap();
    }

    /// Open the body buffer, the way a reader does: freeze the epic's row and press
    /// the letter that edits its long-form text.
    fn open_the_body_buffer(app: &mut App) {
        freeze_the_epics_row(app);
        app.apply(Action::Edit(FreeForm::Body)).unwrap();
        assert!(app.surface().is_some(), "the body key opened no buffer");
    }

    /// Stand on the epic's own `comments` row, which is where a comment is added:
    /// creation acts on the container row the cursor stands on.
    fn to_the_comments_row(app: &mut App) {
        to_the_roster(app);
        app.apply(Action::Descend).unwrap(); // into the epic
        to_row(
            app,
            |kind| matches!(kind, RowKind::Collection(c) if c.name() == "comments"),
        );
    }

    /// Stand on the first comment of the epic's own comments level, which the
    /// fixture writes as the human.
    fn to_a_comment_row(app: &mut App) {
        to_the_comments_row(app);
        app.apply(Action::Descend).unwrap();
    }

    /// Stand on the comment row whose number this is, whoever wrote it: a comment
    /// is addressed by the number the store gave it and never by where it sits.
    fn to_the_comment_numbered(app: &mut App, id: u64) {
        to_the_comments_row(app);
        app.apply(Action::Descend).unwrap();
        let index = app
            .nav()
            .rows()
            .iter()
            .position(|row| row.label == id.to_string())
            .unwrap_or_else(|| panic!("comment {id} is not on the level"));
        app.apply(Action::CursorFirst).unwrap();
        for _ in 0..index {
            app.apply(Action::CursorDown).unwrap();
        }
    }

    /// Open the buffer that adds a comment, the way a reader does: freeze the
    /// comment list's row and press the letter that adds a member to it.
    fn open_the_comment_buffer(app: &mut App) {
        to_the_comments_row(app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Add).unwrap();
        assert!(app.surface().is_some(), "the add key opened no buffer");
    }

    /// Open the buffer that rewrites the human's own comment, the way a reader
    /// does: freeze its row and press the letter that edits a row's long-form text.
    fn open_the_comment_edit(app: &mut App) {
        to_a_comment_row(app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Edit(FreeForm::Body)).unwrap();
        assert!(app.surface().is_some(), "the text key opened no buffer");
    }

    /// Open the label surface, the way a reader does: freeze the label set's row
    /// and press the letter that adds a member to it.
    fn open_the_label_surface(app: &mut App) {
        to_the_labels_row(app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Add).unwrap();
        assert!(app.surface().is_some(), "the add key opened no surface");
    }

    /// The mode an open surface of this shape puts the keyboard under, so a test
    /// names the shape rather than assembling one.
    fn surface_mode(fields: Fields, kind: FieldKind) -> Mode {
        Mode::Surface(Shape { fields, kind })
    }

    /// What a field of text holds and where its cursor sits.
    ///
    /// A picker is a failure rather than an empty answer: a test about typed text
    /// must not quietly pass on a field nothing can be typed into.
    fn text_and_cursor(field: &Field) -> (&str, usize) {
        match field.shown() {
            Shown::Text { value, cursor } => (value, cursor),
            Shown::Pick { .. } => {
                panic!("{} is a picker rather than a field of text", field.label())
            }
        }
    }

    /// What a field of text holds; see [`text_and_cursor`].
    fn text_of(field: &Field) -> &str {
        text_and_cursor(field).0
    }

    /// Where a field of text's cursor sits, in characters from the start.
    fn cursor_of(field: &Field) -> usize {
        text_and_cursor(field).1
    }

    /// The values a picker offers and which of them it holds, which is its value.
    ///
    /// A field of text is a failure rather than an empty answer, for the same reason
    /// the other way round is.
    fn options_of(field: &Field) -> (Vec<&'static str>, usize) {
        match field.shown() {
            Shown::Pick { options, at } => (options, at),
            Shown::Text { .. } => {
                panic!("{} is a field of text rather than a picker", field.label())
            }
        }
    }

    /// The value a picker holds, which is the value it has marked.
    fn marked(field: &Field) -> &'static str {
        let (options, at) = options_of(field);
        options[at]
    }

    /// The name of a field invented for a test, so a rule about one field has a
    /// second field to be told apart from. Nothing writes it.
    const A_SECOND_FIELD: &str = "note";
    const A_THIRD_FIELD: &str = "reason";

    /// Open a surface with the fields a test gives it, which no shipped surface
    /// has: a shape built here is one no slice's own form has to be bent into.
    ///
    /// Two rules about fields are pinned on it — that a field being required is
    /// what makes the unfilled check fire, and that the field a dismissal points at
    /// is the field the reader lands in — because they are rules about any surface
    /// rather than about the one that happens to hold three fields today. It
    /// borrows a write that takes one value, and the field it writes is the first.
    fn open_a_surface_with_fields(app: &mut App, fields: Vec<Field>) {
        open_the_label_surface(app);
        let surface = app.surface.as_mut().expect("the surface is open");
        surface.fields = fields;
        surface.focus = 0;
    }

    /// The frame's lines as a reader reads them, drawn through the headless
    /// backend.
    ///
    /// Drawn from here because a surface with several fields exists only inside
    /// this module's tests, and a hint the reader cannot read off the screen is
    /// not a hint.
    fn frame_lines(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
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

    /// The preview pane's own lines — nothing else the frame draws — through the
    /// same headless backend as [`frame_lines`].
    ///
    /// The breadcrumb, the navigation pane and the footer all read off the row
    /// the cursor stands on, not the document the preview shows, so a test about
    /// what the reader of the document sees — whether a scroll carried over —
    /// must not let any of them stand in for it: a collection row and a label
    /// row of the same container draw different breadcrumbs and different
    /// navigation rows while showing the very same document, and comparing whole
    /// frames would blame the pane for a difference that is entirely theirs. The
    /// split mirrors [`crate::ui::draw`]'s own, so the slice taken here is the
    /// pane exactly as it draws.
    fn preview_lines(app: &mut App, width: u16, height: u16) -> Vec<String> {
        use ratatui::layout::{Constraint, Direction, Layout, Rect};

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();

        let area = Rect::new(0, 0, width, height);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(app.nav_percent()),
                Constraint::Percentage(100 - app.nav_percent()),
            ])
            .split(chunks[1]);
        let preview = panes[1];

        (preview.y..preview.y + preview.height)
            .map(|y| {
                (preview.x..preview.x + preview.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// The store's own words for a write it refuses by rule, taken from the seam
    /// that produces them so a test about a verbatim refusal never spells the
    /// message out.
    ///
    /// A conflict is not one of these: the reader is asked about that one in the
    /// browser's own words, so it must not stand in for a refusal shown verbatim.
    fn store_refusal(store: &Store, write: &data::Write, why: &str) -> String {
        match data::perform(store, write) {
            Err(data::Refusal::Rule(words) | data::Refusal::Partial(words)) => words,
            other => panic!("{why}: {other:?}"),
        }
    }

    /// Type into the open field, one keystroke per character, as a reader does.
    fn type_into(app: &mut App, text: &str) {
        for c in text.chars() {
            app.apply(Action::Insert(c)).unwrap();
        }
    }

    /// Where the focused field's cursor sits, as a line and a column read off what
    /// the field holds — so a test says where a motion landed in the terms a reader
    /// sees rather than in a character offset nobody can check by eye.
    fn cursor_at(app: &App) -> (usize, usize) {
        let surface = app.surface().expect("a surface is open");
        let field = &surface.fields()[surface.focus()];
        let (value, cursor) = text_and_cursor(field);
        let before: String = value.chars().take(cursor).collect();
        (
            before.matches('\n').count(),
            before.chars().rev().take_while(|c| *c != '\n').count(),
        )
    }

    /// What the open surface's focused field holds.
    fn field_value(app: &App) -> String {
        let surface = app.surface().expect("a surface is open");
        text_of(&surface.fields()[surface.focus()]).to_string()
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
                true if app.modal().is_some() || app.surface().is_some() => {
                    // Back out of what it opened, leaving the mode standing. An
                    // untouched surface has nothing to lose, so it goes without a
                    // question.
                    app.apply(Action::Unwind).unwrap();
                    assert!(
                        app.modal().is_none() && app.surface().is_none(),
                        "{action:?} could not be backed out of"
                    );
                }
                // A letter whose write needs nothing typed and nothing answered
                // performs it where it stands, which ends the session and says what
                // it did — so the row is frozen again for the rest of the walk.
                true if app.editing_target().is_none() => {
                    assert!(
                        app.flash_message().is_some(),
                        "{action:?} wrote and said nothing"
                    );
                    app.apply(Action::EnterEditing).unwrap();
                }
                true => panic!("{action:?} is hinted but the key did nothing"),
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
                    // An unhinted letter performs nothing and says why: either the
                    // mode's own wording, or the row's where it has somewhere
                    // better to send the reader. Silence would read as a broken key.
                    assert!(
                        app.flash_message().is_some(),
                        "{action:?} is not hinted and said nothing"
                    );
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
            // The rule is that a reader does not expect these to move anything
            // while the navigation pane is hidden, so the
            // silence is deliberate rather than a stray key going unanswered —
            // and deliberate silence still says nothing.
            assert_eq!(app.flash_message(), None, "{action:?} said something");
        }
    }

    #[test]
    fn an_unbound_key_in_browse_mode_changes_nothing_and_says_nothing() {
        let (_fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // somewhere other than the roster
                                             // A stale notice from an earlier action, so silence is proven by the
                                             // message surviving unchanged rather than by there never having been one
                                             // — `is_some()` would pass on a message this action itself raised.
        app.flash("an earlier notice");
        let cursor = app.nav().cursor();
        let crumbs: Vec<String> = app.nav().crumbs().iter().map(ToString::to_string).collect();
        let zoomed = app.zoomed();
        let percent = app.nav_percent();

        assert!(!app.apply(Action::Unbound).unwrap(), "quit the browser");

        assert_eq!(app.nav().cursor(), cursor);
        assert_eq!(app.nav().crumbs(), crumbs);
        assert_eq!(app.zoomed(), zoomed);
        assert_eq!(app.nav_percent(), percent);
        assert!(app.surface().is_none());
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.flash_message(), Some("an earlier notice"));
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
    fn pressing_the_divider_is_refused_while_a_dialog_is_open() {
        let (_fx, mut app) = app();
        app.set_divider_column(Some(30));
        to_a_label_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Delete).unwrap();
        assert!(app.modal().is_some(), "a dialog is open");

        // The one case where "nothing beneath it moves" is a real guarantee: the
        // press itself is refused, not merely left without effect once dragged,
        // so a reader mid-question cannot resize what the question sits over.
        assert!(
            !app.press(31),
            "the divider was grabbed while a dialog was open"
        );
        app.drag(60, 100);
        assert_eq!(
            app.nav_percent(),
            DEFAULT_NAV_PERCENT,
            "the drag moved the divider under an open dialog"
        );
    }

    #[test]
    fn the_divider_stays_live_through_editing_mode_and_an_open_surface() {
        let (_fx, mut app) = app();
        app.set_divider_column(Some(30));
        freeze_the_epics_row(&mut app);
        assert!(app.press(31), "editing mode blocked the divider");
        app.drag(60, 100);
        assert_eq!(
            app.nav_percent(),
            60,
            "the split did not move while editing mode was on"
        );
        app.release();
        app.apply(Action::Unwind).unwrap(); // leave editing mode before opening a surface

        // The split is the reader's own furniture, not what the mode freezes or
        // what a question is waiting on, so a surface drawn over the very pane
        // the drag resizes must not gate it either.
        open_the_label_surface(&mut app);
        assert!(app.press(31), "the open surface blocked the divider");
        app.drag(20, 100);
        assert_eq!(
            app.nav_percent(),
            20,
            "the split did not move while a surface was open"
        );
    }

    #[test]
    fn a_wheel_is_silent_in_editing_mode_where_a_key_would_notice() {
        let (_fx, mut app) = app();
        freeze_the_epics_row(&mut app);
        let cursor = app.nav().cursor();

        // Not a key: the notice below is worded for one and would be wrong for a
        // scroll, so a wheel event is answered with the same silence as being
        // zoomed with no cursor to move.
        for action in [Action::WheelDown, Action::WheelUp] {
            app.clear_flash();
            assert!(!app.apply(action).unwrap(), "{action:?} quit");
            assert_eq!(app.nav().cursor(), cursor, "{action:?} moved the cursor");
            assert_eq!(app.flash_message(), None, "{action:?} said something");
        }

        // The key it stands in for is a different story: bound to nothing this
        // row offers, it earns the notice that names the way out.
        app.apply(Action::CursorDown).unwrap();
        assert_eq!(
            app.nav().cursor(),
            cursor,
            "CursorDown moved the frozen cursor"
        );
        assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION));
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
    fn an_unbound_key_in_editing_mode_changes_nothing_and_says_nothing() {
        let (_fx, mut app) = app();
        app.apply(Action::EnterEditing).unwrap();
        app.flash("an earlier notice");
        let target = app.editing_target().cloned();

        assert!(!app.apply(Action::Unbound).unwrap(), "quit the browser");

        assert_eq!(app.editing_target(), target.as_ref());
        assert!(app.surface().is_none());
        assert_eq!(app.modal(), None);
        assert_eq!(app.flash_message(), Some("an earlier notice"));
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
    fn no_layer_over_the_mode_ends_a_session_the_mode_itself_refuses_to_end() {
        let (_fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        let target = app.editing_target().cloned();

        // The key list is drawn over the mode rather than being a hole in it, so
        // the key the mode refuses is refused while the list is open too: a reader
        // who opens the key list mid-edit and presses the key that ends a browsing
        // session must not lose what the mode holds.
        app.apply(Action::ToggleHelp).unwrap();
        assert!(
            !app.apply(Action::Quit).unwrap(),
            "the overlay quit the mode"
        );
        assert_eq!(
            app.modal(),
            Some(&Modal::Help),
            "the overlay answered a key it does not own"
        );
        assert_eq!(app.editing_target(), target.as_ref());
        let notice = app.flash_message().expect("a refused key says why");
        assert!(
            notice.contains("Esc"),
            "{notice:?} does not name the way out"
        );

        // And the refusal lasts exactly as long as the mode does: once it is off
        // the same key ends the session, under the overlay or without it.
        app.apply(Action::Unwind).unwrap(); // the overlay
        app.apply(Action::Unwind).unwrap(); // the mode
        assert_eq!(app.editing_target(), None);
        assert!(app.apply(Action::Quit).unwrap());
        app.apply(Action::ToggleHelp).unwrap();
        assert!(app.apply(Action::Quit).unwrap());
    }

    #[test]
    fn quitting_from_the_layer_above_a_buffer_cannot_discard_it() {
        let (_fx, mut app) = app();
        open_the_label_surface(&mut app);
        type_into(&mut app, "half a thought");

        // The key list is the one layer reachable from inside a field, so it is
        // the one place the quit key can be answered while a buffer holds text the
        // store has never been given. Nothing about it is destructive.
        app.apply(Action::ToggleHelp).unwrap();
        assert!(
            !app.apply(Action::Quit).unwrap(),
            "an unsaved buffer was quit out of"
        );
        assert_eq!(app.modal(), Some(&Modal::Help));
        assert_eq!(field_value(&app), "half a thought");

        // So closing the overlay lands back in the buffer with the text still
        // there to save or to discard through the warning that asks.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert_eq!(field_value(&app), "half a thought");
        assert!(app.surface().unwrap().fields()[0].is_dirty());
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

    /// Put the fixture's store into a read-only state and tell the browser what a
    /// reload would tell it, without reloading.
    ///
    /// The state is normally reached by a reload, and that path has tests of its
    /// own; this is for the frozen row a reload would have taken away with the
    /// mode, which the offer table has to decide on anyway.
    fn turn_read_only_behind_the_browser(fx: &Fixture, app: &mut App) {
        let state = ReadOnly::MigrationInProgress;
        assert!(crate::data::fixture::turn_read_only(&fx.store, state));
        app.read_only = data::read_only(&fx.store);
        assert_eq!(app.read_only(), Some(state));
    }

    #[test]
    fn a_store_that_may_not_be_written_is_not_a_store_the_mode_is_entered_on() {
        let fx = Fixture::build();
        let state = ReadOnly::MigrationInProgress;
        assert!(crate::data::fixture::turn_read_only(&fx.store, state));
        // Asked at startup, alongside the readability check the store was opened
        // with: a session that cannot write must not offer to.
        let mut app = App::new(fx.store.clone(), Theme::with_color(false)).unwrap();
        assert_eq!(app.read_only(), Some(state));

        app.apply(Action::EnterEditing).unwrap();
        // Every action the mode could offer is unavailable, so the mode itself is
        // not entered: the key is as unknown as any other action the browser
        // cannot perform, and saying nothing would read as a broken key.
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.mode(), Mode::Browse);
        // In the store's own words, remedy included: the browser must not
        // paraphrase a store rule, and only the store knows what fixes this.
        assert_eq!(app.flash_message(), Some(state.refusal().as_str()));
        assert!(app.editing_hints().is_empty());
    }

    #[test]
    fn the_store_is_asked_again_on_every_reload_and_the_mode_ends_where_it_says_no() {
        let (fx, mut app) = app();
        assert_eq!(app.read_only(), None, "the fixture store is writable");
        to_the_labels_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        assert!(app.editing_target().is_some());

        // An agent can migrate a store, or begin migrating one, while the browser
        // is open, so read-only is a state a session enters mid-flight rather than
        // a verdict reached at startup.
        let state = ReadOnly::MigrationInProgress;
        assert!(crate::data::fixture::turn_read_only(&fx.store, state));
        app.apply(Action::Reload).unwrap();
        assert_eq!(app.read_only(), Some(state));
        // Nothing the mode could do is offered any more, so the mode goes rather
        // than standing over a row with no action on it — and it says so.
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.flash_message(), Some(EDITING_STOPPED_READ_ONLY));

        // Said once: the notice reports the transition, and the condition outlives
        // it — the state slot is what carries a durable fact.
        app.clear_flash();
        app.apply(Action::Reload).unwrap();
        assert_eq!(app.read_only(), Some(state));
        assert_eq!(app.flash_message(), None);

        // And the mode stays unavailable for as long as the state holds.
        app.apply(Action::EnterEditing).unwrap();
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.flash_message(), Some(state.refusal().as_str()));

        // A migration that commits takes the state away again: it is a state a
        // session leaves as well as enters, so the browser cannot settle it once.
        crate::data::fixture::turn_writable(&fx.store);
        app.clear_flash();
        app.apply(Action::Reload).unwrap();
        assert_eq!(app.read_only(), None);
        app.apply(Action::EnterEditing).unwrap();
        assert!(
            app.editing_target().is_some(),
            "the mode is available again"
        );
        assert!(!app.editing_hints().is_empty(), "and so are its actions");
    }

    #[test]
    fn a_reload_that_finds_the_state_while_browsing_marks_it_and_says_nothing() {
        let (fx, mut app) = app();
        let state = ReadOnly::MigrationInProgress;
        assert!(crate::data::fixture::turn_read_only(&fx.store, state));
        app.apply(Action::Reload).unwrap();

        // Nothing was interrupted and nothing was refused, so there is nothing for
        // the transient channel to report: the condition belongs in the state slot,
        // which carries it for as long as it holds.
        assert_eq!(app.read_only(), Some(state));
        assert_eq!(app.flash_message(), None);
    }

    #[test]
    fn the_store_refusing_to_be_written_is_what_a_reload_reports_and_not_the_row() {
        let (fx, mut app) = app();
        to_a_label_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();

        // Both at once: the frozen row goes and the store stops being writable.
        // The reader's next question is about the store, not about where the
        // cursor went, and the strip holds one line.
        fx.strip_the_epics_labels();
        assert!(crate::data::fixture::turn_read_only(
            &fx.store,
            ReadOnly::MigrationInProgress
        ));
        app.apply(Action::Reload).unwrap();
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.flash_message(), Some(EDITING_STOPPED_READ_ONLY));
    }

    #[test]
    fn a_frozen_row_offers_nothing_once_the_store_may_not_be_written() {
        let (fx, mut app) = app();
        let before = fx.epic_labels();
        to_a_label_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        assert_eq!(app.editing_hints(), vec![hint_for(EditingAction::Delete)]);

        // The offer table is the whole of what a letter does on a row, so the
        // state that offers nothing is decided there and not beside each key: a
        // mode that outlived the reload which turned the store read-only offers
        // nothing rather than half of what it did.
        turn_read_only_behind_the_browser(&fx, &mut app);
        assert!(app.editing_hints().is_empty());
        hints_and_keys_agree(&mut app);
        assert_eq!(fx.epic_labels(), before, "a letter wrote something");
    }

    #[test]
    fn a_row_that_only_signposts_the_command_line_is_silent_on_a_read_only_store() {
        let (fx, mut app) = app();
        to_the_assets_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Add).unwrap();
        let signpost = app
            .flash_message()
            .expect("the row names the command that does the job")
            .to_string();
        assert!(signpost.starts_with("loti "), "{signpost:?}");

        // Not even the signpost survives: a store that may not be written offers
        // no action at all, and a message about how to add one to it would send the
        // reader at a command the store would refuse too.
        turn_read_only_behind_the_browser(&fx, &mut app);
        app.clear_flash();
        app.apply(Action::Add).unwrap();
        assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION));
        assert!(app.editing_hints().is_empty());
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
        // as any key the mode does not admit, and the strip does not list it
        // among the actions the row does offer.
        assert!(!app
            .editing_hints()
            .contains(&hint_for(EditingAction::Delete)));
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
        let (fx, mut app) = app();
        // With a claim held somewhere on the tree, because one of the actions is
        // offered only on a claimed row: on a store where nothing is held, its hint
        // is unreachable for a reason that is not the one this test is about.
        fx.claim(&fx.node);
        app.apply(Action::Reload).unwrap();
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
        let refusal = store_refusal(
            &fx.store,
            &data::Write::RemoveLabel(target),
            "the store refuses a label removal on a missing entity",
        );
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
        assert_eq!(text_of(field), "");
        assert!(!field.is_dirty());
        // The float says what is being added and to what, since it covers the row
        // it was opened from.
        assert!(surface.title().contains(&fx.epic), "{:?}", surface.title());
        // Every key now belongs to the field: the mode the keyboard is under is the
        // only bridge from this state to the key table.
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::OneLine));
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

        // The one collection of the epic's level that opens nothing at all is the
        // assets row, and it does not list the letter either. It has somewhere to
        // send the reader instead and says so, which its own test pins.
        to_the_roster(&mut app);
        app.apply(Action::Descend).unwrap(); // into the epic
        to_row(
            &mut app,
            |kind| matches!(kind, RowKind::Collection(c) if c.name() == "assets"),
        );
        app.apply(Action::EnterEditing).unwrap();
        // Cleared first, because a notice lives five seconds: one left over from an
        // earlier row would answer for this one.
        app.clear_flash();
        app.apply(Action::Add).unwrap();
        assert!(
            app.surface().is_none(),
            "the assets row opened the label surface"
        );
        // The exact wording, not merely that something was said: a row that raised
        // another row's notice would send the reader to the wrong command, which is
        // worse than saying nothing.
        assert!(
            app.flash_message()
                .is_some_and(|notice| notice.contains("asset add")),
            "the assets row did not name the command that attaches one"
        );
        assert!(app.editing_hints().is_empty(), "the assets row");
        app.apply(Action::Unwind).unwrap();

        // A dependency list and a comment list do offer an addition, and neither is
        // this one: each collection's member is its own shape of input, so the
        // surface a row opens is the surface that writes what that row holds.
        open_the_blocker_surface(&mut app);
        let surface = app.surface().expect("the surface is open");
        assert_eq!(surface.fields()[0].label(), BLOCKER_FIELD);
        assert!(!surface.title().contains("label"), "{:?}", surface.title());
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();

        open_the_comment_buffer(&mut app);
        let surface = app.surface().expect("the surface is open");
        assert_eq!(surface.fields()[0].label(), COMMENT_FIELD);
        assert!(!surface.title().contains("label"), "{:?}", surface.title());
    }

    #[test]
    fn an_unbound_key_inside_a_one_line_field_touches_nothing_and_leaves_the_surface_open() {
        let (fx, mut app) = app();
        let before = fx.epic_labels();
        open_the_label_surface(&mut app);
        type_into(&mut app, "partial");
        let cursor = cursor_at(&app);

        app.apply(Action::Unbound).unwrap();

        // Nothing about the field the reader was mid-way through typing moved: an
        // unbound key is not a character, so it is not the reflex key's silent
        // catch-all either — it never reaches `apply_to_surface`'s match at all
        // without first being an action the field recognises as its own.
        let surface = app.surface().expect("the surface closed");
        let field = &surface.fields()[0];
        assert_eq!(text_of(field), "partial");
        assert!(field.is_dirty(), "typing before the key pressed was undone");
        assert_eq!(cursor_at(&app), cursor, "the cursor moved");
        assert_eq!(app.flash_message(), None);
        assert_eq!(fx.epic_labels(), before, "the store was written");
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
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::OneLine));
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
    fn a_field_being_required_is_what_stops_the_write_and_the_warning_lands_in_that_field() {
        let (fx, mut app) = app();
        let before = fx.epic_labels();
        // An optional field, then a required one, both empty. What stops the write
        // is the required one: a field nobody has to fill in is not something to
        // warn about, and the warning names the field the reader must go to.
        // A third field, required and also empty, is what makes "the first empty
        // required one" a claim rather than a coincidence: with one such field, a
        // warning that named the last would pass just as happily.
        open_a_surface_with_fields(
            &mut app,
            vec![
                Field::new(A_SECOND_FIELD, false, Lines::One),
                Field::new(LABEL_FIELD, true, Lines::One),
                Field::new(A_THIRD_FIELD, true, Lines::One),
            ],
        );
        // Several fields, and the key map is told so: which keys apply is decided
        // from the shape rather than guessed at.
        assert_eq!(
            app.mode(),
            surface_mode(Fields::Several, FieldKind::OneLine)
        );

        app.apply(Action::Accept).unwrap();
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("an empty required field wrote or said nothing")
        };
        assert!(dialog.message().contains(LABEL_FIELD), "{dialog:?}");
        assert!(
            !dialog.message().contains(A_SECOND_FIELD),
            "an optional field was warned about: {dialog:?}"
        );
        assert!(
            !dialog.message().contains(A_THIRD_FIELD),
            "a later empty field was warned about before the first: {dialog:?}"
        );
        // The field it names is the field dismissal puts the reader in, which here
        // is not the field they started in.
        assert_eq!(dialog.dismissal.performs, Some(OnDismissal::Focus(1)));
        assert_eq!(fx.epic_labels(), before, "the warning wrote something");

        // And that focus is applied rather than merely carried: after dismissing,
        // what is typed lands in the field the warning named.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.surface().map(Surface::focus), Some(1));
        type_into(&mut app, "ui-2");
        let fields = app.surface().expect("the buffer is still open").fields();
        assert_eq!(text_of(&fields[1]), "ui-2");
        assert_eq!(
            text_of(&fields[0]),
            "",
            "the answer was typed into the field the warning was not about"
        );
    }

    #[test]
    fn an_empty_optional_field_is_no_reason_to_refuse_the_write() {
        // The other direction of the same rule: with every required field filled,
        // an empty optional one does not stop an accept.
        let (fx, mut app) = app();
        let before = fx.epic_labels();
        open_a_surface_with_fields(
            &mut app,
            vec![
                Field::new(LABEL_FIELD, true, Lines::One),
                Field::new(A_SECOND_FIELD, false, Lines::One),
            ],
        );
        type_into(&mut app, "a new label");
        app.apply(Action::Accept).unwrap();
        assert_eq!(
            app.modal(),
            None,
            "an empty optional field was warned about"
        );
        let mut expected = before;
        expected.push("a new label".to_string());
        assert_eq!(fx.epic_labels(), expected);
    }

    #[test]
    fn the_field_keys_move_the_keyboard_round_the_fields_without_writing_to_any() {
        let (_fx, mut app) = app();
        open_a_surface_with_fields(
            &mut app,
            vec![
                Field::new(LABEL_FIELD, true, Lines::One),
                Field::new(A_SECOND_FIELD, false, Lines::One),
                Field::new("third", false, Lines::One),
            ],
        );

        // Forwards, and round from the last: the forward key has to reach every
        // field on its own, so a walk that stopped at the end would leave it dead
        // there.
        for expected in [1, 2, 0, 1] {
            app.apply(Action::NextField).unwrap();
            assert_eq!(app.surface().map(Surface::focus), Some(expected));
        }
        // Backwards, and round from the first.
        for expected in [0, 2, 1] {
            app.apply(Action::PreviousField).unwrap();
            assert_eq!(app.surface().map(Surface::focus), Some(expected));
        }

        // The keyboard is where the focus says: what is typed lands in that field
        // and in no other.
        type_into(&mut app, "middle");
        let fields = app.surface().expect("a surface is open").fields();
        assert_eq!(
            fields.iter().map(text_of).collect::<Vec<_>>(),
            vec!["", "middle", ""]
        );
        // And moving is not writing: a field the keyboard only passed through is
        // untouched, so the way out stays silent about it.
        assert!(!fields[0].is_dirty());
        assert!(!fields[2].is_dirty());
    }

    #[test]
    fn the_strip_offers_the_field_keys_where_there_are_fields_to_move_between() {
        let (_fx, mut app) = app();
        open_a_surface_with_fields(
            &mut app,
            vec![
                Field::new(LABEL_FIELD, true, Lines::One),
                Field::new(A_SECOND_FIELD, false, Lines::One),
            ],
        );
        let lines = frame_lines(&mut app, 100, 24);
        let strip = lines.last().expect("the strip is the bottom line").clone();
        // Ranked rather than in key order: saving is what the reader came to do,
        // then moving between the fields, then the power-user escape.
        let at = |hint: &str| {
            strip
                .find(hint)
                .unwrap_or_else(|| panic!("{hint:?}: {strip:?}"))
        };
        assert!(at("Ctrl-S save") < at("Tab fields"), "{strip:?}");
        assert!(at("Tab fields") < at("Ctrl-G editor"), "{strip:?}");
    }

    #[test]
    fn the_strip_of_a_one_field_surface_teaches_no_key_that_moves_between_fields() {
        // A surface with one field answers no key that moves between fields, and
        // the strip never names a key the surface ignores.
        let (_fx, mut app) = app();
        open_the_label_surface(&mut app);
        let lines = frame_lines(&mut app, 100, 24);
        let strip = lines.last().expect("the strip is the bottom line");
        assert!(strip.contains("Ctrl-S save"), "{strip:?}");
        assert!(!strip.contains("Tab"), "{strip:?}");
    }

    #[test]
    fn a_field_that_holds_many_lines_keeps_the_breaks_and_is_moved_through_by_line() {
        let (_fx, mut app) = app();
        open_a_surface_with_fields(
            &mut app,
            vec![Field::new(A_SECOND_FIELD, false, Lines::Many)],
        );

        // A line break is content in a field that holds many lines, so it lands in
        // the value like any other character.
        type_into(&mut app, "one");
        app.apply(Action::Insert('\n')).unwrap();
        type_into(&mut app, "twelve");
        app.apply(Action::Insert('\n')).unwrap();
        type_into(&mut app, "x");
        assert_eq!(field_value(&app), "one\ntwelve\nx");
        assert_eq!(cursor_at(&app), (2, 1));

        // The line keys are the line's, not the whole value's: a field holding one
        // line cannot tell the two apart, and this one can.
        app.apply(Action::MoveToStart).unwrap();
        assert_eq!(cursor_at(&app), (2, 0));
        app.apply(Action::MoveUp).unwrap();
        assert_eq!(cursor_at(&app), (1, 0));
        app.apply(Action::MoveToEnd).unwrap();
        assert_eq!(cursor_at(&app), (1, "twelve".len()));

        // Vertical motion keeps the column, clamped to what the line it lands on
        // has: a short line takes the cursor to its end rather than past it. The
        // clamp is asserted in both directions, because an unclamped step lands
        // inside the line the cursor came from rather than past the value's end,
        // which is a silent wrong place rather than a panic.
        app.apply(Action::MoveUp).unwrap();
        assert_eq!(cursor_at(&app), (0, "one".len()));
        app.apply(Action::MoveDown).unwrap();
        assert_eq!(cursor_at(&app), (1, "one".len()));
        app.apply(Action::MoveToEnd).unwrap();
        assert_eq!(cursor_at(&app), (1, "twelve".len()));
        app.apply(Action::MoveDown).unwrap();
        assert_eq!(cursor_at(&app), (2, 1));
        app.apply(Action::MoveUp).unwrap();
        assert_eq!(cursor_at(&app), (1, 1));
        app.apply(Action::MoveUp).unwrap();
        assert_eq!(cursor_at(&app), (0, 1));
        // And there is no line past either end to land on.
        app.apply(Action::MoveUp).unwrap();
        assert_eq!(cursor_at(&app), (0, 1));
        app.apply(Action::MoveDown).unwrap();
        app.apply(Action::MoveDown).unwrap();
        app.apply(Action::MoveDown).unwrap();
        assert_eq!(cursor_at(&app), (2, 1));

        // What the external editor hands back keeps its breaks too — the one door
        // that carries whole paragraphs — while a carriage return is dropped: an
        // editor leaving CRLF behind means one break, not a break and a character no
        // terminal shows.
        app.editor_returned("alpha\r\nbeta\n");
        assert_eq!(field_value(&app), "alpha\nbeta\n");
    }

    #[test]
    fn a_line_break_never_reaches_a_field_that_holds_one_line() {
        let (_fx, mut app) = app();
        open_the_label_surface(&mut app);

        // The key map sends no break to a one-line field — the reflex key is bound
        // to nothing there — but the field is what guarantees the value never holds
        // one, so a break arriving by any route is dropped rather than stored. (The
        // editor's door is pinned where the editor round-trip is.)
        type_into(&mut app, "one");
        app.apply(Action::Insert('\n')).unwrap();
        type_into(&mut app, "two");
        assert_eq!(field_value(&app), "onetwo");

        // A dropped character was still a keystroke that writes, so the field is
        // dirty by it: dirty is what was pressed and not what came out different.
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Delete).unwrap();
        app.apply(Action::Add).unwrap();
        app.apply(Action::Insert('\n')).unwrap();
        assert_eq!(field_value(&app), "");
        assert!(app.surface().unwrap().fields()[0].is_dirty());
    }

    #[test]
    fn the_shape_the_key_map_is_told_carries_the_focused_fields_kind() {
        let (_fx, mut app) = app();
        // One surface, two kinds of field: which one the keyboard is in is what
        // decides whether a line break is content, so the kind has to follow the
        // focus rather than describe the surface as a whole.
        open_a_surface_with_fields(
            &mut app,
            vec![
                Field::new(LABEL_FIELD, true, Lines::One),
                Field::new(A_SECOND_FIELD, false, Lines::Many),
            ],
        );
        assert_eq!(
            app.mode(),
            surface_mode(Fields::Several, FieldKind::OneLine)
        );
        app.apply(Action::NextField).unwrap();
        assert_eq!(
            app.mode(),
            surface_mode(Fields::Several, FieldKind::ManyLines)
        );
        // Recomputed on every ask rather than captured when the surface opened, so
        // the key map and the surface cannot come to disagree about where the
        // keyboard is.
        app.apply(Action::PreviousField).unwrap();
        assert_eq!(
            app.mode(),
            surface_mode(Fields::Several, FieldKind::OneLine)
        );
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
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::OneLine));

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
        assert_eq!(cursor_of(&app.surface().unwrap().fields()[0]), 0);
        type_into(&mut app, "ab");
        for _ in 0..3 {
            app.apply(Action::MoveRight).unwrap();
        }
        assert_eq!(cursor_of(&app.surface().unwrap().fields()[0]), 2);
        app.apply(Action::MoveToStart).unwrap();
        for _ in 0..3 {
            app.apply(Action::MoveLeft).unwrap();
        }
        assert_eq!(cursor_of(&app.surface().unwrap().fields()[0]), 0);

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
            cursor_of(&app.surface().unwrap().fields()[0]),
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
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::OneLine));
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
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::OneLine));
        assert!(app.editing_target().is_some());
    }

    #[test]
    fn the_body_buffer_opens_on_the_text_the_store_holds_at_that_moment() {
        let (fx, mut app) = app();
        freeze_the_epics_row(&mut app);

        // A write lands after the mode was entered and before the letter was
        // pressed. The buffer has to start from this, not from the text the preview
        // beside it was rendered from: without a read here the conflict window would
        // be "since the cursor last moved" — minutes of browsing before any typing.
        fx.rewrite_the_epics_body("theirs, after the mode was entered\n");
        app.apply(Action::Edit(FreeForm::Body)).unwrap();
        assert_eq!(field_value(&app), fx.epic_body());
        // One field holding many lines, which is the whole of what the key map is
        // told: it is what makes the reflex key a line break here, while the save
        // key accepts here exactly as it does everywhere else.
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::ManyLines));
        // Nothing has been typed, so the way out has nothing to warn about: text
        // the store already held is not text the reader wrote.
        assert!(!app.surface().unwrap().fields()[0].is_dirty());

        // And the stamp the buffer carries is that read's, not one from an earlier
        // listing: accepting at once is not a conflict, though the entity moved on
        // between the mode being entered and the buffer being opened.
        app.apply(Action::Accept).unwrap();
        assert_eq!(
            app.modal(),
            None,
            "the buffer's own read was refused as stale"
        );
    }

    #[test]
    fn a_ticket_offers_its_own_body_and_the_buffer_says_which_row_it_is_editing() {
        let (fx, mut app) = app();
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        // A node has a body of its own, so its row offers the letter and the strip
        // lists it.
        assert!(app
            .editing_hints()
            .contains(&hint_for(EditingAction::Edit(FreeForm::Body))));
        hints_and_keys_agree(&mut app);

        app.apply(Action::Edit(FreeForm::Body)).unwrap();
        let surface = app.surface().expect("the letter opened a buffer");
        // The buffer names the row it is editing, because the pane is not the row
        // and the frozen row is dim beside it — and it holds the node's own text,
        // not the epic's, which is the difference a wrongly addressed read would
        // hide.
        let (_, reference) = fx.node_reference_forms();
        assert!(
            surface.title().contains(&reference),
            "{:?}",
            surface.title()
        );
        assert_eq!(field_value(&app), "node body\n");
        assert_ne!(field_value(&app), fx.epic_body());

        // A collection and its members are edited by their own operations, so no row
        // of one offers the letter: it is as unknown there as any key the mode never
        // binds.
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();
        to_a_label_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        app.clear_flash();
        app.apply(Action::Edit(FreeForm::Body)).unwrap();
        assert!(app.surface().is_none(), "a label row opened a body buffer");
        assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION));
        assert!(!app
            .editing_hints()
            .contains(&hint_for(EditingAction::Edit(FreeForm::Body))));
    }

    #[test]
    fn a_body_that_cannot_be_read_is_reported_and_the_mode_stays_on() {
        let (fx, mut app) = app();
        freeze_the_epics_row(&mut app);

        // The entity can go between the letter being offered and being pressed, and
        // a buffer cannot open on text nothing could read — so the read's failure is
        // reported as any other unreadable part of a store is, and the mode stands.
        fx.remove_the_epics_file();
        app.apply(Action::Edit(FreeForm::Body)).unwrap();
        assert!(
            app.surface().is_none(),
            "a buffer opened on text that could not be read"
        );
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("a failed read said nothing: {:?}", app.modal())
        };
        assert!(dialog.message().contains(&fx.epic), "{dialog:?}");
        // Nothing is at stake in reading it, so it is dismissed rather than
        // answered, and a failure is never a transient notice.
        assert_eq!(dialog.answers(), Answers::Acknowledge);
        assert_eq!(app.flash_message(), None);

        app.apply(Action::Unwind).unwrap();
        assert!(app.editing_target().is_some(), "the mode ended");
        assert_eq!(app.mode(), Mode::Editing);
    }

    #[test]
    fn the_way_out_of_a_body_with_typing_in_it_names_the_field_and_keeps_the_text() {
        let (fx, mut app) = app();
        let before = fx.epic_body();
        open_the_body_buffer(&mut app);
        type_into(&mut app, "a first line");

        // The text is the only copy of what the reader wrote, so the way out asks —
        // naming the field, because the buffer covers the pane and the frozen row is
        // dim beside it.
        app.apply(Action::Unwind).unwrap();
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("a dirty buffer was thrown away unasked: {:?}", app.modal())
        };
        assert!(
            dialog.message().contains(FreeForm::Body.noun()),
            "{dialog:?}"
        );
        assert_eq!(dialog.answers(), Answers::Destructive);

        // The answer that is never destructive lands back in the buffer with the
        // text exactly as it was.
        app.apply(Action::Unwind).unwrap();
        assert!(field_value(&app).starts_with("a first line"));
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::ManyLines));

        // And the destructive letter throws away the buffer and only the buffer:
        // nothing reached the store, and the mode stays on its frozen row.
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Delete).unwrap();
        assert!(app.surface().is_none());
        assert_eq!(app.mode(), Mode::Editing);
        assert_eq!(fx.epic_body(), before, "discarding wrote something");
    }

    #[test]
    fn a_body_emptied_in_the_buffer_is_written_as_empty_and_never_called_required() {
        let (fx, mut app) = app();
        open_the_body_buffer(&mut app);
        let held = field_value(&app);
        assert!(!held.is_empty(), "the fixture's body is already empty");

        // Deleting forwards from where the buffer opens takes the breaks with the
        // characters, so the whole document goes. Bounded by what was there, so a
        // delete that stopped removing anything fails here instead of spinning.
        for _ in 0..held.chars().count() {
            app.apply(Action::DeleteAfter).unwrap();
        }
        assert_eq!(field_value(&app), "", "the buffer would not empty");

        // Emptying a body is a thing a reader may mean, so the save goes through:
        // no field is missing, and what makes a body acceptable is the store's
        // rule and not this surface's.
        app.apply(Action::Accept).unwrap();
        assert_eq!(app.modal(), None, "an emptied body was called required");
        assert!(app.surface().is_none(), "the buffer outlived its own save");
        assert_eq!(fx.epic_body(), "", "emptying the buffer wrote nothing");
    }

    #[test]
    fn a_body_refused_for_a_conflict_asks_and_keeps_the_buffer_whichever_way_it_ends() {
        let (fx, mut app) = app();
        open_the_body_buffer(&mut app);
        type_into(&mut app, "mine");

        // Somebody else writes while the reader is composing theirs, which is the
        // one window the store's own lock cannot cover.
        fx.rewrite_the_epics_body("theirs\n");
        app.apply(Action::Accept).unwrap();
        // Asked rather than told: this is the one refusal with something for the
        // reader to decide, so the keyboard is under the conflict's own answers and
        // not under the ones a report is dismissed by.
        assert_eq!(app.mode(), Mode::Dialog(Answers::Conflict));
        assert_eq!(app.flash_message(), None, "a failure is never a notice");

        // The way out keeps the buffer, the text and the session: only a successful
        // write ends one, so the reader can still carry their text out through the
        // external editor.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert!(field_value(&app).starts_with("mine"));
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::ManyLines));
        assert!(
            app.editing_target().is_some(),
            "a refusal ended the session"
        );

        // And it decided nothing: the same save asks the same question again rather
        // than going through on the strength of having been asked once.
        app.apply(Action::Accept).unwrap();
        assert_eq!(app.mode(), Mode::Dialog(Answers::Conflict));

        // Going ahead ends the session, surface and all, which is what only a
        // successful write does.
        app.apply(Action::Overwrite).unwrap();
        assert_eq!(app.modal(), None);
        assert!(app.surface().is_none());
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.mode(), Mode::Browse);
    }

    #[test]
    fn a_body_refused_by_the_read_only_gate_asks_and_marks_the_store_at_once() {
        let (fx, mut app) = app();
        open_the_body_buffer(&mut app);
        type_into(&mut app, "mine");
        let state = ReadOnly::MigrationInProgress;
        crate::data::fixture::turn_read_only(&fx.store, state);

        app.apply(Action::Accept).unwrap();

        assert_eq!(app.mode(), Mode::Dialog(Answers::Acknowledge));
        let Modal::Dialog(dialog) = app.modal().expect("the refusal is reported") else {
            panic!("the refusal did not open a dialog");
        };
        assert_eq!(dialog.message(), state.refusal());
        assert_eq!(dialog.answers(), Answers::Acknowledge);
        assert_eq!(app.read_only(), Some(state));

        app.apply(Action::Unwind).unwrap();
        assert!(field_value(&app).starts_with("mine"));
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::ManyLines));
        assert!(
            app.editing_target().is_some(),
            "a refusal ended the session"
        );
    }

    /// Enter editing mode on the ticket nested inside the epic, which is the
    /// other kind of row that carries a name, a summary and a body of its own.
    fn freeze_a_ticket_row(app: &mut App) {
        to_the_roster(app);
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(app);
        app.apply(Action::EnterEditing).unwrap();
    }

    /// Empty the open field the way a reader does, one deletion per character.
    ///
    /// Bounded by what the field held, so a deletion that stopped removing anything
    /// fails here rather than spinning at full tilt with nothing to say.
    fn empty_the_field(app: &mut App) {
        for _ in 0..field_value(app).chars().count() {
            app.apply(Action::DeleteAfter).unwrap();
        }
        assert_eq!(field_value(app), "", "the field would not empty");
    }

    #[test]
    fn the_name_and_the_summary_are_one_short_field_opened_on_the_value_the_store_holds() {
        let (fx, mut app) = app();
        // On the epic's row and on a ticket's row, because the two are addressed
        // differently and carry the same fields: a read aimed at the wrong kind of
        // entity, or at the wrong field of the right one, looks correct from either
        // case on its own.
        for on_a_ticket in [false, true] {
            for field in [FreeForm::Name, FreeForm::Summary] {
                match on_a_ticket {
                    true => freeze_a_ticket_row(&mut app),
                    false => freeze_the_epics_row(&mut app),
                }
                let reference = app
                    .editing_target()
                    .expect("the mode froze a row")
                    .reference();
                // The row offers the letter, so the strip teaches it: a letter no
                // hint names is a letter nobody presses.
                assert!(
                    app.editing_hints()
                        .contains(&hint_for(EditingAction::Edit(field))),
                    "{field:?} on {reference}"
                );
                app.apply(Action::Edit(field)).unwrap();

                let surface = app.surface().expect("the letter opened a surface");
                // It says which row it is editing and which field of it, because the
                // float covers the frozen row and the row is dim beside it.
                assert!(
                    surface.title().contains(&reference) && surface.title().contains(field.noun()),
                    "{:?}",
                    surface.title()
                );
                assert_eq!(surface.fields()[0].label(), field.noun());
                // It opens on that field's own value as the store holds it, and not
                // on a neighbour's: this is the text the save writes back.
                let held = match on_a_ticket {
                    true => fx.node_field(field),
                    false => fx.epic_field(field),
                };
                assert_eq!(field_value(&app), held, "{field:?} on {reference}");
                // One field holding one line, which is the whole of what the key map
                // is told: a value that may hold no break is one the reflex key does
                // not reach at all, and the save key is what finishes it.
                assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::OneLine));
                // Nothing has been typed, so the way out asks nothing: text the
                // store already held is not text the reader wrote.
                assert!(!app.surface().unwrap().fields()[0].is_dirty());
                app.apply(Action::Unwind).unwrap();
                assert_eq!(app.modal(), None, "leaving a clean field asked");
                app.apply(Action::Unwind).unwrap();
            }
        }

        // And a collection or one of its members has none of these fields, so no row
        // of one offers either letter: it is as unknown there as any key the mode
        // never binds.
        to_a_label_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        for field in [FreeForm::Name, FreeForm::Summary] {
            app.clear_flash();
            app.apply(Action::Edit(field)).unwrap();
            assert!(app.surface().is_none(), "{field:?} opened on a label row");
            assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION));
            assert!(!app
                .editing_hints()
                .contains(&hint_for(EditingAction::Edit(field))));
        }
        hints_and_keys_agree(&mut app);
    }

    #[test]
    fn a_name_emptied_in_the_field_is_refused_as_required_and_a_summary_is_written() {
        let (fx, mut app) = app();
        let named = fx.epic_field(FreeForm::Name);

        // A name is how every row addresses what it names, so a row with none is a
        // row the reader cannot pick out: emptying one is refused, naming the field,
        // and nothing reaches the store.
        freeze_the_epics_row(&mut app);
        app.apply(Action::Edit(FreeForm::Name)).unwrap();
        empty_the_field(&mut app);
        app.apply(Action::Accept).unwrap();
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("an emptied name was written: {:?}", app.modal())
        };
        assert!(
            dialog.message().contains(FreeForm::Name.noun()),
            "{dialog:?}"
        );
        assert_eq!(fx.epic_field(FreeForm::Name), named, "the name was written");
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Delete).unwrap(); // the field is dirty, so the way out asks
        app.apply(Action::Unwind).unwrap();

        // A summary is not the same case: an entity with no summary is a legitimate
        // thing to mean, so emptying one is a write and not a missing field.
        freeze_the_epics_row(&mut app);
        app.apply(Action::Edit(FreeForm::Summary)).unwrap();
        empty_the_field(&mut app);
        app.apply(Action::Accept).unwrap();
        assert_eq!(app.modal(), None, "an emptied summary was called required");
        assert!(app.surface().is_none(), "the field outlived its own save");
        assert_eq!(
            fx.epic_field(FreeForm::Summary),
            "",
            "emptying the field wrote nothing"
        );
        assert_eq!(
            fx.epic_field(FreeForm::Name),
            named,
            "the summary's save took the name with it"
        );
    }

    #[test]
    fn a_comment_offers_its_author_two_letters_and_everyone_else_none() {
        let (fx, mut app) = app();
        let mine = fx.the_humans_comment();
        let agents = fx.an_agents_comment();
        let withdrawn = fx.a_withdrawn_comment();
        app.apply(Action::Reload).unwrap();
        let held = fx.epic_comments();

        // The human's own live comment offers exactly two letters: the long-form
        // text and the withdrawal. Not a name, a summary, a state or a claim — a
        // comment has none of those — and not an addition, which belongs to the list
        // its row sits on.
        to_the_comment_numbered(&mut app, mine);
        app.apply(Action::EnterEditing).unwrap();
        assert_eq!(
            app.editing_hints(),
            vec![
                hint_for(EditingAction::Delete),
                hint_for(EditingAction::Edit(FreeForm::Body)),
            ]
        );
        hints_and_keys_agree(&mut app);
        app.apply(Action::Unwind).unwrap();

        // A comment somebody else wrote, and one already withdrawn, offer nothing at
        // all: a comment is its author's alone to change, and a tombstone has no
        // text to rewrite and cannot be withdrawn twice. The keys are simply absent
        // — pressed anyway they are as unknown as any key the mode never binds, and
        // the reader is told in the mode's own words rather than by a refusal the
        // browser invents, so the rule itself is never learned from the screen.
        for (id, whose) in [(agents, "an agent's comment"), (withdrawn, "a tombstone")] {
            to_the_comment_numbered(&mut app, id);
            app.apply(Action::EnterEditing).unwrap();
            assert!(app.editing_hints().is_empty(), "{whose} teaches a letter");
            for intent in [Action::Edit(FreeForm::Body), Action::Delete] {
                app.clear_flash();
                app.apply(intent).unwrap();
                assert!(app.surface().is_none(), "{whose} opened a buffer");
                assert_eq!(app.modal(), None, "{whose} asked something");
                assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION), "{whose}");
            }
            hints_and_keys_agree(&mut app);
            app.apply(Action::Unwind).unwrap();
        }
        // And nothing was written on the way: an offer nobody made must not have
        // been half performed.
        assert_eq!(fx.epic_comments(), held);
    }

    #[test]
    fn a_comment_is_a_buffer_in_the_pane_and_is_never_written_empty() {
        let (fx, mut app) = app();
        let held = fx.epic_comments();

        // A comment is prose, so it is written where a body is: many lines in the
        // preview pane, with the list's row visible beside it. It starts empty —
        // nothing the browser puts there could be text the reader meant.
        open_the_comment_buffer(&mut app);
        let surface = app.surface().expect("the buffer is open");
        assert_eq!(surface.placement(), Placement::Pane);
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::ManyLines));
        assert_eq!(text_of(&surface.fields()[0]), "");
        assert!(!surface.fields()[0].is_dirty());
        assert!(surface.title().contains(&fx.epic), "{:?}", surface.title());

        // A comment with nothing in it says nothing, so saving an empty one warns,
        // naming the field, and the list is left as it was.
        app.apply(Action::Accept).unwrap();
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("an empty comment was written: {:?}", app.modal())
        };
        assert!(dialog.message().contains(COMMENT_FIELD), "{dialog:?}");
        assert_eq!(fx.epic_comments(), held, "an empty comment was written");

        // Typed into, the same save writes it: a reflex line break is content here,
        // as it is in any buffer that holds many lines.
        app.apply(Action::Unwind).unwrap();
        type_into(&mut app, "a remark");
        app.apply(Action::Insert('\n')).unwrap();
        app.apply(Action::Accept).unwrap();
        assert_eq!(app.modal(), None, "the store refused the comment");
        let after = fx.epic_comments();
        let added = after
            .iter()
            .find(|c| !held.iter().any(|had| had.id == c.id))
            .expect("the comment was added");
        assert_eq!(added.text, "a remark\n");

        // The buffer that rewrites one opens on the text the store holds at that
        // moment rather than on nothing, and is required for the same reason: a
        // comment is taken back by being withdrawn, not by being emptied.
        open_the_comment_edit(&mut app);
        let surface = app.surface().expect("the buffer is open");
        assert_eq!(surface.placement(), Placement::Pane);
        let mine = fx.the_humans_comment();
        assert_eq!(
            text_of(&surface.fields()[0]),
            after
                .iter()
                .find(|c| c.id == mine)
                .expect("the human's comment is held")
                .text,
            "the buffer opened on text the store was not holding"
        );
        empty_the_field(&mut app);
        app.apply(Action::Accept).unwrap();
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("an emptied comment was written: {:?}", app.modal())
        };
        assert!(
            dialog
                .message()
                .contains(data::Replaceable::CommentText.noun()),
            "{dialog:?}"
        );
        assert_eq!(fx.epic_comments(), after, "emptying it wrote something");
    }

    #[test]
    fn a_rewritten_comment_holds_what_was_typed_and_the_notice_names_it_by_number() {
        let (fx, mut app) = app();
        let mine = fx.the_humans_comment();

        // The words that reach the store are the reader's own: a rewrite that saved
        // anything else would report success over text nobody wrote, and the comment
        // it replaced is gone by then.
        open_the_comment_edit(&mut app);
        empty_the_field(&mut app);
        type_into(&mut app, "rewritten by hand");
        app.apply(Action::Accept).unwrap();
        assert_eq!(app.modal(), None, "the save asked something");
        assert!(app.surface().is_none(), "the buffer outlived its own save");
        assert!(
            app.editing_target().is_none(),
            "a successful save stayed in"
        );
        let held = fx
            .epic_comments()
            .into_iter()
            .find(|c| c.id == mine)
            .expect("the comment is still there");
        assert_eq!(held.text, "rewritten by hand");

        // And the notice says which of a list of comments moved, by the number the
        // list shows — a comment has no name to be recognised by, and the reader is
        // looking at a row that says nothing but a number and an author.
        let notice = app.flash_message().expect("a successful save said nothing");
        assert!(notice.contains("text"), "{notice:?}");
        assert!(notice.contains(&format!("comment {mine}")), "{notice:?}");
        assert!(notice.contains("saved"), "{notice:?}");
    }

    #[test]
    fn every_field_a_surface_replaces_is_written_under_the_stamp_it_was_read_at() {
        let (fx, mut app) = app();
        // Every replaceable field, because each opens on a read of its own and each
        // has to name that read's stamp: a field whose write named none would
        // silently overwrite whatever landed while the reader was typing. A
        // comment's text is one of them and is opened from its own row, by the
        // letter that opens a body on an epic — and the stamp that guards it is its
        // container's, so the same change underneath refuses it.
        for field in data::Replaceable::ALL.iter().copied() {
            match field {
                data::Replaceable::Field(field) => {
                    freeze_the_epics_row(&mut app);
                    app.apply(Action::Edit(field)).unwrap();
                }
                data::Replaceable::CommentText => open_the_comment_edit(&mut app),
            }
            type_into(&mut app, "mine");
            let typed = field_value(&app);

            // Somebody else writes while the reader is composing theirs, which is
            // the one window the store's own lock cannot cover. The stamp is the
            // entity's rather than the field's, so any change to it refuses.
            fx.rewrite_the_epics_body("theirs\n");
            app.apply(Action::Accept).unwrap();
            assert_eq!(
                app.mode(),
                Mode::Dialog(Answers::Conflict),
                "{field:?} was written over a change that landed under it"
            );

            // The way out keeps the text and the session, whichever field it is:
            // only a successful write ends one.
            app.apply(Action::Unwind).unwrap();
            assert_eq!(field_value(&app), typed, "{field:?}");
            assert!(
                app.editing_target().is_some(),
                "{field:?} ended the session"
            );
            app.apply(Action::Unwind).unwrap();
            app.apply(Action::Delete).unwrap(); // the field is dirty, so it asks
            app.apply(Action::Unwind).unwrap();
        }
    }

    /// Open the state picker on the frozen row, the way a reader does.
    fn open_the_state_picker(app: &mut App) {
        app.apply(Action::SetState).unwrap();
        assert!(app.surface().is_some(), "the state key opened no surface");
    }

    /// Mark the state that goes by this word, moving the way a reader does, and fail
    /// rather than run on if the picker will not reach it.
    ///
    /// Bounded by how many values there are: a mark that stopped moving would
    /// otherwise spin with nothing to say.
    fn mark_the_state(app: &mut App, wanted: &str) {
        let picker = |app: &App| options_of(&app.surface().expect("a surface is open").fields()[0]);
        let (options, _) = picker(app);
        let at = options
            .iter()
            .position(|option| *option == wanted)
            .unwrap_or_else(|| panic!("{wanted:?} is not offered: {options:?}"));
        for _ in 0..options.len() {
            let (_, on) = picker(app);
            match on.cmp(&at) {
                std::cmp::Ordering::Less => app.apply(Action::MoveDown).unwrap(),
                std::cmp::Ordering::Greater => app.apply(Action::MoveUp).unwrap(),
                std::cmp::Ordering::Equal => break,
            };
        }
        assert_eq!(
            marked(&app.surface().unwrap().fields()[0]),
            wanted,
            "the mark would not reach {wanted:?}"
        );
    }

    /// The labels of the fields the open surface holds, top to bottom — which is what
    /// a reader is being asked for.
    fn field_labels(app: &App) -> Vec<String> {
        app.surface()
            .expect("a surface is open")
            .fields()
            .iter()
            .map(|field| field.label().to_string())
            .collect()
    }

    #[test]
    fn nothing_typed_reaches_a_picker_and_no_editor_is_ever_handed_one() {
        let (_fx, mut app) = app();
        freeze_a_ticket_row(&mut app);
        open_the_state_picker(&mut app);
        let was = marked(&app.surface().unwrap().fields()[0]);

        // A picker holds no text, so the keys that write text change nothing in it —
        // and leave it clean, because nothing in it could have changed. The way out is
        // then silent, as it is on any field nothing has been done to.
        for action in [
            Action::Insert('x'),
            Action::Insert('\n'),
            Action::DeleteBefore,
            Action::DeleteAfter,
            Action::MoveLeft,
            Action::MoveRight,
            Action::MoveToStart,
            Action::MoveToEnd,
        ] {
            app.apply(action).unwrap();
            assert_eq!(
                marked(&app.surface().unwrap().fields()[0]),
                was,
                "{action:?} changed what the picker holds"
            );
            assert!(
                !app.surface().unwrap().fields()[0].is_dirty(),
                "{action:?} dirtied a picker it could not change"
            );
        }

        // And there is nothing to hand an external editor: the key that hands a field
        // over is not bound in a picker, and the handoff refuses one in any case — an
        // editor opened on a picker would ask the reader to type a value they can only
        // mark.
        app.apply(Action::ExternalEditor).unwrap();
        assert_eq!(app.take_editor_handoff(), None);

        // The way out of a picker nothing has been done to goes at once, with no
        // question: there is nothing in it that could be lost.
        app.apply(Action::Unwind).unwrap();
        assert!(
            app.surface().is_none(),
            "an untouched picker asked a question"
        );
        assert!(app.editing_target().is_some(), "the way out left the mode");
    }

    #[test]
    fn marking_a_value_is_a_change_and_the_list_of_values_has_ends() {
        let (_fx, mut app) = app();
        freeze_a_ticket_row(&mut app);
        open_the_state_picker(&mut app);
        let (options, opened_on) = options_of(&app.surface().unwrap().fields()[0]);

        // The mark is the value, so moving it is what changes what a save would write:
        // the way out has to warn about it exactly as it does about typing.
        app.apply(Action::MoveDown).unwrap();
        assert_eq!(
            marked(&app.surface().unwrap().fields()[0]),
            options[opened_on + 1]
        );
        assert!(app.surface().unwrap().fields()[0].is_dirty());
        app.apply(Action::Unwind).unwrap();
        assert!(
            matches!(app.modal(), Some(Modal::Dialog(_))),
            "moving the mark asked nothing on the way out"
        );
        // The question is about the field the mark is in, so it names it.
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("a question is open")
        };
        assert!(
            dialog.message().contains(STATE_FIELD),
            "{:?}",
            dialog.message()
        );

        // Answered by throwing the pick away, which leaves the mode standing: the
        // question was about the surface, and the way out unwinds one layer at a time.
        app.apply(Action::Delete).unwrap();
        assert!(app.surface().is_none(), "the discard kept the surface");

        // The list has ends rather than wrapping round: a mark that leapt from the
        // last value to the first would be a screen arguing with the keyboard, since
        // the values are drawn as the list these keys walk.
        open_the_state_picker(&mut app);
        assert_eq!(
            opened_on, 0,
            "this ticket does not open on the first value, so pushing up cannot stay put"
        );
        for _ in 0..options.len() + 2 {
            app.apply(Action::MoveUp).unwrap();
        }
        assert_eq!(marked(&app.surface().unwrap().fields()[0]), options[0]);
        // And a key that finds no value that way has changed nothing, so it leaves the
        // field clean: the way out of a picker the reader only pushed at is silent.
        assert!(
            !app.surface().unwrap().fields()[0].is_dirty(),
            "a mark that never moved dirtied the field"
        );
        for _ in 0..options.len() + 2 {
            app.apply(Action::MoveDown).unwrap();
        }
        assert_eq!(
            marked(&app.surface().unwrap().fields()[0]),
            options[options.len() - 1]
        );
    }

    #[test]
    fn the_reason_typed_is_what_the_store_holds_and_the_next_picker_opens_on_it() {
        let (fx, mut app) = app();
        freeze_a_ticket_row(&mut app);
        open_the_state_picker(&mut app);

        // A state that has to say why, and the words that say it. They are the whole
        // record of why the row stopped, so they reach the store as the reader wrote
        // them: a surface that mangled or mislaid them would lose the only copy.
        let was = marked(&app.surface().unwrap().fields()[0]);
        mark_the_state(&mut app, "blocked");
        app.apply(Action::NextField).unwrap();
        type_into(&mut app, "waiting on the store");
        app.apply(Action::Accept).unwrap();
        assert!(app.surface().is_none(), "the pick did not go through");
        assert_eq!(
            fx.node_state(&fx.node),
            (
                "blocked".to_string(),
                Some("waiting on the store".to_string()),
                None
            )
        );

        // And the next picker opens marked on what the store now holds rather than on
        // the first value it lists: opening on the first would mean the save key alone
        // moved a row nobody asked to move, and said so as though it were meant.
        freeze_a_ticket_row(&mut app);
        open_the_state_picker(&mut app);
        let (options, at) = options_of(&app.surface().unwrap().fields()[0]);
        assert_eq!(options[at], "blocked");
        assert_ne!(at, 0, "blocked is the first value, so this proves nothing");
        assert_ne!(
            was, "blocked",
            "the row was already blocked, so this proves nothing"
        );
    }

    #[test]
    fn a_field_the_mark_reveals_again_does_not_take_the_keyboard_with_it() {
        let (_fx, mut app) = app();
        freeze_a_ticket_row(&mut app);
        open_the_state_picker(&mut app);

        // A state that says why reveals the field it says it in, and the keyboard
        // stays in the picker: the reader is marking values, not filling in a form
        // that moves under them.
        mark_the_state(&mut app, "blocked");
        assert_eq!(field_labels(&app), vec![STATE_FIELD, REASON_FIELD]);
        assert_eq!(app.surface().unwrap().focus(), 0);

        // The reader fills the reason in, then goes back to the picker — the two keys
        // that move between fields are the only way to — and marks a state that says
        // nothing about why. The reason goes with the mark.
        app.apply(Action::NextField).unwrap();
        type_into(&mut app, "waiting");
        app.apply(Action::PreviousField).unwrap();
        assert_eq!(app.surface().unwrap().focus(), 0);
        mark_the_state(&mut app, "done");
        assert_eq!(field_labels(&app), vec![STATE_FIELD]);

        // Marking a state that says why again reveals the field again — and the
        // keyboard is still in the picker rather than having been pulled into the
        // field that came back, so the next key moves the mark and does not type.
        mark_the_state(&mut app, "blocked");
        assert_eq!(field_labels(&app), vec![STATE_FIELD, REASON_FIELD]);
        assert_eq!(app.surface().unwrap().focus(), 0);
        app.apply(Action::MoveDown).unwrap();
        assert_eq!(marked(&app.surface().unwrap().fields()[0]), "done");
    }

    #[test]
    fn a_revealed_field_holds_what_is_typed_into_it_and_goes_when_the_mark_moves_off() {
        let (_fx, mut app) = app();
        freeze_a_ticket_row(&mut app);
        open_the_state_picker(&mut app);
        mark_the_state(&mut app, "blocked");

        // Every keystroke brings the field list back in line with what the picker
        // holds, so a revealed field has to survive the reader typing into it: a list
        // rebuilt from the mark alone would swallow each character as it landed.
        app.apply(Action::NextField).unwrap();
        type_into(&mut app, "waiting on review");
        let reason = app.surface().unwrap().fields()[1].clone();
        assert_eq!(reason.label(), REASON_FIELD);
        assert_eq!(text_of(&reason), "waiting on review");
        assert!(
            reason.is_dirty(),
            "the field the reader typed into is clean"
        );

        // A surface holds exactly the fields on screen: the field appears the moment
        // the mark lands on a state that wants it and goes the moment the mark moves
        // off, so it can never hold words the reader cannot see. Coming back therefore
        // asks afresh — the accepted cost of a picker with no confirming key, where
        // looking at another value is choosing it.
        app.apply(Action::PreviousField).unwrap();
        mark_the_state(&mut app, "done");
        assert_eq!(field_labels(&app), vec![STATE_FIELD]);
        mark_the_state(&mut app, "blocked");
        let reason = app.surface().unwrap().fields()[1].clone();
        assert_eq!(reason.label(), REASON_FIELD);
        assert_eq!(text_of(&reason), "");
        assert!(!reason.is_dirty(), "a field nobody has typed into is dirty");
    }

    #[test]
    fn an_affirmative_answer_that_belongs_to_another_question_performs_nothing() {
        let (fx, mut app) = app();
        let before = fx.epic_labels();

        // Every dialog carries the answers of its own set, and an intent reaches the
        // state machine without passing the key map — a mouse wheel does. So a
        // question must not be answered by another question's affirmative intent:
        // the letter that throws a label away is not the letter that overwrites
        // somebody else's text, and a dialog that only reports has nothing to go
        // ahead with at all.
        to_a_label_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Delete).unwrap();
        let asked = app.modal().cloned().expect("a question is open");
        app.apply(Action::Overwrite).unwrap();
        assert_eq!(app.modal(), Some(&asked), "the question was answered");
        assert_eq!(fx.epic_labels(), before, "something was written");
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();

        // A report has no affirmative answer at all, so neither intent performs
        // anything at one and it stands until it is dismissed.
        open_the_label_surface(&mut app);
        app.apply(Action::Accept).unwrap();
        let reported = app.modal().cloned().expect("a report is open");
        assert_eq!(app.mode(), Mode::Dialog(Answers::Acknowledge));
        for action in [Action::Delete, Action::Overwrite] {
            app.apply(action).unwrap();
            assert_eq!(app.modal(), Some(&reported), "{action:?} answered a report");
        }
        assert_eq!(fx.epic_labels(), before, "something was written");
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();

        // And the other way round: the destructive letter does not answer the
        // question about somebody else's text.
        let body = fx.epic_body();
        open_the_body_buffer(&mut app);
        type_into(&mut app, "mine");
        fx.rewrite_the_epics_body("theirs\n");
        app.apply(Action::Accept).unwrap();
        let asked = app.modal().cloned().expect("a question is open");
        app.apply(Action::Delete).unwrap();
        assert_eq!(app.modal(), Some(&asked), "the question was answered");
        assert_eq!(fx.epic_body(), "theirs\n", "something was written");
        assert_ne!(
            fx.epic_body(),
            body,
            "the fixture cannot tell the two apart"
        );
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
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::OneLine));

        // One layer at a time: the overlay goes and the buffer stays, text and all.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert_eq!(field_value(&app), "kept");
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::OneLine));
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
    fn the_add_key_opens_one_short_reference_field_on_a_dependency_list_only() {
        let (fx, mut app) = app();
        let before = fx.node_blockers();
        to_the_blocked_by_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        // The row offers the addition, and the strip lists exactly what it offers.
        assert_eq!(app.editing_hints(), vec![hint_for(EditingAction::Add)]);
        hints_and_keys_agree(&mut app);

        app.apply(Action::Add).unwrap();
        let surface = app.surface().expect("the letter opened a surface");
        // One short field, empty and untouched: a blocker is named by a reference,
        // and nothing the browser put there itself could be one the reader meant.
        assert_eq!(surface.fields().len(), 1);
        assert_eq!(surface.focus(), 0);
        let field = &surface.fields()[0];
        assert_eq!(text_of(field), "");
        assert!(!field.is_dirty());
        // The float says what is being added and to what, since it covers the row
        // it was opened from.
        let (_, node) = fx.node_reference_forms();
        assert!(surface.title().contains(&node), "{:?}", surface.title());
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::OneLine));

        // There is nothing to send without a reference, so an accepted empty field
        // warns naming it and writes nothing — which is the browser refusing to send
        // an empty field, not a store rule about references reimplemented.
        app.apply(Action::Accept).unwrap();
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!(
                "an empty reference wrote or said nothing: {:?}",
                app.modal()
            )
        };
        assert!(dialog.message().contains(BLOCKER_FIELD), "{dialog:?}");
        assert_eq!(fx.node_blockers(), before, "the warning wrote something");
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();

        // An epic is not a unit of work that can be blocked, so its level carries no
        // dependency list at all: there is no row an addition could be offered on.
        to_the_roster(&mut app);
        app.apply(Action::Descend).unwrap(); // into the epic
        assert!(
            app.nav().rows().iter().all(|row| !matches!(
                &row.kind,
                RowKind::Collection(c) if c.name() == "blocked-by"
            )),
            "an epic was offered a dependency list: {:?}",
            app.nav().rows()
        );
    }

    #[test]
    fn an_epics_dependency_list_is_offered_nothing_even_when_a_row_names_one() {
        let (fx, mut app) = app();
        // An epic is not a unit of work that can be blocked, so no row of the
        // browser points at a dependency list of one. The offer says so itself
        // rather than trusting that: what a row may be given is decided from what
        // the row names, so a level that grew a row it should not have would still
        // be offered nothing to write with it.
        app.editing = Some(Selection::Collection(
            Container::Epic(fx.epic.clone()),
            Collection::BlockedBy,
        ));
        assert!(app.editing_hints().is_empty());
        app.apply(Action::Add).unwrap();
        assert!(app.surface().is_none(), "an epic was offered a blocker");
        assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION));
    }

    #[test]
    fn a_blocker_row_offers_a_removal_and_cannot_be_followed_to_the_node_it_names() {
        let (_fx, mut app) = app();
        to_a_blocker_row(&mut app);
        let level = app.nav().crumbs().join("/");

        // A collection member is a leaf, a blocker included: what blocks you is read
        // where it stands, and there is no navigation from the entry to the node it
        // names — so the key that opens a level says why nothing happened instead.
        assert!(!app.nav().frame().current().unwrap().enterable());
        app.apply(Action::Descend).unwrap();
        assert_eq!(app.nav().crumbs().join("/"), level);
        assert!(app.flash_message().is_some(), "nothing said why");
        app.clear_flash();

        // A dependency list has no rename, so an entry is only ever removed, and the
        // strip lists exactly that.
        app.apply(Action::EnterEditing).unwrap();
        assert_eq!(app.editing_hints(), vec![hint_for(EditingAction::Delete)]);
        hints_and_keys_agree(&mut app);
    }

    #[test]
    fn a_blocker_is_added_in_either_form_and_the_notice_names_what_was_recorded() {
        let (fx, mut app) = app();
        let before = fx.node_blockers();
        let (bare, whole) = fx.another_node();

        // A bare number names a node of the dependency list's own epic, which is the
        // shorthand every reference is written in, and the store records the whole
        // reference whichever way it was typed.
        add_a_blocker(&mut app, &bare);
        let mut expected = before.clone();
        expected.push(whole.clone());
        assert_eq!(fx.node_blockers(), expected);
        // A successful write ends the session, surface and all, and says what it did
        // — naming the blocker as the store holds it, so a notice about a bare number
        // does not read as a ticket belonging to no epic.
        assert!(app.surface().is_none());
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.mode(), Mode::Browse);
        let notice = app.flash_message().expect("every write says what it did");
        assert!(notice.contains(&whole), "{notice:?}");
        // The store was re-read: the list under the row the reader stands on now
        // holds one more entry than it did.
        let row = app.nav().frame().current().expect("a highlighted row");
        assert_eq!(row.children, expected.len());

        // Repeating the same request is a successful no-op in the store. The
        // browser must not call it an addition when the list the reader sees did
        // not change.
        add_a_blocker(&mut app, &whole);
        assert_eq!(fx.node_blockers(), expected);
        assert_eq!(
            app.flash_message(),
            Some(format!("blocker {whole} is already listed").as_str())
        );

        // And a whole reference is the other form a reader writes one in — the form
        // that reaches another epic, which is the only thing distinguishing it from
        // the shorthand, and unprovable against a node of this epic.
        let (_, elsewhere) = fx.a_node_of_another_epic();
        add_a_blocker(&mut app, &elsewhere);
        expected.push(elsewhere.clone());
        assert_eq!(fx.node_blockers(), expected);
        let notice = app.flash_message().expect("every write says what it did");
        assert!(notice.contains(&elsewhere), "{notice:?}");
    }

    #[test]
    fn a_reference_the_store_will_not_take_is_refused_in_the_stores_own_words() {
        let (fx, mut app) = app();
        let before = fx.node_blockers();
        let refusal_for = |reference: &str| {
            store_refusal(
                &fx.store,
                &data::Write::AddBlocker(fx.blocked_by_selection(), reference.to_string()),
                "the store judges what may block what",
            )
        };

        // The browser judges nothing about the reference: a blocker that does not
        // exist is the store's judgement, so the action is offered, attempted, and
        // what comes back is shown verbatim — compared against the message the seam
        // itself produces, never a string spelled out here, which is what a browser
        // precondition or a reworded refusal would pass.
        open_the_blocker_surface(&mut app);
        type_into(&mut app, "999");
        app.apply(Action::Accept).unwrap();
        assert_eq!(
            app.modal(),
            Some(&Modal::Dialog(Box::new(Dialog::refusal(refusal_for(
                "999"
            )))))
        );
        assert_eq!(app.mode(), Mode::Dialog(Answers::Acknowledge));
        assert_eq!(app.flash_message(), None, "a failure is never a notice");
        assert_eq!(fx.node_blockers(), before, "a refusal wrote something");

        // A refused save keeps the buffer and the session: only a successful write
        // ends it, so the reference is still there to be corrected.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(field_value(&app), "999");
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::OneLine));
        assert!(app.editing_target().is_some());

        // A node blocking itself is the store's judgement in the same way — the
        // browser knows which node the list belongs to and still does not pre-check
        // it, so the rule lives in one place.
        for _ in 0.."999".len() {
            app.apply(Action::DeleteBefore).unwrap();
        }
        let (own_number, _) = fx.node_reference_forms();
        type_into(&mut app, &own_number);
        app.apply(Action::Accept).unwrap();
        assert_eq!(
            app.modal(),
            Some(&Modal::Dialog(Box::new(Dialog::refusal(refusal_for(
                &own_number
            )))))
        );
        assert_eq!(fx.node_blockers(), before, "a self-block was written");
    }

    #[test]
    fn a_blocker_removal_asks_naming_the_entry_and_a_cancel_changes_nothing() {
        let (fx, mut app) = app();
        to_a_blocker_row(&mut app);
        let blocker = row_label(&app);
        app.apply(Action::EnterEditing).unwrap();

        app.apply(Action::Delete).unwrap();
        // The question names the entry: the frozen row is dimmed and the entries of
        // a list read alike, so an unnamed question would not say which one goes.
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!(
                "a deletion is gated behind a confirmation: {:?}",
                app.modal()
            )
        };
        assert!(dialog.message().contains(&blocker), "{dialog:?}");
        assert_eq!(dialog.answers(), Answers::Destructive);
        assert!(
            fx.node_blockers().contains(&blocker),
            "asking wrote something"
        );

        // The answer that is never destructive, here as everywhere else — and it
        // unwinds one layer: the question goes and the mode stays on its row.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert!(
            fx.node_blockers().contains(&blocker),
            "a cancel wrote something"
        );
        assert_eq!(app.mode(), Mode::Editing);
        assert_eq!(row_label(&app), blocker);
        assert_eq!(app.flash_message(), None, "nothing happened worth saying");
    }

    #[test]
    fn a_confirmed_removal_takes_the_entry_the_row_names_and_no_other() {
        let (fx, mut app) = app();
        // Two entries, so the entry the row names is a claim rather than a
        // coincidence: with one, a removal that emptied the whole list would pass.
        let (bare, added) = fx.another_node();
        add_a_blocker(&mut app, &bare);
        let before = fx.node_blockers();
        assert!(before.len() > 1, "the promise needs more than one entry");

        to_a_blocker_row(&mut app);
        // The last entry rather than the first: an entry that is only ever at the
        // top could not tell the one the row names from the one the list leads with.
        app.apply(Action::CursorLast).unwrap();
        let blocker = row_label(&app);
        assert_eq!(blocker, added, "the cursor is not on the entry just added");
        app.apply(Action::EnterEditing).unwrap();

        // The letter that asks is the letter that answers: one key for everything
        // destructive, learned once.
        app.apply(Action::Delete).unwrap();
        app.apply(Action::Delete).unwrap();

        assert_eq!(app.modal(), None);
        let survivors: Vec<String> = before.into_iter().filter(|b| *b != blocker).collect();
        assert_eq!(fx.node_blockers(), survivors);
        assert!(
            !survivors.is_empty(),
            "an entry the row did not name went too"
        );
        // A successful write ends the session and says what it did, naming the
        // blocker, because by the time the notice is read its row is gone.
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.mode(), Mode::Browse);
        let notice = app.flash_message().expect("every write says what it did");
        assert!(notice.contains(&blocker), "{notice:?}");
        // The store is re-read with it: an entry that no longer exists must not stay
        // on screen for the next keypress to act on.
        assert!(app.nav().rows().iter().all(|r| r.label != blocker));
    }

    #[test]
    fn an_asset_deletion_asks_naming_the_asset_and_a_cancel_changes_nothing() {
        let (fx, mut app) = app();
        to_an_asset_row(&mut app);
        let asset = row_label(&app);
        app.apply(Action::EnterEditing).unwrap();
        // The browser cannot attach or replace an asset, so its row offers a
        // deletion and nothing else, and the strip lists exactly that.
        assert_eq!(app.editing_hints(), vec![hint_for(EditingAction::Delete)]);
        hints_and_keys_agree(&mut app);

        app.apply(Action::Delete).unwrap();
        // The question names the asset: the frozen row is dimmed and the members of
        // a collection read alike, so an unnamed question would not say what goes —
        // and here what goes are bytes the store keeps no tombstone for.
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!(
                "a deletion is gated behind a confirmation: {:?}",
                app.modal()
            )
        };
        assert!(dialog.message().contains(&asset), "{dialog:?}");
        assert_eq!(dialog.answers(), Answers::Destructive);
        assert!(fx.epic_assets().contains(&asset), "asking wrote something");

        // The answer that is never destructive, here as everywhere else — and it
        // unwinds one layer: the question goes and the mode stays on its row.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert!(
            fx.epic_assets().contains(&asset),
            "a cancel wrote something"
        );
        assert_eq!(app.mode(), Mode::Editing);
        assert_eq!(row_label(&app), asset);
        assert_eq!(app.flash_message(), None, "nothing happened worth saying");
    }

    #[test]
    fn a_confirmed_asset_deletion_takes_the_asset_the_row_names_and_no_other() {
        let (fx, mut app) = app();
        // Two assets, so the asset the row names is a claim rather than a
        // coincidence: with one, a deletion that emptied the level would pass.
        let added = fx.another_asset();
        let before = fx.epic_assets();
        assert!(before.len() > 1, "the promise needs more than one asset");

        to_an_asset_row(&mut app);
        // The last row rather than the first: an asset that is only ever at the top
        // could not tell the one the row names from the one the level leads with.
        app.apply(Action::CursorLast).unwrap();
        let asset = row_label(&app);
        assert_eq!(asset, added, "the cursor is not on the asset just added");
        app.apply(Action::EnterEditing).unwrap();

        // The letter that asks is the letter that answers: one key for everything
        // destructive, learned once.
        app.apply(Action::Delete).unwrap();
        app.apply(Action::Delete).unwrap();

        assert_eq!(app.modal(), None);
        let survivors: Vec<String> = before.into_iter().filter(|a| *a != asset).collect();
        assert_eq!(fx.epic_assets(), survivors);
        assert!(
            !survivors.is_empty(),
            "an asset the row did not name went too"
        );
        // A successful write ends the session and says what it did, naming the
        // asset, because by the time the notice is read its row is gone.
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.mode(), Mode::Browse);
        let notice = app.flash_message().expect("every write says what it did");
        assert!(notice.contains(&asset), "{notice:?}");
        // The store is re-read with it: a row that no longer exists must not stay on
        // screen for the next keypress to act on.
        assert!(app.nav().rows().iter().all(|r| r.label != asset));
    }

    #[test]
    fn a_refused_asset_deletion_carries_the_stores_own_words_and_keeps_the_session_on() {
        let (fx, mut app) = app();
        to_an_asset_row(&mut app);
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
        let refusal = store_refusal(
            &fx.store,
            &data::Write::DeleteAsset(target),
            "the store refuses an asset deletion on a missing entity",
        );
        assert_eq!(
            app.modal(),
            Some(&Modal::Dialog(Box::new(Dialog::refusal(refusal))))
        );
        assert_eq!(app.mode(), Mode::Dialog(Answers::Acknowledge));
        assert_eq!(app.flash_message(), None, "a failure is never a notice");

        // Only a successful write ends the session, so dismissing lands back in it.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.modal(), None);
        assert!(app.editing_target().is_some());
        assert_eq!(app.mode(), Mode::Editing);
    }

    #[test]
    fn the_assets_row_offers_nothing_and_names_the_command_that_attaches_one() {
        let (fx, mut app) = app();
        let before = fx.epic_assets();
        to_the_assets_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();

        // Attaching a payload is picking a file and carrying bytes about, which the
        // browser does not do at all: the row offers no action, so the strip lists
        // no letter for it — there is no dimmed, present-but-unavailable hint.
        assert!(app.editing_hints().is_empty());
        hints_and_keys_agree(&mut app);

        // The letter pressed anyway writes nothing, opens nothing, and names the
        // command that does the job instead — the epic's own, since an epic's assets
        // and a node's assets are different commands.
        app.apply(Action::Add).unwrap();
        assert!(app.surface().is_none(), "the assets row opened a surface");
        assert_eq!(app.modal(), None, "the assets row raised a dialog");
        assert_eq!(fx.epic_assets(), before, "the signpost wrote something");
        let notice = app.flash_message().expect("the row says where to go");
        assert!(
            notice.contains(&format!("loti epic asset add {} --file", fx.epic)),
            "{notice:?}"
        );
        // And the mode is still on: a letter a row does not offer is not an
        // implicit exit.
        assert!(app.editing_target().is_some());
        assert_eq!(app.mode(), Mode::Editing);
        app.apply(Action::Unwind).unwrap();

        // A node's assets are reached by the noun the command line gives a node,
        // and by the node's own reference: a signpost naming the container the
        // reader is not standing on sends them to the wrong assets.
        to_the_roster(&mut app);
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        app.apply(Action::Descend).unwrap(); // into the ticket
        let node = app.nav().crumbs().len();
        assert!(node > 2, "the cursor is not inside a ticket");
        to_row(
            &mut app,
            |kind| matches!(kind, RowKind::Collection(c) if c.name() == "assets"),
        );
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Add).unwrap();
        let (_, reference) = fx.node_reference_forms();
        let notice = app.flash_message().expect("the row says where to go");
        assert!(
            notice.contains(&format!("loti ticket asset add {reference} --file")),
            "{notice:?}"
        );
    }

    /// Stand on the fixture's ticket row, inside its epic, which is the row a
    /// claim is taken and released on: a claim is taken on a unit of work.
    fn to_the_tickets_row(app: &mut App) {
        to_the_roster(app);
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(app);
    }

    /// Freeze the ticket's row with a claim already held on it, as another writer
    /// left it: the row is what says a claim is held, so it has to be re-read
    /// before the offer is asked.
    fn freeze_a_claimed_ticket(fx: &Fixture, app: &mut App) -> String {
        let holder = fx.claim(&fx.node);
        to_the_tickets_row(app);
        app.apply(Action::Reload).unwrap();
        app.apply(Action::EnterEditing).unwrap();
        holder
    }

    #[test]
    fn the_claim_pair_is_offered_on_a_unit_of_work_and_release_only_while_it_is_held() {
        let (fx, mut app) = app();
        to_the_tickets_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();

        // Taking is offered whatever the row's claim state, because taking is
        // reassigning: a claim has one holder, so there is no second letter for
        // taking one that is held. Giving up is offered only while one is — there is
        // nothing to give up on a row nobody is on — and an action a row does not
        // offer is not listed at all: there is no dimmed, present-but-unavailable
        // hint anywhere in the mode.
        let hints = app.editing_hints();
        assert!(
            hints.contains(&hint_for(EditingAction::TakeClaim)),
            "{hints:?}"
        );
        assert!(
            !hints.contains(&hint_for(EditingAction::ReleaseClaim)),
            "an unclaimed row offered a release: {hints:?}"
        );
        hints_and_keys_agree(&mut app);
        app.apply(Action::Unwind).unwrap();

        // Held, the same row offers both halves of the pair.
        let holder = freeze_a_claimed_ticket(&fx, &mut app);
        let hints = app.editing_hints();
        for action in [EditingAction::TakeClaim, EditingAction::ReleaseClaim] {
            assert!(hints.contains(&hint_for(action)), "{action:?}: {hints:?}");
        }
        // The offer follows the row's own claim rather than a state captured when
        // the mode was entered: with the claim gone from under the frozen row, the
        // release is not offered on the next frame.
        fx.release(&fx.node);
        app.apply(Action::Reload).unwrap();
        assert!(
            !app.editing_hints()
                .contains(&hint_for(EditingAction::ReleaseClaim)),
            "a released claim is still offered a release"
        );
        assert!(
            app.editing_target().is_some(),
            "a claim released elsewhere ended the session"
        );
        assert!(!holder.is_empty(), "the fixture claimed for nobody");
    }

    #[test]
    fn nothing_but_a_unit_of_works_own_row_is_offered_a_claim() {
        let (fx, mut app) = app();
        // A blocker entry reads as work and carries the claim marker, so it is the
        // row a claim letter would wrongly reach: the entry is a reference to a node
        // and not that node's own row, which is where its claim is acted on.
        fx.claim(&fx.blocker);
        to_a_blocker_row(&mut app);
        app.apply(Action::Reload).unwrap();
        app.apply(Action::EnterEditing).unwrap();
        let hints = app.editing_hints();
        for action in [EditingAction::TakeClaim, EditingAction::ReleaseClaim] {
            assert!(!hints.contains(&hint_for(action)), "{action:?}: {hints:?}");
        }
        hints_and_keys_agree(&mut app);
        app.apply(Action::Unwind).unwrap();

        // An epic is not a unit of work, so its row is offered neither half however
        // its own tickets are claimed.
        freeze_the_epics_row(&mut app);
        let hints = app.editing_hints();
        for action in [EditingAction::TakeClaim, EditingAction::ReleaseClaim] {
            assert!(!hints.contains(&hint_for(action)), "{action:?}: {hints:?}");
        }
        hints_and_keys_agree(&mut app);
    }

    #[test]
    fn a_release_is_offered_from_the_frozen_rows_own_claim_and_no_others() {
        let (fx, mut app) = app();
        fx.claim(&fx.node);
        // The holder comes off the frozen row, so the row has to be the row the mode
        // is acting on: a mode standing somewhere else must not be offered the claim
        // of whatever the cursor happens to be on, and the offer says so itself
        // rather than trusting the two to agree.
        to_the_tickets_row(&mut app);
        app.apply(Action::Reload).unwrap();
        app.editing = Some(fx.epic_selection());
        assert!(!app
            .editing_hints()
            .contains(&hint_for(EditingAction::ReleaseClaim)));

        // And the other way round: the mode on the claimed ticket while the cursor
        // has been put elsewhere is offered nothing off that other row either.
        app.editing = Some(Selection::Node(fx.node.clone()));
        app.nav.cursor_first();
        assert!(!app
            .editing_hints()
            .contains(&hint_for(EditingAction::ReleaseClaim)));
        app.apply(Action::ReleaseClaim).unwrap();
        assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION));
        assert!(
            fx.node_claim().is_some(),
            "a release wrote from a row the mode was not on"
        );
    }

    #[test]
    fn taking_a_claim_asks_who_is_taking_it_and_sends_nothing_without_one() {
        let (fx, mut app) = app();
        to_the_tickets_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::TakeClaim).unwrap();

        let surface = app.surface().expect("the letter opened a surface");
        // One short field, empty and untouched: the holder is freeform text the
        // reader supplies, and nothing the browser put there itself — the holder
        // already on the row included — could be who is picking the work up now.
        assert_eq!(surface.fields().len(), 1);
        assert_eq!(surface.focus(), 0);
        assert_eq!(text_of(&surface.fields()[0]), "");
        assert!(!surface.fields()[0].is_dirty());
        assert_eq!(surface.placement(), Placement::Float);
        // The float says which ticket is being claimed, since it covers the row it
        // was opened from.
        let (_, reference) = fx.node_reference_forms();
        assert!(
            surface.title().contains(&reference),
            "{:?}",
            surface.title()
        );
        assert_eq!(app.mode(), surface_mode(Fields::One, FieldKind::OneLine));

        // A claim with no holder is a row marked for nobody, so an accepted empty
        // field warns naming it and sends nothing — the browser refusing to send a
        // field the reader never filled in, not a store rule reimplemented.
        app.apply(Action::Accept).unwrap();
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("an empty holder wrote or said nothing: {:?}", app.modal())
        };
        assert!(dialog.message().contains(CLAIM_FIELD), "{dialog:?}");
        assert_eq!(fx.node_claim(), None, "the warning wrote something");
    }

    #[test]
    fn the_holder_asked_for_is_empty_even_where_somebody_already_holds_the_claim() {
        let (fx, mut app) = app();
        let held = freeze_a_claimed_ticket(&fx, &mut app);
        assert!(!held.is_empty(), "the fixture claimed for nobody");

        app.apply(Action::TakeClaim).unwrap();
        let surface = app.surface().expect("the letter opened a surface");
        // Seeding the field with whoever holds it now would make the save key alone
        // enough to write that same holder back and report the claim as taken, so a
        // reader who meant to take it would be told they had while nothing moved.
        // Who is picking the work up is never something the browser can supply.
        assert_eq!(
            text_of(&surface.fields()[0]),
            "",
            "the field was seeded with the holder it is replacing"
        );
        assert!(!surface.fields()[0].is_dirty());

        // And the previous holder is still on the row underneath: the surface asks
        // rather than assuming, and asking has not written anything yet.
        assert_eq!(fx.node_claim().map(|c| c.by), Some(held));
    }

    #[test]
    fn a_claim_taken_ends_the_session_and_a_release_asks_nothing_at_all() {
        let (fx, mut app) = app();
        to_the_tickets_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::TakeClaim).unwrap();
        type_into(&mut app, "a human");
        app.apply(Action::Accept).unwrap();

        // A session is one edit long, so the write that succeeded ends it, surface
        // and all: the notice arriving as the mode indicator goes is what reads as
        // "that finished".
        assert_eq!(app.modal(), None);
        assert!(app.surface().is_none());
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.mode(), Mode::Browse);

        // Giving it up again needs nothing typed and asks nothing: the row carries
        // the claim, so the letter is the whole interaction — a confirmation is what
        // stands in front of a deletion, and a claim is state on a unit of work.
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::ReleaseClaim).unwrap();
        assert_eq!(app.modal(), None, "a release asked something");
        assert!(app.surface().is_none(), "a release opened a surface");
        assert_eq!(app.editing_target(), None);
        assert_eq!(app.mode(), Mode::Browse);
        assert!(fx.node_claim().is_none(), "the claim is still held");
    }

    /// Open the form that creates an epic, the way a reader does: from the epics
    /// list, with the browser's own key.
    fn open_the_epic_form(app: &mut App) {
        to_the_roster(app);
        app.apply(Action::CreateEpic).unwrap();
        assert!(app.surface().is_some(), "the epic key opened no form");
    }

    /// Fill the open form in, field by field from the first, the way a reader does:
    /// type, move on with the field key, type again.
    fn fill(app: &mut App, values: &[&str]) {
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                app.apply(Action::NextField).unwrap();
            }
            type_into(app, value);
        }
    }

    /// Replace the id the open epic form holds, the way a reader does: back to the
    /// start of the field, out with what is there, in with the new.
    ///
    /// Bounded by how many fields the form holds, so a field key that stopped
    /// moving fails here rather than spinning with nothing to say.
    fn retype_the_id(app: &mut App, id: &str) {
        let fields = app.surface().map_or(0, |surface| surface.fields().len());
        for _ in 0..=fields {
            if app.surface().map(Surface::focus) == Some(0) {
                break;
            }
            app.apply(Action::NextField).unwrap();
        }
        assert_eq!(
            app.surface().map(Surface::focus),
            Some(0),
            "the field keys would not reach the id"
        );
        app.apply(Action::MoveToStart).unwrap();
        empty_the_field(app);
        type_into(app, id);
    }

    /// What the open dialog is titled, which is what says whose rule the reader has
    /// run into: the browser's own, or the store's shown verbatim.
    fn dialog_title(app: &App) -> &str {
        match app.modal() {
            Some(Modal::Dialog(dialog)) => dialog.title(),
            other => panic!("no dialog is open: {other:?}"),
        }
    }

    /// What the open dialog says.
    fn dialog_message(app: &App) -> &str {
        match app.modal() {
            Some(Modal::Dialog(dialog)) => dialog.message(),
            other => panic!("no dialog is open: {other:?}"),
        }
    }

    /// The epic ids the roster lists, as the browser holds them after its own
    /// reload — so a test about a creation asserts what the reader now sees.
    fn roster_ids(app: &App) -> Vec<String> {
        app.nav()
            .frame()
            .rows
            .iter()
            .map(|row| row.label.clone())
            .collect()
    }

    #[test]
    fn the_epic_key_opens_its_form_at_the_epics_list_and_nowhere_else() {
        let (fx, mut app) = app();
        open_the_epic_form(&mut app);
        let surface = app.surface().expect("the form is open");
        // Three fields: the address it will have, and the pair every creation form
        // ends with. A float, because none of them holds prose.
        assert_eq!(
            surface
                .fields()
                .iter()
                .map(Field::label)
                .collect::<Vec<_>>(),
            vec![EPIC_ID_FIELD, "name", "summary"]
        );
        assert_eq!(surface.placement(), Placement::Float);
        assert_eq!(surface.focus(), 0);
        // Several fields, and the key map is told so, which is what binds the keys
        // that move between them.
        assert_eq!(
            app.mode(),
            surface_mode(Fields::Several, FieldKind::OneLine)
        );
        // No row is frozen: an epic has no container row to be added from, which is
        // the whole reason this is not an action inside the mode.
        assert_eq!(app.editing_target(), None);
        app.apply(Action::Unwind).unwrap();

        // And nowhere else. Inside an epic and inside one of its tickets the key
        // opens nothing and says where an epic is made instead — silence would read
        // as a broken key, since the key list teaches it without saying where.
        app.apply(Action::Descend).unwrap(); // into the epic
        for _ in 0..2 {
            app.clear_flash();
            app.apply(Action::CreateEpic).unwrap();
            assert!(
                app.surface().is_none(),
                "a form opened below the epics list: {:?}",
                app.nav().crumbs()
            );
            assert_eq!(
                app.flash_message(),
                Some(EPICS_ARE_MADE_FROM_THE_EPICS_LIST),
                "{:?}",
                app.nav().crumbs()
            );
            to_work_row(&mut app);
            app.apply(Action::Descend).unwrap(); // into the ticket
        }
        // Nothing was created on the way: the key that opened nothing wrote nothing.
        to_the_roster(&mut app);
        assert_eq!(roster_ids(&app), vec![fx.epic.clone()]);
    }

    #[test]
    fn the_epic_key_is_the_one_write_the_roster_of_an_empty_store_still_offers() {
        let (_dir, store) = crate::data::fixture::empty_store();
        let mut app = App::new(store, Theme::with_color(false)).unwrap();
        assert!(app.nav().frame().current().is_none());

        // Editing mode acts on a row and this screen has none — which is exactly why
        // creating an epic is a key of the browser's own: it is the one write that
        // still works where there is nothing to stand on.
        app.apply(Action::EnterEditing).unwrap();
        assert_eq!(app.editing_target(), None);
        app.apply(Action::CreateEpic).unwrap();
        assert!(app.surface().is_some(), "the epic key opened no form");

        fill(&mut app, &["first", "The first effort", ""]);
        app.apply(Action::Accept).unwrap();

        // The reader is no longer on a screen with no selection: the write reloaded
        // the level, and the cursor has something to stand on.
        assert_eq!(app.modal(), None, "{:?}", app.modal());
        assert_eq!(roster_ids(&app), vec!["first".to_string()]);
        assert!(app.nav().frame().current().is_some());
        assert_eq!(app.mode(), Mode::Browse);
    }

    #[test]
    fn the_epic_key_is_as_unavailable_as_every_other_write_while_the_store_refuses_them() {
        let (fx, mut app) = app();
        turn_read_only_behind_the_browser(&fx, &mut app);
        let reason = app.read_only().expect("the store refuses every write");
        to_the_roster(&mut app);

        // This key does not pass the offer table, which is where every other write's
        // availability is decided, so it asks the store's own state for itself: a
        // store the format gate will not let this binary write offers no write at
        // all, and the reason is the store's own words rather than a paraphrase that
        // could go stale.
        app.apply(Action::CreateEpic).unwrap();
        assert!(
            app.surface().is_none(),
            "a store that may not be written opened a form"
        );
        assert_eq!(app.flash_message(), Some(reason.refusal().as_str()));
        assert_eq!(roster_ids(&app), vec![fx.epic.clone()]);

        // And it comes back with the store: read-only is a state a session leaves as
        // well as enters, so the refusal must not outlive the condition.
        crate::data::fixture::turn_writable(&fx.store);
        app.apply(Action::Reload).unwrap();
        app.apply(Action::CreateEpic).unwrap();
        assert!(app.surface().is_some(), "the form did not come back");
    }

    #[test]
    fn a_malformed_id_is_the_forms_own_refusal_and_a_taken_one_is_the_stores() {
        let (fx, mut app) = app();
        let before = roster_ids(&app);

        // The browser's own check, and the only value it makes one about: an epic
        // id is a plain name, and the store judges nothing about one today. Every
        // shape that is no name is caught before anything is sent at all, under a
        // title of the browser's own — a name with a separator in it, and the two
        // names that mean a directory rather than a name.
        open_the_epic_form(&mut app);
        fill(&mut app, &["", "Escaping", ""]);
        for malformed in ["../elsewhere", "a/b", ".", ".."] {
            retype_the_id(&mut app, malformed);
            // Accepted from a field that is not the id, so where the dismissal lands
            // is somewhere the keyboard was not already: a form is saved from
            // wherever the reader finished typing, and the field that has to change
            // is not that one.
            app.apply(Action::NextField).unwrap();
            assert_ne!(app.surface().map(Surface::focus), Some(0));
            app.apply(Action::Accept).unwrap();
            assert_eq!(dialog_title(&app), REJECTED_TITLE, "{malformed:?}");
            assert!(
                dialog_message(&app).contains("one name"),
                "{malformed:?}: {:?}",
                dialog_message(&app)
            );
            assert_eq!(roster_ids(&app), before, "{malformed:?} wrote something");
            // The buffer was never what the warning was about, so dismissing lands
            // back in the field that has to change, with the typing still there.
            app.apply(Action::Unwind).unwrap();
            assert_eq!(app.surface().map(Surface::focus), Some(0));
            assert_eq!(field_value(&app), malformed);
        }

        // A taken id is a different mechanism and reads as one: the browser asks
        // nothing about it, the store refuses under the lock, and what the reader is
        // shown is the store's own sentence under the store's own title.
        retype_the_id(&mut app, &fx.epic);
        app.apply(Action::Accept).unwrap();
        assert_eq!(dialog_title(&app), REFUSAL_TITLE);
        let its_own = store_refusal(
            &fx.store,
            &data::Write::CreateEpic {
                epic: fx.epic_selection(),
                name: "Escaping".to_string(),
                summary: String::new(),
            },
            "the store refuses an id it already holds",
        );
        assert_eq!(dialog_message(&app), its_own);
        assert_ne!(
            dialog_title(&app),
            REJECTED_TITLE,
            "the store's refusal was dressed as the browser's own"
        );
        // A refused write keeps the buffer and the form, so the reader can fix the
        // id rather than retype the whole thing.
        app.apply(Action::Unwind).unwrap();
        assert!(app.surface().is_some(), "a refusal closed the form");
        assert_eq!(roster_ids(&app), before);
    }

    #[test]
    fn accepting_a_form_looks_past_the_field_the_reader_is_in_at_every_required_one() {
        let (fx, mut app) = app();
        let before = roster_ids(&app);
        open_the_epic_form(&mut app);

        // An epic with no id has nowhere to live — the id is the name of the place
        // the store keeps it — so the id is required, and an empty form names it
        // first, being the first required field the accept walks past.
        app.apply(Action::Accept).unwrap();
        assert_eq!(dialog_title(&app), REQUIRED_TITLE);
        assert!(
            dialog_message(&app).contains(EPIC_ID_FIELD),
            "an empty id was sent: {:?}",
            dialog_message(&app)
        );
        assert_eq!(roster_ids(&app), before, "the warning wrote something");
        app.apply(Action::Unwind).unwrap();

        // The first field is filled and valid and the reader is standing in the
        // last: what stops the write is a required field neither of those is, so a
        // check that looked only at the field in front of the reader — or only at
        // the first — would send a form with no name in it.
        // The two fields the browser has no rule of its own about carry a separator,
        // which is ordinary text in them: the id's check is the id's alone, and a
        // rule that leaked onto its neighbours would refuse a name a reader may
        // perfectly well write.
        fill(&mut app, &["a-second-effort", "", "the read/write path"]);
        assert_eq!(app.surface().map(Surface::focus), Some(2));
        app.apply(Action::Accept).unwrap();

        assert_eq!(dialog_title(&app), REQUIRED_TITLE);
        assert!(
            dialog_message(&app).contains(data::FreeForm::Name.noun()),
            "{:?}",
            dialog_message(&app)
        );
        assert!(
            !dialog_message(&app).contains(EPIC_ID_FIELD),
            "a filled field was warned about: {:?}",
            dialog_message(&app)
        );
        assert_eq!(roster_ids(&app), before, "the warning wrote something");

        // Acknowledging lands in the field it named rather than where the reader
        // was, and what they type then goes there.
        app.apply(Action::Unwind).unwrap();
        assert_eq!(app.surface().map(Surface::focus), Some(1));
        type_into(&mut app, "Rework read/write");
        assert_eq!(field_value(&app), "Rework read/write");

        // The summary was never required, so with the name in place the form goes.
        app.apply(Action::Accept).unwrap();
        assert_eq!(app.modal(), None, "{:?}", app.modal());
        assert!(roster_ids(&app).contains(&"a-second-effort".to_string()));
        assert!(roster_ids(&app).contains(&fx.epic));
    }

    #[test]
    fn the_add_key_makes_a_ticket_on_an_epics_row_and_a_subticket_on_a_tickets() {
        let (fx, mut app) = app();
        freeze_the_epics_row(&mut app);
        // The row offers the addition and the strip teaches it, and every other
        // letter on the row agrees with the strip about itself.
        assert!(app.editing_hints().contains(&hint_for(EditingAction::Add)));
        hints_and_keys_agree(&mut app);

        app.apply(Action::Add).unwrap();
        let surface = app.surface().expect("the letter opened a form");
        // The pair every creation form ends with, and no id: an epic gives the new
        // ticket its number, so there is nothing here to address it by.
        assert_eq!(
            surface
                .fields()
                .iter()
                .map(Field::label)
                .collect::<Vec<_>>(),
            vec!["name", "summary"]
        );
        assert_eq!(surface.placement(), Placement::Float);
        // The float says what is being made and on what, since it covers the row it
        // was opened from.
        assert!(
            surface.title().contains("ticket") && surface.title().contains(&fx.epic),
            "{:?}",
            surface.title()
        );
        assert!(
            !surface.title().contains("subticket"),
            "an epic's row offered a subticket: {:?}",
            surface.title()
        );
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();

        // A ticket's row is a container too, and what it makes is a subticket: the
        // row decides, so one form covers both and the title follows the same answer
        // the write does.
        to_the_tickets_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Add).unwrap();
        let surface = app.surface().expect("the letter opened a form");
        let (_, reference) = fx.node_reference_forms();
        assert!(
            surface.title().contains("subticket") && surface.title().contains(&reference),
            "{:?}",
            surface.title()
        );

        // And a row that is no container of units of work offers no addition at all:
        // a collection member is a leaf.
        app.apply(Action::Unwind).unwrap();
        app.apply(Action::Unwind).unwrap();
        to_a_label_row(&mut app);
        app.apply(Action::EnterEditing).unwrap();
        app.apply(Action::Add).unwrap();
        assert!(app.surface().is_none(), "a label row offered a creation");
        assert_eq!(app.flash_message(), Some(NOT_AN_EDITING_ACTION));
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

    #[test]
    fn a_cursor_move_that_cannot_change_the_document_leaves_the_scroll_alone() {
        let (fx, mut app) = app();
        to_the_labels_row(&mut app);

        // A narrow, short frame, so the epic's own document — metadata table,
        // body and children — does not fit in one screen and a scroll actually
        // moves what is visible.
        let (width, height) = (40, 10);
        let top = preview_lines(&mut app, width, height);
        app.preview_viewer().scroll_down(3);
        let scrolled = preview_lines(&mut app, width, height);
        assert_ne!(
            scrolled, top,
            "the fixture's epic document is not tall enough at this frame size for a scroll to move it, so this test cannot prove anything"
        );

        // Moving across the epic's own collection rows cannot change what the pane
        // shows — every one of them keeps the epic's own document — so the pane
        // must stay exactly where it was scrolled to. The breadcrumb and the
        // navigation pane are free to change underneath it — a collection row and
        // a label row read as different rows there — which is exactly why only
        // the preview pane is compared.
        app.apply(Action::CursorDown).unwrap(); // labels -> comments
        assert_eq!(
            preview_lines(&mut app, width, height),
            scrolled,
            "moving to the next collection row of the same container reset the scroll"
        );

        // Standing on a label row keeps the same document too, so entering the
        // labels collection must not move the pane either.
        to_a_label_row(&mut app);
        assert_eq!(
            preview_lines(&mut app, width, height),
            scrolled,
            "entering a label row reset the scroll, though it shows the container's own document"
        );

        // Leaving to a document that actually differs — a ticket, not the epic —
        // must start that document at the top rather than carry the scroll over.
        to_the_roster(&mut app);
        app.apply(Action::Descend).unwrap(); // into the epic
        to_work_row(&mut app);
        let on_the_ticket = preview_lines(&mut app, width, height);

        // What the ticket's own document looks like at the top, from a second
        // browser on the same store that never touched a scrollbar — the only
        // honest baseline for "started at the top".
        let mut fresh = App::new(fx.store.clone(), Theme::with_color(false)).unwrap();
        fresh.apply(Action::Descend).unwrap();
        to_work_row(&mut fresh);
        let ticket_top = preview_lines(&mut fresh, width, height);
        assert_eq!(
            on_the_ticket, ticket_top,
            "moving to a different document did not start it at the top"
        );
    }
}
