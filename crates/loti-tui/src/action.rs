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
    Surface,
    /// A dialog is open, admitting the set of answers it lists and nothing else.
    ///
    /// The set travels with the dialog rather than being a mode of its own, so a
    /// new kind of dialog is a set of answers here and never another mode.
    Dialog(Answers),
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
    /// Nothing is at stake — the dialog reports rather than asks — so it is
    /// dismissed rather than answered, and either dismissing key does it.
    Acknowledge,
}

impl Answers {
    /// Every set a dialog may ask for. A surface that has to cover them all —
    /// the letters, and the answers each set lists — cannot then miss one.
    pub const ALL: &'static [Answers] = &[Answers::Destructive, Answers::Acknowledge];
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
}

impl EditingAction {
    /// Every editing action, so a surface covering all of them cannot miss one.
    /// Its length is pinned by a test, because a variant left out here would be
    /// an action with no hint and no key.
    pub const ALL: &'static [EditingAction] = &[EditingAction::Add, EditingAction::Delete];

    /// The intent a key carries for this action. The state machine reads an
    /// intent, so the two vocabularies meet here and nowhere else.
    pub fn intent(self) -> Action {
        match self {
            EditingAction::Add => Action::Add,
            EditingAction::Delete => Action::Delete,
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
    /// Put a character into the open field, where the cursor is.
    Insert(char),
    /// Delete the character before the cursor.
    DeleteBefore,
    /// Delete the character under the cursor.
    DeleteAfter,
    /// Move the field cursor one character left.
    MoveLeft,
    /// Move the field cursor one character right.
    MoveRight,
    /// Move the field cursor to the start of its content.
    MoveToStart,
    /// Move the field cursor to the end of its content.
    MoveToEnd,
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
    fn every_editing_action_and_every_answer_set_is_listed() {
        // The lists are what the hint strip and the key map walk, so a variant
        // left out of one is an action with no hint, or answers no dialog could
        // list. The exhaustive matches make a new variant a compile error here,
        // and the counts make leaving it out of the list a failure here.
        for action in EditingAction::ALL {
            match action {
                EditingAction::Add | EditingAction::Delete => {}
            }
        }
        assert_eq!(EditingAction::ALL.len(), 2);
        for answers in Answers::ALL {
            match answers {
                Answers::Destructive | Answers::Acknowledge => {}
            }
        }
        assert_eq!(Answers::ALL.len(), 2);
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
    }
}
