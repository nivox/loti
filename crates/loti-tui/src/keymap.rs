//! Keys to intents — the only place in the crate that names a key.
//!
//! The two panes never compete for a key, so there is no focus to switch: the
//! navigation cursor owns the plain motion keys and the preview owns the paging
//! keys. `Esc` unwinds one layer of where the reader is standing rather than
//! leaving the application, so a mis-hit can never discard the session; `q` is
//! the way out of the browser, and it is not a key any mode borrows.
//!
//! One key may carry different intents in different modes, but never two intents
//! in the same mode: a binding maps to one intent here, and the state machine
//! decides what that intent means where the reader is standing.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::{Action, AnswerWords, Answers, EditingAction, FieldKind, Fields, Mode, Shape};
use crate::data::FreeForm;

/// The intent a key press carries in a mode, or `None` if it is not bound there.
pub fn action_for(key: KeyEvent, mode: Mode) -> Option<Action> {
    // A dialog answers for itself: only the answers it lists are bound while it
    // is open, so nothing underneath it can be moved, reloaded or quit by a key
    // pressed at a question.
    if let Mode::Dialog(answers) = mode {
        return dialog_action(key, answers);
    }
    // An open surface takes the whole keyboard, so nothing below is consulted:
    // a letter typed into a field is a character, never the action that letter
    // carries one layer up.
    if let Mode::Surface(shape) = mode {
        return surface_action(key, shape);
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match (key.code, ctrl) {
        // Quitting. `q` is the way out of the browser and no mode borrows it.
        (KeyCode::Char('q'), false) => Action::Quit,

        // Raw mode delivers Ctrl-C as a key press rather than a signal, so it
        // has to be bound explicitly to be honoured at all. Inside editing mode
        // it is the same key as `Esc` — the conventional way to abandon what you
        // are in — so it ends the session while browsing only, which is the safe
        // direction for a key a habit presses.
        (KeyCode::Char('c'), true) => match mode {
            Mode::Browse => Action::Quit,
            // A dialog's answers and a surface's keys are settled before this
            // table is reached, so only browsing and editing arrive here.
            Mode::Editing | Mode::Surface(_) | Mode::Dialog(_) => Action::Unwind,
        },

        // Preview paging. Bound before the plain motions so Ctrl-D/Ctrl-U are
        // not shadowed by any character binding.
        (KeyCode::Char('d'), true) => Action::PreviewHalfDown,
        (KeyCode::Char('u'), true) => Action::PreviewHalfUp,
        (KeyCode::PageDown, _) | (KeyCode::Char(' '), false) => Action::PreviewPageDown,
        (KeyCode::PageUp, _) => Action::PreviewPageUp,
        (KeyCode::Home, _) => Action::PreviewTop,
        (KeyCode::End, _) => Action::PreviewBottom,

        // Navigation motions.
        (KeyCode::Char('j'), false) | (KeyCode::Down, false) => Action::CursorDown,
        (KeyCode::Char('k'), false) | (KeyCode::Up, false) => Action::CursorUp,
        (KeyCode::Char('g'), false) => Action::CursorFirst,
        (KeyCode::Char('G'), false) => Action::CursorLast,
        (KeyCode::Enter, _) | (KeyCode::Char('l'), false) | (KeyCode::Right, false) => {
            Action::Descend
        }
        // `Esc` unwinds one layer at a time, which is not what a level key does:
        // while editing mode is on the level cannot change, so the level keys do
        // nothing there and `Esc` alone is still the way out.
        (KeyCode::Esc, _) => Action::Unwind,
        (KeyCode::Backspace, _) | (KeyCode::Char('h'), false) | (KeyCode::Left, false) => {
            Action::Ascend
        }

        // Layout.
        (KeyCode::Char('<'), false) => Action::ShrinkNav,
        (KeyCode::Char('>'), false) => Action::GrowNav,
        (KeyCode::Char('='), false) => Action::ResetSplit,
        (KeyCode::Char('z'), false) => Action::ToggleZoom,

        // The letters of editing mode's action-selection layer, bound inside the
        // mode only: browse mode is where a reader's fingers rest, and a letter
        // that removed something from there would be one stray keystroke away
        // from a write.
        (KeyCode::Char('a'), false) if matches!(mode, Mode::Editing) => Action::Add,
        (KeyCode::Char('d'), false) if matches!(mode, Mode::Editing) => Action::Delete,
        // A letter names the kind of thing being edited: `b` the long-form text, `n`
        // the name, `S` the summary. The summary takes the shifted key because the
        // unshifted one belongs to the state a reader changes far more often, at the
        // cost of two unrelated nouns sharing a letter.
        //
        // `b` is the long-form text *of the row it is pressed on* — a body on an
        // epic or a node, a comment's text on a comment — so a comment needs no
        // letter of its own and one key still carries one intent. Which field the
        // intent reaches is the row's answer, decided where a row's offers are.
        (KeyCode::Char('b'), false) if matches!(mode, Mode::Editing) => {
            Action::Edit(FreeForm::Body)
        }
        (KeyCode::Char('n'), false) if matches!(mode, Mode::Editing) => {
            Action::Edit(FreeForm::Name)
        }
        (KeyCode::Char('S'), false) if matches!(mode, Mode::Editing) => {
            Action::Edit(FreeForm::Summary)
        }
        // The unshifted letter belongs to the state because the state is the action
        // a reader takes far more often than any other; the summary takes the shift.
        (KeyCode::Char('s'), false) if matches!(mode, Mode::Editing) => Action::SetState,
        // The one shift-pair whose halves are the same noun: the letter takes the
        // claim and the shifted letter gives it up. Giving up is the shifted half
        // because it is offered only while a claim is held, so it is the rarer of
        // the two.
        (KeyCode::Char('c'), false) if matches!(mode, Mode::Editing) => Action::TakeClaim,
        (KeyCode::Char('C'), false) if matches!(mode, Mode::Editing) => Action::ReleaseClaim,

        // Session. `F1` is a help key beside `?` everywhere, because inside a
        // field `?` is a literal character and a reader must not have to learn a
        // second help key on arriving there.
        (KeyCode::Char('e'), false) => Action::EnterEditing,
        // The one write key that is not a letter a row offers: an epic has no
        // container row to be added to, so creating one is the browser's own key
        // rather than an action inside the mode. Shifted, because the unshifted
        // letter is a motion a reader's fingers rest on and this one writes — the
        // same reason no editing letter is bound while browsing.
        (KeyCode::Char('N'), false) => Action::CreateEpic,
        (KeyCode::Char('r'), false) => Action::Reload,
        (KeyCode::Char('?'), false) | (KeyCode::F(1), _) => Action::ToggleHelp,

        _ => return None,
    })
}

/// The intent a key carries inside an open surface, where every key belongs to
/// the field.
///
/// Nothing browse mode binds survives: the paging keys that scroll a preview are
/// the field's while it is open, and a letter is a character rather than the
/// action that letter carries one layer up. A key this table does not bind is
/// ignored rather than handed to the level underneath.
///
/// The surface's shape is an input rather than something read off a key, because
/// several keys mean different things by it: how many fields there are decides
/// whether the field-motion keys have anywhere to go, and what kind of field the
/// keyboard is in decides whether a line break is content, what the vertical keys
/// move, and whether there is any text for the external editor to take. None of
/// them decides how a surface is accepted — that is one key everywhere.
fn surface_action(key: KeyEvent, shape: Shape) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match (key.code, ctrl) {
        // The save key accepts every surface, whatever kind of field has focus and
        // however many fields there are: the way to finish is one key, learned once.
        // Raw mode is already on, which clears `IXON`, so Ctrl-S arrives as a key
        // press instead of being eaten as flow control.
        (KeyCode::Char('s'), true) => Action::Accept,
        // The reflex key is a line break, which is content in a field that holds
        // many lines and in no other, so anywhere else it is unbound: it neither
        // accepts nor moves, and a reader learns one meaning for it instead of a
        // rule with cases. Ignoring it says nothing, deliberately — the hint strip
        // already carries the save key, and a notice covers the whole strip for as
        // long as it is up, so teaching the save key would hide the hint that
        // teaches it along with the field and editor hints beside it.
        (KeyCode::Enter, _) if matches!(shape.kind, FieldKind::ManyLines) => Action::Insert('\n'),
        // Field motion, and the only keys that move between fields. Bound where
        // there is another field to reach and nowhere else: a key that did nothing
        // on a one-field surface would be a key taught for nothing.
        (KeyCode::Tab, _) if matches!(shape.fields, Fields::Several) => Action::NextField,
        (KeyCode::BackTab, _) if matches!(shape.fields, Fields::Several) => Action::PreviousField,
        // Handing the field over is for a field made of text: a picker holds none,
        // so there is nothing to hand over and nothing an editor could hand back.
        // Unbound there rather than ignored, so the strip and the key are settled by
        // one answer.
        (KeyCode::Char('g'), true)
            if matches!(shape.kind, FieldKind::OneLine | FieldKind::ManyLines) =>
        {
            Action::ExternalEditor
        }
        (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => Action::Unwind,
        // The one help key that can be pressed inside a field.
        (KeyCode::F(1), _) => Action::ToggleHelp,

        // The emacs motions, with the arrows and the line keys beside them. These
        // are a line's start and end, which in a field holding one line is the whole
        // of it, so the keys that page a preview while browsing move within the
        // field here.
        (KeyCode::Char('a'), true) | (KeyCode::Home, _) => Action::MoveToStart,
        (KeyCode::Char('e'), true) | (KeyCode::End, _) => Action::MoveToEnd,
        (KeyCode::Char('b'), true) | (KeyCode::Left, _) => Action::MoveLeft,
        (KeyCode::Char('f'), true) | (KeyCode::Right, _) => Action::MoveRight,
        // Vertical motion, bound where there is something above or below to reach:
        // another line of text, or the picker's next value. One meaning — up and
        // down — and the field the keyboard is in says what is up and down in it.
        // In a field holding one line there is neither, so the keys are unbound
        // rather than landing where they started.
        (KeyCode::Up, _) | (KeyCode::Char('p'), true)
            if matches!(shape.kind, FieldKind::ManyLines | FieldKind::Pick) =>
        {
            Action::MoveUp
        }
        (KeyCode::Down, _) | (KeyCode::Char('n'), true)
            if matches!(shape.kind, FieldKind::ManyLines | FieldKind::Pick) =>
        {
            Action::MoveDown
        }

        // Ctrl-H is unavailable as a binding of its own: terminals send 0x08 for
        // Backspace, so it cannot be told apart from the key that deletes a
        // character.
        (KeyCode::Backspace, _) => Action::DeleteBefore,
        (KeyCode::Delete, _) => Action::DeleteAfter,
        (KeyCode::Char(c), false) => Action::Insert(c),

        _ => return None,
    })
}

/// The letter one set of dialog answers goes ahead by, and `None` for a set that
/// only reports — such a dialog has nothing to go ahead with, so it has no letter
/// for doing so.
///
/// `d` answers anything destructive, learned once. Overwriting a change that
/// landed under the reader's buffer takes a letter of its own, because it is the
/// one question where going ahead destroys somebody else's text rather than the
/// reader's, and one letter for both would make the habit built on deletions
/// answer a question about somebody else's work.
fn affirmative_key(answers: Answers) -> Option<char> {
    match answers {
        Answers::Destructive => Some('d'),
        Answers::Conflict => Some('o'),
        Answers::Acknowledge => None,
    }
}

/// The intent a key carries at one set of dialog answers, which are exactly the
/// answers that set lists.
///
/// The affirmative letter is this map's and what it means is the set's, so a key
/// pressed at one question can never perform another's answer. `Esc` is the one
/// answer that is never destructive, here as everywhere else, and on a question
/// `Enter` is bound to nothing at all: a reader arrives at one in a hurry, so a
/// reflex press must not be the thing that destroys something. Where nothing is at
/// stake `Enter` dismisses alongside `Esc`, because a second key is then kindness.
fn dialog_action(key: KeyEvent, answers: Answers) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if let (KeyCode::Char(c), false) = (key.code, ctrl) {
        if affirmative_key(answers) == Some(c) {
            return answers.affirmative();
        }
    }
    Some(match (key.code, ctrl) {
        (KeyCode::Enter, _) if matches!(answers, Answers::Acknowledge) => Action::Unwind,
        (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => Action::Unwind,
        _ => return None,
    })
}

/// The answers a dialog lists, in the order it lists them: the keys of the set it
/// asked for, each carrying the word that dialog gave it. A dialog says how to
/// answer it, so the way out of one never depends on the hint strip a notice or a
/// narrow terminal may have taken.
///
/// Two halves, deliberately: the set decides which keys are bound and listed, and
/// the dialog decides what they are called — the destructive letter removes a
/// label on one dialog and throws a buffer away on another, and one set of
/// hardwired words could not say both. Every answer still leads with the key that
/// gives it, and that key is bound in the same set: a dialog naming a letter it
/// does not answer would seal the reader in.
pub fn dialog_answers(answers: Answers, words: AnswerWords) -> Vec<String> {
    let dismissal = format!("Esc {}", words.dismissal);
    match affirmative_key(answers) {
        Some(letter) => words
            .affirmative
            .into_iter()
            .map(|word| format!("{letter} {word}"))
            .chain(std::iter::once(dismissal))
            .collect(),
        // Nothing is at stake, so both dismissing keys are listed on the one
        // answer they share.
        None => vec![format!("Esc / Enter {}", words.dismissal)],
    }
}

/// The bindings as the help overlay and the footer present them: one row per
/// group, so both surfaces describe the same keymap without restating it.
pub const HELP: &[(&str, &str)] = &[
    (
        "j / k / ↓ / ↑",
        "move the cursor; ↑ / ↓ by line, or through a picker",
    ),
    ("g / G", "first / last row"),
    // Enter's two meanings, and it has no third: it is a line break where breaks
    // are content, and nothing at all in a field that holds one line.
    ("Enter / l / →", "open the row; a line break in a text area"),
    ("Backspace / h / ←", "leave the level"),
    ("Esc", "leave the level, a field, or editing mode"),
    // Both ways of scrolling the preview on one row, because the list is as tall
    // as the shortest terminal the browser supports and a row past that is
    // clipped without saying so: the keys that page it and the keys that page half
    // of it are one group, read together.
    (
        "PgDn / PgUp / Space",
        "scroll the preview a screen; Ctrl-D / Ctrl-U half",
    ),
    (
        "Home / End",
        "preview start / end; a field line's ends inside one",
    ),
    ("< / > / =", "narrow / widen / reset the panes"),
    ("z", "preview fills the width; mouse released"),
    // Two keys on one row, for the same reason the groups below share theirs: the
    // list is as tall as the shortest terminal the browser supports and a row past
    // that is clipped without saying so. These two are the ways into a write from
    // browsing, and neither is answered on a store that may not be written.
    ("e / N", "editing mode on the row / a new epic, from epics"),
    ("a / d", "editing mode: add a member / remove it, confirmed"),
    // Three keys on one row, because the list is as tall as the shortest terminal
    // the browser supports and a row past that is clipped without saying so: the
    // three replace a whole field between them and are read as one group. The last
    // of them is named for what it edits on any row rather than for a body, because
    // on a comment it is the comment's own text.
    (
        "n / S / b",
        "editing mode: the name / the summary / the text",
    ),
    // Three keys on one row, for the same reason the fields above share one: the
    // list is as tall as the shortest terminal the browser supports and a row past
    // that is clipped without saying so. These three set what a row *is* rather than
    // what it says, so they read as one group.
    (
        "s / c / C",
        "editing mode: the state / take the claim / release it",
    ),
    ("Ctrl-S", "save, whichever field you are in"),
    ("Tab / Shift-Tab", "next / previous field"),
    ("Ctrl-G", "edit the open field in $EDITOR"),
    (
        "Ctrl-A / Ctrl-E",
        "line start / end; Ctrl-B / Ctrl-F or ← / → by one",
    ),
    ("r", "re-read the store"),
    ("? / F1", "these keys; F1 works inside a field"),
    ("q", "quit"),
];

/// The hints the strip under the panes is built from, in the order a reader
/// wants them. A narrow terminal drops trailing hints rather than clipping one
/// mid-word, so each entry has to stand alone.
pub const FOOTER_HINTS: &[&str] = &[
    "j/k move",
    "Enter open",
    "Esc back",
    "Ctrl-D/U scroll",
    "</> resize",
    "z zoom",
    "r reload",
];

/// The pair the strip always appends, in rank order: how to get out of where you
/// are, then how to get help.
///
/// The way out comes first because a width too narrow for the pair clips the
/// tail: a reader who cannot see how to get help can still leave, widen the
/// terminal or read the docs, while a reader who cannot see how to leave is stuck
/// inside the application.
pub const FOOTER_ESSENTIAL: &[&str] = &["q quit", "? keys"];

/// The droppable hints of editing mode: one per editing action, in this map's
/// own order rather than reordered per row, so a letter keeps its relative place
/// from row to row instead of moving with what a row happens to offer.
///
/// Each hint travels with the action it names, because the strip lists the subset
/// the frozen row offers and only the state machine knows which that is.
pub const FOOTER_HINTS_EDITING: &[(EditingAction, &str)] = &[
    (EditingAction::Add, "a add"),
    (EditingAction::Delete, "d remove"),
    (EditingAction::Edit(FreeForm::Name), "n name"),
    (EditingAction::Edit(FreeForm::Summary), "S summary"),
    // Named for the long-form text rather than for a body: the same letter edits a
    // comment's text on a comment row, and the strip must not name a field the row
    // it is drawn beside has not got.
    (EditingAction::Edit(FreeForm::Body), "b text"),
    (EditingAction::SetState, "s state"),
    (EditingAction::TakeClaim, "c claim"),
    (EditingAction::ReleaseClaim, "C release"),
];

/// The droppable hints of an open surface, ranked rather than in key order: they
/// are not peers. Saving is what the reader came to do, then moving between the
/// fields, and handing a field to an external editor is the power-user escape
/// from a cramped one.
///
/// A list of its own rather than more entries beside the editing actions: those
/// are letters a row offers and this row offers none of them — while a surface is
/// open the only keys that apply are the surface's.
///
/// Each hint is listed only where the surface answers the key it names, because the
/// strip must never name a key the surface ignores: the field-navigation hint wants
/// another field to reach, and the external editor wants a field made of text — a
/// picker has none to hand over.
pub fn footer_hints_surface(shape: Shape) -> Vec<&'static str> {
    let mut hints = vec!["Ctrl-S save"];
    if matches!(shape.fields, Fields::Several) {
        hints.push("Tab fields");
    }
    if matches!(shape.kind, FieldKind::OneLine | FieldKind::ManyLines) {
        hints.push("Ctrl-G editor");
    }
    hints
}

/// Editing mode's essential pair. Neither browse hint applies — `q` does not quit
/// while the mode is on, and the way out of the mode is not the way out of a
/// level — so without a pair of its own a narrow terminal would show a mode with
/// no visible way out of it.
pub const FOOTER_ESSENTIAL_EDITING: &[&str] = &["Esc leave", "? keys"];

/// An open surface's essential pair. `F1` rather than `?`, because inside a field
/// `?` is a literal character — and rather than `Ctrl-S`, because the key list
/// `F1` opens *contains* `Ctrl-S`, so help is the one hint that substitutes for
/// every other and a reader is never sealed inside an uncommittable buffer.
pub const FOOTER_ESSENTIAL_SURFACE: &[&str] = &["Esc cancel", "F1 keys"];

/// The separator between hints.
pub const HINT_SEPARATOR: &str = " · ";

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// Every mode a dialog puts the keyboard under, one per set of answers.
    fn dialog_modes() -> Vec<Mode> {
        Answers::ALL.iter().copied().map(Mode::Dialog).collect()
    }

    /// Every mode an open surface puts the keyboard under, one per shape: a key
    /// that belongs to the field belongs to it on every shape, so a test about the
    /// field walks them all.
    fn surface_modes() -> Vec<Mode> {
        Shape::ALL.iter().copied().map(Mode::Surface).collect()
    }

    /// A surface holding this many fields, whose focused field is one line of text.
    fn one_line(fields: Fields) -> Mode {
        Mode::Surface(Shape {
            fields,
            kind: FieldKind::OneLine,
        })
    }

    /// Every mode a surface whose focused field is of this kind puts the keyboard
    /// under, one per field count: how many fields there are must not change what a
    /// key means inside the field the keyboard is in.
    fn focused_on(kind: FieldKind) -> Vec<Mode> {
        Fields::ALL
            .iter()
            .copied()
            .map(|fields| Mode::Surface(Shape { fields, kind }))
            .collect()
    }

    /// A surface whose focused field holds many lines, at each field count.
    fn text_areas() -> Vec<Mode> {
        focused_on(FieldKind::ManyLines)
    }

    /// A surface whose focused field is a picker, at each field count.
    fn pickers() -> Vec<Mode> {
        focused_on(FieldKind::Pick)
    }

    /// Every mode a surface whose focused field is made of text puts the keyboard
    /// under: the kinds an external editor can be handed, and the kinds a character
    /// is content in.
    fn text_fields() -> Vec<Mode> {
        text_areas().into_iter().chain(one_lines()).collect()
    }

    /// A surface whose focused field is one line of text, at each field count.
    fn one_lines() -> Vec<Mode> {
        focused_on(FieldKind::OneLine)
    }

    /// Every key press a terminal can deliver that this crate could plausibly
    /// bind: each named code and each printable ASCII character, plain and with
    /// the control modifier. A claim that *one* key does something is only worth
    /// as much as the sweep proving no other does, and the keys that must not is
    /// the list nobody thinks to write down.
    fn every_key() -> Vec<KeyEvent> {
        let codes = [
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Esc,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
        ];
        let mut keys: Vec<KeyEvent> = Vec::new();
        for code in codes.into_iter().chain((1..=12).map(KeyCode::F)) {
            keys.push(plain(code));
            keys.push(KeyEvent::new(code, KeyModifiers::CONTROL));
            keys.push(KeyEvent::new(code, KeyModifiers::SHIFT));
        }
        for c in ' '..='~' {
            keys.push(plain(KeyCode::Char(c)));
            keys.push(ctrl(c));
        }
        keys
    }

    /// Words for a dialog's answers, so a test about the letters is not also a
    /// test about anybody's prose.
    fn words() -> AnswerWords {
        AnswerWords {
            affirmative: Some("go ahead"),
            dismissal: "get out",
        }
    }

    /// The key a hint or an answer leads with, read back from the way it is
    /// spelled. A strip teaches keys by name, so a test that the name is bound has
    /// to parse the name the reader sees.
    fn key_named(spelling: &str) -> KeyEvent {
        match spelling {
            "Esc" => plain(KeyCode::Esc),
            "Enter" => plain(KeyCode::Enter),
            "F1" => plain(KeyCode::F(1)),
            "Tab" => plain(KeyCode::Tab),
            // Terminals send shifted Tab as a code of its own, so the name a strip
            // shows and the key a reader presses meet here.
            "Shift-Tab" => KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            _ => match spelling.strip_prefix("Ctrl-") {
                Some(rest) => ctrl(one_char(rest).to_ascii_lowercase()),
                None => plain(KeyCode::Char(one_char(spelling))),
            },
        }
    }

    fn one_char(spelling: &str) -> char {
        let mut chars = spelling.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => c,
            _ => panic!("{spelling:?} is not a key this test can read back"),
        }
    }

    /// The key an answer or a hint leads with.
    fn leading(text: &str) -> &str {
        text.split_whitespace().next().expect("a leading key")
    }

    #[test]
    fn escape_carries_its_own_intent_and_never_the_way_out_of_the_application() {
        // What that intent does with a level is the state machine's, and is
        // pinned there; here it is only that the two are not the same binding.
        for mode in [Mode::Browse, Mode::Editing] {
            assert_eq!(action_for(plain(KeyCode::Esc), mode), Some(Action::Unwind));
            assert_eq!(
                action_for(plain(KeyCode::Char('q')), mode),
                Some(Action::Quit)
            );
        }
    }

    #[test]
    fn the_way_out_is_not_one_of_the_level_keys() {
        // Editing mode leaves the level keys with nothing to do and keeps `Esc`
        // as its way out, so the two intents cannot share a binding.
        for code in [KeyCode::Backspace, KeyCode::Char('h'), KeyCode::Left] {
            assert_eq!(action_for(plain(code), Mode::Browse), Some(Action::Ascend));
        }
        assert_eq!(
            action_for(plain(KeyCode::Esc), Mode::Browse),
            Some(Action::Unwind)
        );
    }

    #[test]
    fn creating_an_epic_is_the_browsers_own_key_and_no_other_key_carries_it() {
        // An epic has no container row to be added from, so creating one is not a
        // letter a row offers: it is the browser's own key, bound wherever the
        // reader is not inside a field. Inside editing mode it carries the same
        // intent, which that mode does not admit — exactly as the key that enters
        // the mode does — so the mode answers it rather than the key going dead.
        for mode in [Mode::Browse, Mode::Editing] {
            let key = plain(KeyCode::Char('N'));
            assert_eq!(action_for(key, mode), Some(Action::CreateEpic), "{mode:?}");
            // And nothing else on the board carries it: a key that writes must not
            // be one stray keystroke away from a key that does not, and the keys
            // that must not carry it are the list nobody thinks to write down.
            for other in every_key().into_iter().filter(|other| *other != key) {
                assert_ne!(
                    action_for(other, mode),
                    Some(Action::CreateEpic),
                    "{other:?} creates an epic in {mode:?}"
                );
            }
        }
        // Inside a field it is a character like any other letter, so nothing typed
        // into a buffer can open another one.
        for mode in surface_modes() {
            assert_eq!(
                action_for(plain(KeyCode::Char('N')), mode),
                Some(Action::Insert('N')),
                "{mode:?}"
            );
        }
        // And no question answers it: a dialog admits the answers it lists and
        // nothing underneath it may write while one is open.
        for mode in dialog_modes() {
            assert_eq!(
                action_for(plain(KeyCode::Char('N')), mode),
                None,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn editing_mode_is_entered_by_its_own_key() {
        assert_eq!(
            action_for(plain(KeyCode::Char('e')), Mode::Browse),
            Some(Action::EnterEditing)
        );
    }

    #[test]
    fn every_essential_pair_says_the_way_out_before_it_says_help() {
        let pairs = [
            (Mode::Browse, FOOTER_ESSENTIAL),
            (Mode::Editing, FOOTER_ESSENTIAL_EDITING),
        ]
        .into_iter()
        // Both field shapes: the way out of a buffer is the same key whichever
        // shape it has, and a pair that named a key one shape ignores would seal
        // the reader into that one.
        .chain(
            surface_modes()
                .into_iter()
                .map(|mode| (mode, FOOTER_ESSENTIAL_SURFACE)),
        );
        for (mode, essential) in pairs {
            // Clipping eats the tail, so the rank is the order: the way out
            // first, help second. Both are checked by what their key does in that
            // mode rather than by how they are spelled — inside a field the help
            // key is not the one browse mode teaches, and a pair naming a key that
            // mode does not answer would seal the reader in.
            assert_eq!(essential.len(), 2);
            let out = action_for(key_named(leading(essential[0])), mode);
            assert!(
                matches!(out, Some(Action::Unwind) | Some(Action::Quit)),
                "{essential:?} in {mode:?} does not lead with a way out"
            );
            assert_eq!(
                action_for(key_named(leading(essential[1])), mode),
                Some(Action::ToggleHelp),
                "{essential:?} in {mode:?} does not offer help second"
            );
        }
        // And the way out of a mode is not the way out of the browser.
        assert_ne!(FOOTER_ESSENTIAL[0], FOOTER_ESSENTIAL_EDITING[0]);
        assert_ne!(FOOTER_ESSENTIAL[0], FOOTER_ESSENTIAL_SURFACE[0]);
    }

    #[test]
    fn an_open_surface_takes_every_key_and_lets_none_reach_what_is_under_it() {
        for mode in surface_modes() {
            // A letter is a character in a field, not the action it carries one
            // layer up: nothing typed can quit, move a cursor or open a level.
            for c in ['q', 'j', 'k', 'e', 'r', 'd', 'a', '?', 'z'] {
                assert_eq!(
                    action_for(plain(KeyCode::Char(c)), mode),
                    Some(Action::Insert(c)),
                    "{c:?} did not reach the field in {mode:?}"
                );
            }
            // The paging keys belong to the buffer while it is open, so they cannot
            // scroll the preview behind it — and a single-line field has no page, so
            // the ones with nothing to do there do nothing at all.
            for code in [KeyCode::PageDown, KeyCode::PageUp] {
                assert_eq!(action_for(plain(code), mode), None, "{code:?} in {mode:?}");
            }
            for c in ['d', 'u'] {
                assert_eq!(action_for(ctrl(c), mode), None, "Ctrl-{c} in {mode:?}");
            }
            // The field's own ends, from the keys that page a preview one layer up.
            assert_eq!(
                action_for(plain(KeyCode::Home), mode),
                Some(Action::MoveToStart),
                "{mode:?}"
            );
            assert_eq!(
                action_for(plain(KeyCode::End), mode),
                Some(Action::MoveToEnd),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_field_is_edited_by_the_keys_a_reader_already_knows() {
        // The same keys whatever shape the surface has: how many fields there are
        // decides which key moves between them, never what a field is edited with.
        for mode in surface_modes() {
            for (key, intent) in [
                (ctrl('a'), Action::MoveToStart),
                (ctrl('e'), Action::MoveToEnd),
                (ctrl('b'), Action::MoveLeft),
                (ctrl('f'), Action::MoveRight),
                (plain(KeyCode::Left), Action::MoveLeft),
                (plain(KeyCode::Right), Action::MoveRight),
                (plain(KeyCode::Backspace), Action::DeleteBefore),
                (plain(KeyCode::Delete), Action::DeleteAfter),
            ] {
                assert_eq!(action_for(key, mode), Some(intent), "{key:?} in {mode:?}");
            }
        }
    }

    #[test]
    fn the_keyboard_sweep_holds_the_keys_a_reader_would_reach_for() {
        // The sweeps are worth exactly what they cover, and one that had lost the
        // reflex key or the field keys would pass while saying nothing about the
        // keys this map used to give a second meaning to.
        let sweep = every_key();
        for key in [
            plain(KeyCode::Enter),
            key_named("Tab"),
            key_named("Shift-Tab"),
            ctrl('s'),
            plain(KeyCode::Esc),
        ] {
            assert!(sweep.contains(&key), "{key:?} is not swept");
        }
    }

    #[test]
    fn the_save_key_accepts_every_shape_of_surface_and_no_other_key_accepts_any() {
        // One key finishes a surface — one field or many, whatever kind of field the
        // keyboard is in — so a reader learns the way to finish once instead of a
        // rule with cases. The sweep is the point: the keys that must not accept
        // include the ones a reader would reach for, and naming them is exactly the
        // list an author forgets.
        for mode in surface_modes() {
            assert_eq!(
                action_for(ctrl('s'), mode),
                Some(Action::Accept),
                "{mode:?}"
            );
            for key in every_key().into_iter().filter(|key| *key != ctrl('s')) {
                assert_ne!(
                    action_for(key, mode),
                    Some(Action::Accept),
                    "{key:?} accepts in {mode:?}"
                );
            }
        }
    }

    #[test]
    fn a_surface_is_left_by_the_same_keys_on_every_shape_and_only_a_field_of_text_is_handed_over() {
        for mode in surface_modes() {
            // The way out, and its alias: inside a mode Ctrl-C is exactly Esc. One
            // key whatever the reader is standing in, picker included.
            for key in [plain(KeyCode::Esc), ctrl('c')] {
                assert_eq!(
                    action_for(key, mode),
                    Some(Action::Unwind),
                    "{key:?} in {mode:?}"
                );
            }
        }
        // The external editor reaches a field made of text, whichever kind and
        // however many fields there are.
        for mode in text_fields() {
            assert_eq!(
                action_for(ctrl('g'), mode),
                Some(Action::ExternalEditor),
                "{mode:?}"
            );
        }
        // And nowhere else: a picker holds no text, so there is nothing to hand an
        // editor and nothing it could hand back. Unbound rather than ignored, so the
        // strip that lists it and the key that answers it are settled by one answer.
        for mode in pickers() {
            assert_eq!(action_for(ctrl('g'), mode), None, "{mode:?}");
        }
    }

    #[test]
    fn the_vertical_keys_move_a_pickers_mark_and_nothing_else_in_it_does() {
        // A picker has no confirming key: the keys that move the mark are the keys
        // that change what a save would write, and they are the same keys that move
        // by line in a text area — one meaning, and the field says what is up and
        // down in it.
        for mode in pickers() {
            for (key, intent) in [
                (plain(KeyCode::Up), Action::MoveUp),
                (plain(KeyCode::Down), Action::MoveDown),
                (ctrl('p'), Action::MoveUp),
                (ctrl('n'), Action::MoveDown),
            ] {
                assert_eq!(action_for(key, mode), Some(intent), "{key:?} in {mode:?}");
            }
            // And no other key moves it: a value the reader did not mark must not be
            // what a save writes, and the sweep is the point — the keys that must not
            // move a mark are the list nobody thinks to write down.
            for key in every_key().into_iter().filter(|key| {
                !matches!(key.code, KeyCode::Up | KeyCode::Down)
                    && *key != ctrl('p')
                    && *key != ctrl('n')
            }) {
                assert!(
                    !matches!(
                        action_for(key, mode),
                        Some(Action::MoveUp) | Some(Action::MoveDown)
                    ),
                    "{key:?} moves the mark in {mode:?}"
                );
            }
            // The reflex key is not a way of confirming one either: a break is
            // content in a text area and nothing anywhere else, a picker included.
            assert_eq!(action_for(plain(KeyCode::Enter), mode), None, "{mode:?}");
        }
    }

    #[test]
    fn the_field_keys_move_between_fields_wherever_there_is_a_field_to_reach() {
        // Whatever kind of field the keyboard is in: the count decides whether there
        // is anywhere to go, and the kind never does — a form holding a text area is
        // navigated exactly like one that does not.
        for kind in FieldKind::ALL.iter().copied() {
            let several = Mode::Surface(Shape {
                fields: Fields::Several,
                kind,
            });
            assert_eq!(
                action_for(key_named("Tab"), several),
                Some(Action::NextField),
                "{kind:?}"
            );
            assert_eq!(
                action_for(key_named("Shift-Tab"), several),
                Some(Action::PreviousField),
                "{kind:?}"
            );
            // And no field to move to means no key that moves: a bound key that
            // could only land where it started is a key taught for nothing.
            let one = Mode::Surface(Shape {
                fields: Fields::One,
                kind,
            });
            for key in [key_named("Tab"), key_named("Shift-Tab")] {
                assert_eq!(action_for(key, one), None, "{key:?} in {one:?}");
            }
        }
        // The field keys are the surface's alone: while browsing they are not bound
        // at all, so nothing moves a field cursor that does not exist.
        for mode in [Mode::Browse, Mode::Editing] {
            for key in [key_named("Tab"), key_named("Shift-Tab")] {
                assert_eq!(action_for(key, mode), None, "{key:?} in {mode:?}");
            }
        }
        // And they are the only keys that move: no other key on the board reaches
        // another field, so a reader who has learned these two has learned all of it.
        for mode in surface_modes() {
            for key in every_key()
                .into_iter()
                .filter(|key| !matches!(key.code, KeyCode::Tab | KeyCode::BackTab))
            {
                assert!(
                    !matches!(
                        action_for(key, mode),
                        Some(Action::NextField) | Some(Action::PreviousField)
                    ),
                    "{key:?} moves between fields in {mode:?}"
                );
            }
        }
    }

    #[test]
    fn help_is_reachable_by_the_one_key_that_works_inside_a_field_too() {
        // `?` is a literal character in a field, so `F1` is the help key that
        // reaches every mode — and it is bound outside a field as well, so there
        // is no second help key to learn on arriving in one.
        for mode in [Mode::Browse, Mode::Editing]
            .into_iter()
            .chain(surface_modes())
        {
            assert_eq!(
                action_for(plain(KeyCode::F(1)), mode),
                Some(Action::ToggleHelp),
                "{mode:?}"
            );
        }
        assert_eq!(
            action_for(plain(KeyCode::Char('?')), Mode::Browse),
            Some(Action::ToggleHelp)
        );
        assert_eq!(
            action_for(plain(KeyCode::Char('?')), one_line(Fields::One)),
            Some(Action::Insert('?'))
        );
    }

    #[test]
    fn every_surface_hint_names_a_key_that_shape_of_surface_answers() {
        // The strip teaches keys by name, so a hint naming a key the surface does
        // not answer teaches a key that does nothing. These are not the editing
        // actions' hints: a row offers those, and while a surface is open no row
        // offers anything.
        for shape in Shape::ALL.iter().copied() {
            let hints = footer_hints_surface(shape);
            for hint in &hints {
                assert!(
                    action_for(key_named(leading(hint)), Mode::Surface(shape)).is_some(),
                    "{hint:?} names a key {shape:?} ignores"
                );
            }
            // And the field keys are hinted exactly where they are bound: a shape
            // with fields to move between says so, and the one without does not
            // teach a key it ignores.
            assert_eq!(
                hints.iter().any(|hint| leading(hint) == "Tab"),
                shape.fields == Fields::Several,
                "{shape:?}: {hints:?}"
            );
            // And so is the external editor: it takes a field made of text, so the
            // strip teaches it in one and stays quiet in a picker, which has no text
            // to hand over.
            assert_eq!(
                hints.iter().any(|hint| leading(hint) == "Ctrl-G"),
                shape.kind != FieldKind::Pick,
                "{shape:?}: {hints:?}"
            );
        }
    }

    #[test]
    fn the_reflex_key_is_a_line_break_where_breaks_are_content_and_is_ignored_elsewhere() {
        // One meaning, and the field's kind is the whole of what decides whether it
        // applies: a break is what a reader means by the key while writing prose.
        for mode in text_areas() {
            assert_eq!(
                action_for(plain(KeyCode::Enter), mode),
                Some(Action::Insert('\n')),
                "{mode:?}"
            );
        }
        // Anywhere else it is bound to nothing at all, at either field count: it
        // neither finishes a surface nor moves the keyboard, because a key that did
        // one of those here and a break there is a key with cases to remember.
        for fields in Fields::ALL.iter().copied() {
            let mode = one_line(fields);
            assert_eq!(action_for(plain(KeyCode::Enter), mode), None, "{mode:?}");
        }
    }

    #[test]
    fn a_field_that_holds_many_lines_is_moved_through_by_line_and_a_one_line_field_is_not() {
        for mode in text_areas() {
            for (key, intent) in [
                (plain(KeyCode::Up), Action::MoveUp),
                (plain(KeyCode::Down), Action::MoveDown),
                (ctrl('p'), Action::MoveUp),
                (ctrl('n'), Action::MoveDown),
            ] {
                assert_eq!(action_for(key, mode), Some(intent), "{key:?} in {mode:?}");
            }
        }
        // And nowhere else: a field holding one line has no line to move to, so
        // the keys are unbound there rather than landing where they started — and
        // the letters they are spelled with are still characters in it.
        for fields in Fields::ALL.iter().copied() {
            let mode = one_line(fields);
            for key in [
                plain(KeyCode::Up),
                plain(KeyCode::Down),
                ctrl('p'),
                ctrl('n'),
            ] {
                assert_eq!(action_for(key, mode), None, "{key:?} in {mode:?}");
            }
            for c in ['p', 'n'] {
                assert_eq!(
                    action_for(plain(KeyCode::Char(c)), mode),
                    Some(Action::Insert(c)),
                    "{c:?} in {mode:?}"
                );
            }
        }
    }

    #[test]
    fn ctrl_c_is_the_way_out_of_a_mode_and_quits_while_browsing_only() {
        // Raw mode delivers it as a key rather than a signal, so what it means is
        // this map's decision: inside editing mode it is the same key as `Esc`,
        // and only outside one does it end the session.
        assert_eq!(action_for(ctrl('c'), Mode::Browse), Some(Action::Quit));
        assert_eq!(action_for(ctrl('c'), Mode::Editing), Some(Action::Unwind));
        assert_eq!(
            action_for(ctrl('c'), Mode::Editing),
            action_for(plain(KeyCode::Esc), Mode::Editing),
            "inside a mode Ctrl-C is exactly Esc"
        );
    }

    #[test]
    fn preview_paging_is_not_shadowed_by_the_plain_motions() {
        assert_eq!(
            action_for(ctrl('d'), Mode::Browse),
            Some(Action::PreviewHalfDown)
        );
        assert_eq!(
            action_for(ctrl('u'), Mode::Browse),
            Some(Action::PreviewHalfUp)
        );
        assert_eq!(
            action_for(plain(KeyCode::Char('j')), Mode::Browse),
            Some(Action::CursorDown)
        );
    }

    #[test]
    fn unbound_keys_are_ignored() {
        // A key no mode binds is ignored in every mode, dialogs included: there
        // is no layer that quietly gives a spare letter a meaning of its own.
        for mode in [Mode::Browse, Mode::Editing]
            .into_iter()
            .chain(dialog_modes())
        {
            assert_eq!(action_for(plain(KeyCode::Char('x')), mode), None);
            assert_eq!(action_for(plain(KeyCode::F(5)), mode), None);
        }
    }

    #[test]
    fn an_editing_letter_is_bound_inside_the_mode_only() {
        // Browse mode is where a reader's fingers rest, so a letter that writes
        // must not be one stray keystroke away from a write there.
        for action in EditingAction::ALL.iter().copied() {
            let hint = FOOTER_HINTS_EDITING
                .iter()
                .find(|(bound, _)| *bound == action)
                .map(|(_, hint)| *hint)
                .expect("every editing action has a hint");
            let key = key_named(leading(hint));
            assert_eq!(action_for(key, Mode::Editing), Some(action.intent()));
            assert_eq!(
                action_for(key, Mode::Browse),
                None,
                "{hint:?} in browse mode"
            );
        }
        // And the shifted-out variant keeps its own meaning in both: the letter
        // did not take a modifier combination with it.
        for mode in [Mode::Browse, Mode::Editing] {
            assert_eq!(action_for(ctrl('d'), mode), Some(Action::PreviewHalfDown));
        }
    }

    #[test]
    fn the_destructive_answer_is_the_same_letter_and_never_the_reflex_key() {
        let destructive = Mode::Dialog(Answers::Destructive);
        // The letter that asks for a deletion is the letter that answers for it,
        // and it is the letter that answers everything else destructive too.
        assert_eq!(
            action_for(plain(KeyCode::Char('d')), destructive),
            action_for(plain(KeyCode::Char('d')), Mode::Editing)
        );
        // A reader arrives at a destructive question in a hurry: `Enter` is bound
        // to nothing at all there, so a reflex press cannot be what destroys.
        assert_eq!(action_for(plain(KeyCode::Enter), destructive), None);
        // `Esc` is the answer that is never destructive, anywhere.
        assert_eq!(
            action_for(plain(KeyCode::Esc), destructive),
            Some(Action::Unwind)
        );
        assert_eq!(action_for(ctrl('c'), destructive), Some(Action::Unwind));
    }

    #[test]
    fn each_answer_set_goes_ahead_by_its_own_letter_and_no_other_sets() {
        // The letter is this map's and its meaning is the set's, so the two cannot
        // drift: a set whose letter carried another set's intent would let a reader
        // answering one question perform the other's answer.
        for answers in Answers::ALL.iter().copied() {
            let mode = Mode::Dialog(answers);
            let affirmative = dialog_answers(answers, words())
                .into_iter()
                .find(|answer| answer.ends_with(words().affirmative.unwrap()));
            match (answers.affirmative(), affirmative) {
                (Some(intent), Some(answer)) => {
                    assert_eq!(
                        action_for(key_named(leading(&answer)), mode),
                        Some(intent),
                        "{answers:?} lists {answer:?}"
                    );
                    // And that letter reaches no other set: every other dialog
                    // either binds it to nothing or binds it to its own answer.
                    for other in Answers::ALL.iter().copied().filter(|o| *o != answers) {
                        assert_ne!(
                            action_for(key_named(leading(&answer)), Mode::Dialog(other)),
                            Some(intent),
                            "{answers:?}'s answer also answers {other:?}"
                        );
                    }
                }
                // A set that only reports lists no way to go ahead and has no
                // intent that would: the two halves agree that there is nothing to
                // answer.
                (None, None) => {}
                (intent, answer) => {
                    panic!("{answers:?} lists {answer:?} and performs {intent:?}")
                }
            }
        }
    }

    #[test]
    fn overwriting_a_change_underneath_the_buffer_is_not_the_destructive_letter() {
        // Both answers to a conflict lose something, so the letter a habit built on
        // deletions presses must not be the one that throws away somebody else's
        // text — and the reflex key answers nothing here either.
        let conflict = Mode::Dialog(Answers::Conflict);
        assert_eq!(
            action_for(plain(KeyCode::Char('o')), conflict),
            Some(Action::Overwrite)
        );
        assert_eq!(action_for(plain(KeyCode::Char('d')), conflict), None);
        assert_eq!(action_for(plain(KeyCode::Enter), conflict), None);
        // The way out, which keeps the buffer and decides nothing.
        for key in [plain(KeyCode::Esc), ctrl('c')] {
            assert_eq!(action_for(key, conflict), Some(Action::Unwind), "{key:?}");
        }
        // And the letter is the conflict's alone: nowhere else does it overwrite,
        // and in a field it is a character like any other.
        for mode in [Mode::Browse, Mode::Editing] {
            assert_ne!(
                action_for(plain(KeyCode::Char('o')), mode),
                Some(Action::Overwrite),
                "{mode:?}"
            );
        }
        assert_eq!(
            action_for(plain(KeyCode::Char('o')), one_line(Fields::One)),
            Some(Action::Insert('o'))
        );
    }

    #[test]
    fn each_letter_that_replaces_a_whole_field_is_bound_inside_the_mode_only() {
        // Browse mode is where a reader's fingers rest, so a letter that opens a
        // surface on stored text is bound inside the mode and nowhere else. The
        // summary takes the shifted letter because the unshifted one belongs to the
        // state a reader changes far more often: two unrelated nouns share a letter
        // deliberately, and neither letter carries anything else in either mode.
        for (letter, field) in [
            ('n', FreeForm::Name),
            ('S', FreeForm::Summary),
            ('b', FreeForm::Body),
        ] {
            assert_eq!(
                action_for(plain(KeyCode::Char(letter)), Mode::Editing),
                Some(Action::Edit(field)),
                "{letter:?}"
            );
            assert_eq!(
                action_for(plain(KeyCode::Char(letter)), Mode::Browse),
                None,
                "{letter:?}"
            );
            // And inside a field each of them is a character rather than the action
            // it carries one layer up.
            for mode in surface_modes() {
                assert_eq!(
                    action_for(plain(KeyCode::Char(letter)), mode),
                    Some(Action::Insert(letter)),
                    "{letter:?} in {mode:?}"
                );
            }
        }
        // The motions the letters are spelled like keep their own meaning in a
        // field: an editing letter never took a control combination with it.
        for mode in surface_modes() {
            assert_eq!(
                action_for(ctrl('b'), mode),
                Some(Action::MoveLeft),
                "{mode:?}"
            );
        }
        for mode in text_areas() {
            assert_eq!(
                action_for(ctrl('n'), mode),
                Some(Action::MoveDown),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn the_claim_pair_is_one_letter_and_its_shift_and_neither_is_the_control_key() {
        // Two halves of one noun, so a reader learns the letter once: unshifted takes
        // the claim, shifted gives it up. Distinct intents, because a single one with
        // a direction would let the letter that takes answer for the letter that
        // releases.
        assert_eq!(
            action_for(plain(KeyCode::Char('c')), Mode::Editing),
            Some(Action::TakeClaim)
        );
        assert_eq!(
            action_for(plain(KeyCode::Char('C')), Mode::Editing),
            Some(Action::ReleaseClaim)
        );
        // The letter takes no control combination with it: Ctrl-C is the way out of
        // a mode and the way out of the browser, and neither may become a write.
        for mode in [Mode::Browse, Mode::Editing] {
            for intent in [Action::TakeClaim, Action::ReleaseClaim] {
                assert_ne!(action_for(ctrl('c'), mode), Some(intent), "{mode:?}");
            }
        }
        // And inside a field both are characters rather than the actions they carry
        // one layer up, the shifted one included.
        for mode in surface_modes() {
            for letter in ['c', 'C'] {
                assert_eq!(
                    action_for(plain(KeyCode::Char(letter)), mode),
                    Some(Action::Insert(letter)),
                    "{letter:?} in {mode:?}"
                );
            }
        }
    }

    #[test]
    fn a_dialog_that_only_reports_is_dismissed_by_either_key() {
        let acknowledge = Mode::Dialog(Answers::Acknowledge);
        // Nothing is at stake, so a second key is kindness.
        for code in [KeyCode::Esc, KeyCode::Enter] {
            assert_eq!(action_for(plain(code), acknowledge), Some(Action::Unwind));
        }
        // And nothing else: neither answer destroys anything, so there is no
        // destructive answer to offer.
        assert_eq!(action_for(plain(KeyCode::Char('d')), acknowledge), None);
    }

    #[test]
    fn a_dialog_binds_nothing_that_reaches_past_it() {
        // A question is critical by construction — the flash carries everything
        // that is not — so no key pressed at one may move, reload or end the
        // session underneath it.
        for mode in dialog_modes() {
            for code in [
                KeyCode::Char('q'),
                KeyCode::Char('j'),
                KeyCode::Char('k'),
                KeyCode::Char('r'),
                KeyCode::Char('e'),
                KeyCode::Char('z'),
                KeyCode::Char('?'),
                KeyCode::Backspace,
            ] {
                assert_eq!(action_for(plain(code), mode), None, "{code:?} in {mode:?}");
            }
        }
    }

    #[test]
    fn every_editing_hint_names_an_intent_the_mode_can_carry() {
        for (action, hint) in FOOTER_HINTS_EDITING {
            // The hint's own letter has to be the letter bound to that intent, or
            // the strip would teach a key the mode does not answer.
            let letter = hint.chars().next().expect("a hint leads with its key");
            assert_eq!(
                action_for(plain(KeyCode::Char(letter)), Mode::Editing),
                Some(action.intent()),
                "{hint:?}"
            );
        }
    }

    #[test]
    fn every_action_a_row_may_offer_has_exactly_one_hint() {
        // The strip lists the subset of these the frozen row offers, so an action
        // with no hint is one no reader can discover, and a second hint for one
        // action is a letter taught twice.
        for action in EditingAction::ALL {
            let hints = FOOTER_HINTS_EDITING
                .iter()
                .filter(|(bound, _)| bound == action)
                .count();
            assert_eq!(hints, 1, "{action:?}");
        }
        // And no hint names anything else: a hint the offer table cannot decide on
        // would be a letter the strip shows and no row answers.
        assert_eq!(FOOTER_HINTS_EDITING.len(), EditingAction::ALL.len());
    }

    #[test]
    fn every_answer_a_dialog_lists_is_one_that_set_admits() {
        for answers in Answers::ALL.iter().copied() {
            let listed = dialog_answers(answers, words());
            // A dialog that lists no answer seals the reader inside it.
            assert!(!listed.is_empty(), "{answers:?} lists no way to answer it");
            for answer in &listed {
                // Every answer leads with the key that gives it, read back from
                // the way the dialog spells that key.
                assert!(
                    action_for(key_named(leading(answer)), Mode::Dialog(answers)).is_some(),
                    "{answer:?} is listed but not admitted by {answers:?}"
                );
            }
            // And the way out is listed by every set: dismissal is unconditional.
            assert!(
                listed.iter().any(|a| a.contains(words().dismissal)),
                "{answers:?} does not list its way out: {listed:?}"
            );
        }
    }

    #[test]
    fn a_dialog_words_its_own_answers_while_the_letters_stay_this_maps() {
        // Two dialogs share the destructive letter and mean different things by
        // it, so the words travel with the dialog: one set of hardwired words
        // could not say both, and a word never spells a key.
        let removing = dialog_answers(
            Answers::Destructive,
            AnswerWords {
                affirmative: Some("remove"),
                dismissal: "cancel",
            },
        );
        let discarding = dialog_answers(
            Answers::Destructive,
            AnswerWords {
                affirmative: Some("discard"),
                dismissal: "keep editing",
            },
        );
        assert_ne!(removing, discarding);
        assert_eq!(
            removing.iter().map(|a| leading(a)).collect::<Vec<_>>(),
            discarding.iter().map(|a| leading(a)).collect::<Vec<_>>(),
            "the letters are the set's, not the dialog's"
        );
        assert!(discarding[0].ends_with("discard"), "{discarding:?}");

        // A dialog that only reports has nothing to go ahead with, so it lists
        // one answer: the way out, and both keys that give it.
        let reporting = dialog_answers(
            Answers::Acknowledge,
            AnswerWords {
                affirmative: None,
                dismissal: "dismiss",
            },
        );
        assert_eq!(reporting, vec!["Esc / Enter dismiss".to_string()]);
    }
}
