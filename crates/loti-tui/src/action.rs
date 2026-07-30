//! Every user intent the browser can carry out, named independently of the keys
//! that trigger it.
//!
//! Invariant: [`Action`] is the only vocabulary the application state
//! understands. A new capability is a new variant plus one binding in
//! [`crate::keymap`]; a rebinding touches the keymap alone. Nothing here knows
//! about key codes, and nothing in the state machine matches on a key.

/// Which set of bindings is live, because one key may carry a different intent
/// in each mode.
///
/// Invariant: a mode is an input to the key-to-intent mapping and is derived from
/// the application state, never from a key — so the mapping stays the only place
/// a key is named, and the state machine stays the only place an intent is
/// interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Browsing the store: the cursor moves and every browse binding is live.
    Browse,
    /// Editing mode, with the selection frozen on one row.
    Editing,
    /// An editing surface is open, and every key belongs to the field it holds.
    ///
    /// No browse binding survives here: the paging keys a preview owns are the
    /// field's while it is open, and a letter is a character rather than an
    /// action, so nothing typed into a field can move, reload or edit anything
    /// underneath it.
    ///
    /// The shape of the surface travels with the mode, because a key means
    /// different things by it — see [`Shape`] — and a mode is the whole of what
    /// the key map is told.
    Surface(Shape),
    /// A dialog is open, admitting the set of answers it lists and nothing else.
    ///
    /// The set travels with the dialog rather than being a mode of its own, so a
    /// new kind of dialog is a set of answers here and never another mode.
    Dialog(Answers),
}

/// How many fields an open surface holds, to the precision a key's meaning turns
/// on: whether there is another field to move to at all.
///
/// Invariant: this reaches the key map, so the map decides what the reflex key
/// means instead of guessing — it accepts a surface with one field, and moves to
/// the next field on a surface with several, where accepting is the save key's
/// alone. A key map that could not tell them apart would either submit a form
/// half filled in or leave a one-field surface with no reflex way to finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fields {
    /// Exactly one, so there is nowhere to move: the field-navigation keys have
    /// nothing to reach and are not bound.
    One,
    /// More than one, so the fields are navigated and the reflex key moves
    /// between them.
    Several,
}

impl Fields {
    /// Every count a surface may have, so a surface that has to cover them all —
    /// the keys each answers, and the hints each lists — cannot then miss one.
    pub const ALL: &'static [Fields] = &[Fields::One, Fields::Several];

    /// The shape a surface holding this many fields has. A surface with nothing
    /// to fill in is not a surface, so anything below two is the one-field shape.
    pub fn of(count: usize) -> Self {
        match count {
            0 | 1 => Fields::One,
            _ => Fields::Several,
        }
    }
}

/// How many lines a field holds, which is the axis deciding whether a line break
/// is content or a key doing something else.
///
/// Invariant: a one-line field never holds a line break, whichever door text
/// arrives through — a keystroke, or an external editor's result. A field that
/// holds many keeps them, and is moved through by line as well as by character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    /// One line, so a line break is not content and there is no line to move to.
    One,
    /// As many as the reader writes, so a line break is a character like any
    /// other and the cursor moves between lines.
    Many,
}

impl Lines {
    /// Every kind a field may be, so a surface that has to cover them all — the
    /// keys each answers, and what each does with a line break — cannot miss one.
    pub const ALL: &'static [Lines] = &[Lines::One, Lines::Many];
}

/// An open surface as the key map is told it: how many fields it holds, and how
/// many lines the field the keyboard is in holds.
///
/// Invariant: the key map needs both halves, because the reflex key's meaning
/// turns on both — it accepts a surface with one field, moves on through a form,
/// and is a newline in a field that holds many lines however many fields there
/// are, where accepting is the save key's alone. Neither half is enough by
/// itself: a count alone would submit a body buffer on the reader's first
/// paragraph break, and a kind alone would submit a form half filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    /// How many fields the surface holds.
    pub fields: Fields,
    /// How many lines the focused field holds. The focused one, because a key is
    /// answered by the field it lands in and a surface may hold both kinds.
    pub lines: Lines,
}

impl Shape {
    /// Every shape a surface may have: every field count against every field
    /// kind, so a surface covering them all cannot miss a combination.
    pub const ALL: &'static [Shape] = &[
        Shape {
            fields: Fields::One,
            lines: Lines::One,
        },
        Shape {
            fields: Fields::One,
            lines: Lines::Many,
        },
        Shape {
            fields: Fields::Several,
            lines: Lines::One,
        },
        Shape {
            fields: Fields::Several,
            lines: Lines::Many,
        },
    ];
}

/// The answers a dialog admits, which are exactly the answers it lists.
///
/// Invariant: a dialog names the set it wants and the key map spells the letters,
/// so no dialog's own text names a key and no key is named outside that map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answers {
    /// A destructive question. It is answered by the same letter that asks for the
    /// destruction, so no key a hurried reader presses by reflex can be what
    /// destroys something: the way out answers it safely, and the key that
    /// normally means "yes, go on" is bound to nothing here at all.
    Destructive,
    /// A question about text that moved on under the reader: going ahead throws
    /// away somebody else's change, getting out keeps the buffer and decides
    /// nothing. A letter of its own rather than the destructive one, because what
    /// goes is not what the reader is looking at — and the reflex key is bound to
    /// nothing here either, for the same reason it is on a deletion.
    Conflict,
    /// Nothing is at stake — the dialog reports rather than asks — so it is
    /// dismissed rather than answered, and either dismissing key does it.
    Acknowledge,
}

impl Answers {
    /// Every set a dialog may ask for. A surface that has to cover them all —
    /// the letters, and the answers each set lists — cannot then miss one.
    pub const ALL: &'static [Answers] = &[
        Answers::Destructive,
        Answers::Conflict,
        Answers::Acknowledge,
    ];

    /// The intent that goes ahead with this set's question, and `None` for a set
    /// that only reports — such a dialog has a way out and nothing to go ahead
    /// with, so nothing it was raised for can be performed by mistake.
    ///
    /// Invariant: a set and the intent that answers it meet here, so the state
    /// machine that performs an answer names no intent of its own and an
    /// affirmative answer belonging to one set can never answer another set's
    /// question. The key map spells the letter; what the letter means is here.
    pub fn affirmative(self) -> Option<Action> {
        match self {
            Answers::Destructive => Some(Action::Delete),
            Answers::Conflict => Some(Action::Overwrite),
            Answers::Acknowledge => None,
        }
    }
}

/// The words a dialog gives its own answers, one per answer its set admits.
///
/// Invariant: the answer set decides which keys are bound and the dialog decides
/// what they are called, because two dialogs share a key and mean different
/// things by it — the letter that removes a label is the letter that throws a
/// buffer away. Neither half names the other's business: a word here never spells
/// a key, and the key map never invents the prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnswerWords {
    /// What going ahead does, and `None` for a dialog that only reports: such a
    /// dialog has a way out and no answer, so there is nothing to word.
    pub affirmative: Option<&'static str>,
    /// What getting out does. Every dialog has a way out, so this is never
    /// absent.
    pub dismissal: &'static str,
}

/// An action editing mode offers on the row it froze, as opposed to the keys the
/// mode itself answers — the way out, help and a reload.
///
/// Invariant: only actions the browser believes it can perform on the frozen row
/// are ever listed, and the row's own offer is what both lists a hint and answers
/// the letter — so the hint strip can never name a letter the row ignores, nor
/// hide one it answers. A new editing action is a variant here, which the
/// compiler then demands an offer and an answer for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditingAction {
    /// Add a member to the container the frozen row names.
    Add,
    /// Delete what the frozen row names, behind a confirmation.
    Delete,
    /// Edit the long-form text of what the frozen row names: an epic's or a
    /// node's body.
    Body,
}

impl EditingAction {
    /// Every editing action, so a surface covering all of them cannot miss one.
    /// Its length is pinned by a test, because a variant left out here would be
    /// an action with no hint and no key.
    pub const ALL: &'static [EditingAction] = &[
        EditingAction::Add,
        EditingAction::Delete,
        EditingAction::Body,
    ];

    /// The intent a key carries for this action. The state machine reads an
    /// intent, so the two vocabularies meet here and nowhere else.
    pub fn intent(self) -> Action {
        match self {
            EditingAction::Add => Action::Add,
            EditingAction::Delete => Action::Delete,
            EditingAction::Body => Action::Body,
        }
    }

    /// The editing action an intent asks for, or `None` for an intent that is not
    /// one. Derived from [`EditingAction::intent`], so a key and an offer can
    /// never disagree about which action they mean.
    pub fn for_intent(intent: Action) -> Option<Self> {
        Self::ALL.iter().copied().find(|a| a.intent() == intent)
    }
}

/// A resolved user intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Move the navigation cursor down one row.
    CursorDown,
    /// Move the navigation cursor up one row.
    CursorUp,
    /// Move the navigation cursor to the first row.
    CursorFirst,
    /// Move the navigation cursor to the last row.
    CursorLast,
    /// Enter the highlighted row's children.
    Descend,
    /// Leave the current level for its parent.
    Ascend,
    /// Back out of the innermost thing the reader is inside: an open overlay,
    /// then editing mode, then the level.
    ///
    /// Distinct from [`Action::Ascend`] because editing mode has to tell the way
    /// out from a level key: while the mode is on the level cannot change, and
    /// the one key that unwinds must not be one of several that look like it.
    Unwind,
    /// Scroll the preview down half a screen.
    PreviewHalfDown,
    /// Scroll the preview up half a screen.
    PreviewHalfUp,
    /// Scroll the preview down a screen.
    PreviewPageDown,
    /// Scroll the preview up a screen.
    PreviewPageUp,
    /// Scroll the preview to the start.
    PreviewTop,
    /// Scroll the preview to the end.
    PreviewBottom,
    /// Give the navigation pane less width.
    ShrinkNav,
    /// Give the navigation pane more width.
    GrowNav,
    /// Restore the default split.
    ResetSplit,
    /// Toggle the preview filling the whole width.
    ToggleZoom,
    /// Re-read the store.
    Reload,
    /// Enter editing mode on the highlighted row.
    EnterEditing,
    /// Add a member to the container editing mode is acting on, which opens a
    /// surface to fill in.
    Add,
    /// Delete the row editing mode is acting on, which asks first. On the dialog
    /// that asks, the same intent is the answer that goes ahead: one letter
    /// answers everything destructive, so it is learned once — a label removed, a
    /// buffer thrown away.
    Delete,
    /// Edit the body of what editing mode is acting on, which opens a buffer on
    /// the text as the store holds it now.
    Body,
    /// Write anyway, over a change that landed under the open buffer. The
    /// affirmative answer of the one question whose two answers both lose
    /// something, which is why it is not the destructive letter: what this throws
    /// away is somebody else's text rather than the reader's own.
    Overwrite,
    /// Put a character into the open field, where the cursor is.
    ///
    /// A line break is a character like any other in a field that holds many
    /// lines, and is dropped by a field that holds one — the field enforces that,
    /// so no route into a one-line value can put a break in it.
    Insert(char),
    /// Delete the character before the cursor.
    DeleteBefore,
    /// Delete the character under the cursor.
    DeleteAfter,
    /// Move the field cursor one character left.
    MoveLeft,
    /// Move the field cursor one character right.
    MoveRight,
    /// Move the field cursor to the start of the line it is on, which in a field
    /// holding one line is the start of its content.
    MoveToStart,
    /// Move the field cursor to the end of the line it is on, which in a field
    /// holding one line is the end of its content.
    MoveToEnd,
    /// Move the field cursor to the same column of the line above, or leave it
    /// where it is when there is no line above — a field holding one line
    /// included.
    MoveUp,
    /// Move the field cursor to the same column of the line below, or leave it
    /// where it is when there is no line below.
    MoveDown,
    /// Put the keyboard in the next field of the open surface.
    NextField,
    /// Put the keyboard in the previous field of the open surface.
    PreviousField,
    /// Accept the open surface: write what its fields hold.
    Accept,
    /// Hand the open field's content to the external editor and take the result
    /// back.
    ExternalEditor,
    /// Toggle the key-binding overlay.
    ToggleHelp,
    /// Leave the browser.
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_editing_action_every_answer_set_and_every_field_shape_is_listed() {
        // The lists are what the hint strip and the key map walk, so a variant
        // left out of one is an action with no hint, or answers no dialog could
        // list. The exhaustive matches make a new variant a compile error here,
        // and the counts make leaving it out of the list a failure here.
        for action in EditingAction::ALL {
            match action {
                EditingAction::Add | EditingAction::Delete | EditingAction::Body => {}
            }
        }
        assert_eq!(EditingAction::ALL.len(), 3);
        for answers in Answers::ALL {
            match answers {
                Answers::Destructive | Answers::Conflict | Answers::Acknowledge => {}
            }
        }
        assert_eq!(Answers::ALL.len(), 3);
        for fields in Fields::ALL {
            match fields {
                Fields::One | Fields::Several => {}
            }
        }
        assert_eq!(Fields::ALL.len(), 2);
        for lines in Lines::ALL {
            match lines {
                Lines::One | Lines::Many => {}
            }
        }
        assert_eq!(Lines::ALL.len(), 2);
    }

    #[test]
    fn every_field_count_against_every_field_kind_is_a_shape_a_surface_may_have() {
        // Derived from the two axes rather than counted, so an axis that gains a
        // case fails here instead of leaving a combination the key map is never
        // asked about and no surface test ever walks.
        let combinations: Vec<Shape> = Fields::ALL
            .iter()
            .copied()
            .flat_map(|fields| {
                Lines::ALL
                    .iter()
                    .copied()
                    .map(move |lines| Shape { fields, lines })
            })
            .collect();
        assert_eq!(Shape::ALL.len(), combinations.len());
        for shape in combinations {
            assert!(Shape::ALL.contains(&shape), "{shape:?} is not listed");
        }
    }

    #[test]
    fn a_surface_holds_one_field_or_several_and_nothing_counts_as_one() {
        // The distinction is the whole of what the key map is told, so it is drawn
        // at exactly one place: a second field is where a surface starts being
        // navigated. A surface with nothing to fill in is not a surface, so a count
        // of none reads as the one-field shape rather than as a third case.
        assert_eq!(Fields::of(1), Fields::One);
        assert_eq!(Fields::of(0), Fields::One);
        for count in 2..8 {
            assert_eq!(Fields::of(count), Fields::Several, "{count} fields");
        }
    }

    #[test]
    fn an_editing_action_and_the_intent_that_asks_for_it_are_one_mapping() {
        // Derived one from the other, so a key that carries an intent and a row
        // that offers the action cannot mean two different things by it.
        for action in EditingAction::ALL {
            assert_eq!(EditingAction::for_intent(action.intent()), Some(*action));
        }
        // And an intent that is not an editing action asks for none: the mode's
        // own keys are answered by the mode, not offered by a row, and a key that
        // belongs to an open field is answered by the field.
        assert_eq!(EditingAction::for_intent(Action::Unwind), None);
        assert_eq!(EditingAction::for_intent(Action::Quit), None);
        assert_eq!(EditingAction::for_intent(Action::Accept), None);
        assert_eq!(EditingAction::for_intent(Action::Insert('a')), None);
        assert_eq!(EditingAction::for_intent(Action::NextField), None);
        assert_eq!(EditingAction::for_intent(Action::PreviousField), None);
        assert_eq!(EditingAction::for_intent(Action::MoveUp), None);
        assert_eq!(EditingAction::for_intent(Action::MoveDown), None);
        assert_eq!(EditingAction::for_intent(Action::Overwrite), None);
    }

    #[test]
    fn no_two_answer_sets_are_answered_by_the_same_intent() {
        // The set is what decides whether an intent answers the open dialog, so two
        // sets sharing an affirmative intent would let one dialog's answer perform
        // the other's — a reader answering a question about their own text would be
        // throwing away somebody else's.
        let affirmative: Vec<Action> = Answers::ALL
            .iter()
            .copied()
            .filter_map(Answers::affirmative)
            .collect();
        for (index, intent) in affirmative.iter().enumerate() {
            assert!(
                !affirmative[index + 1..].contains(intent),
                "{intent:?} answers two sets"
            );
        }
        // And a set that only reports is answered by nothing at all: there is
        // nothing to go ahead with, so no intent may go ahead with it.
        assert_eq!(Answers::Acknowledge.affirmative(), None);
        assert!(!affirmative.is_empty(), "no set can be answered at all");
    }
}
