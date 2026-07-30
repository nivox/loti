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

use crate::action::{Action, AnswerWords, Answers, EditingAction, Fields, Mode};

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
    if let Mode::Surface(fields) = mode {
        return surface_action(key, fields);
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

        // Session. `F1` is a help key beside `?` everywhere, because inside a
        // field `?` is a literal character and a reader must not have to learn a
        // second help key on arriving there.
        (KeyCode::Char('e'), false) => Action::EnterEditing,
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
/// How many fields the surface holds is an input rather than something read off a
/// key, because the reflex key means one thing on each shape.
fn surface_action(key: KeyEvent, fields: Fields) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match (key.code, ctrl) {
        // Raw mode is already on, which clears `IXON`, so Ctrl-S arrives as a key
        // press instead of being eaten as flow control.
        (KeyCode::Char('s'), true) => Action::Accept,
        // A one-field surface is finished by the reflex key too: there is no next
        // field for it to move to. Where there is one it moves instead, so the save
        // key is the only way to accept a form and a reader pressing on through the
        // fields never submits one by arriving at the last.
        (KeyCode::Enter, _) => match fields {
            Fields::One => Action::Accept,
            Fields::Several => Action::NextField,
        },
        // Field navigation, bound where there is another field to reach and nowhere
        // else: a key that did nothing on a one-field surface would be a key taught
        // for nothing.
        (KeyCode::Tab, _) if matches!(fields, Fields::Several) => Action::NextField,
        (KeyCode::BackTab, _) if matches!(fields, Fields::Several) => Action::PreviousField,
        (KeyCode::Char('g'), true) => Action::ExternalEditor,
        (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => Action::Unwind,
        // The one help key that can be pressed inside a field.
        (KeyCode::F(1), _) => Action::ToggleHelp,

        // The emacs motions, with the arrows and the line keys beside them. A
        // single-line field's start and end are its line's, so the keys that page
        // a preview while browsing move within the field here.
        (KeyCode::Char('a'), true) | (KeyCode::Home, _) => Action::MoveToStart,
        (KeyCode::Char('e'), true) | (KeyCode::End, _) => Action::MoveToEnd,
        (KeyCode::Char('b'), true) | (KeyCode::Left, _) => Action::MoveLeft,
        (KeyCode::Char('f'), true) | (KeyCode::Right, _) => Action::MoveRight,

        // Ctrl-H is unavailable as a binding of its own: terminals send 0x08 for
        // Backspace, so it cannot be told apart from the key that deletes a
        // character.
        (KeyCode::Backspace, _) => Action::DeleteBefore,
        (KeyCode::Delete, _) => Action::DeleteAfter,
        (KeyCode::Char(c), false) => Action::Insert(c),

        _ => return None,
    })
}

/// The letters one set of dialog answers is given, which are exactly the answers
/// that set lists.
///
/// `d` answers anything destructive and `Esc` is the one answer that is never
/// destructive, here as everywhere else. On a destructive question `Enter` is
/// bound to nothing at all: a reader arrives at one in a hurry, so a reflex press
/// must not be the thing that destroys something. Where nothing is at stake
/// `Enter` dismisses alongside `Esc`, because a second key is then kindness.
fn dialog_action(key: KeyEvent, answers: Answers) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match (key.code, ctrl) {
        (KeyCode::Char('d'), false) if matches!(answers, Answers::Destructive) => Action::Delete,
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
    match answers {
        Answers::Destructive => words
            .affirmative
            .into_iter()
            .map(|word| format!("d {word}"))
            .chain(std::iter::once(dismissal))
            .collect(),
        // Nothing is at stake, so both dismissing keys are listed on the one
        // answer they share.
        Answers::Acknowledge => vec![format!("Esc / Enter {}", words.dismissal)],
    }
}

/// The bindings as the help overlay and the footer present them: one row per
/// group, so both surfaces describe the same keymap without restating it.
pub const HELP: &[(&str, &str)] = &[
    ("j / k / ↓ / ↑", "move the cursor"),
    ("g / G", "first / last row"),
    (
        "Enter / l / →",
        "open the row (nothing if it has nothing below)",
    ),
    ("Backspace / h / ←", "leave the level"),
    ("Esc", "leave the level, a field, or editing mode"),
    ("Ctrl-D / Ctrl-U", "scroll the preview half a screen"),
    ("PgDn / PgUp / Space", "scroll the preview a screen"),
    (
        "Home / End",
        "preview start / end; a field's ends inside one",
    ),
    ("< / > / =", "narrow / widen / reset the panes"),
    ("z", "preview fills the width; mouse released"),
    ("e", "editing mode, on the highlighted row"),
    ("a", "editing mode: add a label or a blocker, in a dialog"),
    ("d", "editing mode: remove or delete the row, confirmed"),
    (
        "Ctrl-S / Enter",
        "save the open surface; Enter if it has one field",
    ),
    (
        "Tab / Shift-Tab",
        "next / previous field; Enter moves forwards too",
    ),
    ("Ctrl-G", "edit the open field in $EDITOR"),
    (
        "Ctrl-A / Ctrl-E",
        "field start / end; Ctrl-B / Ctrl-F or ← / → by one",
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
/// The field-navigation hint is listed on a surface with fields to move between
/// and on no other, because the strip must never name a key the surface ignores.
pub fn footer_hints_surface(fields: Fields) -> Vec<&'static str> {
    let mut hints = vec!["Ctrl-S save"];
    if matches!(fields, Fields::Several) {
        hints.push("Tab fields");
    }
    hints.push("Ctrl-G editor");
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

    /// Every mode an open surface puts the keyboard under, one per field shape: a
    /// key that belongs to the field belongs to it on either shape, so a test
    /// about the field walks both.
    fn surface_modes() -> Vec<Mode> {
        Fields::ALL.iter().copied().map(Mode::Surface).collect()
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
    fn a_one_field_surface_is_accepted_by_either_key_and_left_by_the_way_out() {
        let one = Mode::Surface(Fields::One);
        // Ctrl-S accepts anywhere; a one-field surface has no next field for the
        // reflex key to move to, so it accepts too.
        for key in [ctrl('s'), plain(KeyCode::Enter)] {
            assert_eq!(action_for(key, one), Some(Action::Accept), "{key:?}");
        }
        // And no field to move to means no key that moves: a bound key that could
        // only land where it started is a key taught for nothing.
        for key in [key_named("Tab"), key_named("Shift-Tab")] {
            assert_eq!(action_for(key, one), None, "{key:?}");
        }
        for mode in surface_modes() {
            // The way out, and its alias: inside a mode Ctrl-C is exactly Esc.
            for key in [plain(KeyCode::Esc), ctrl('c')] {
                assert_eq!(
                    action_for(key, mode),
                    Some(Action::Unwind),
                    "{key:?} in {mode:?}"
                );
            }
            assert_eq!(
                action_for(ctrl('g'), mode),
                Some(Action::ExternalEditor),
                "{mode:?}"
            );
            // Saving is the one way to accept that every shape shares.
            assert_eq!(
                action_for(ctrl('s'), mode),
                Some(Action::Accept),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_surface_with_several_fields_is_navigated_and_the_reflex_key_moves_rather_than_accepts() {
        let several = Mode::Surface(Fields::Several);
        // The reflex key means what the shape says, which is why the shape reaches
        // this map at all: on a form it moves on, so pressing through the fields
        // never submits one, and the save key stays the only way to accept.
        assert_eq!(
            action_for(plain(KeyCode::Enter), several),
            Some(Action::NextField)
        );
        assert_eq!(
            action_for(plain(KeyCode::Enter), Mode::Surface(Fields::One)),
            Some(Action::Accept),
            "the reflex key means the same thing on both shapes"
        );
        // Forwards and backwards, by the keys a form is navigated with everywhere.
        assert_eq!(
            action_for(key_named("Tab"), several),
            Some(Action::NextField)
        );
        assert_eq!(
            action_for(key_named("Shift-Tab"), several),
            Some(Action::PreviousField)
        );
        // The navigation keys are the surface's alone: while browsing they are not
        // bound at all, so nothing moves a field cursor that does not exist.
        for mode in [Mode::Browse, Mode::Editing] {
            for key in [key_named("Tab"), key_named("Shift-Tab")] {
                assert_eq!(action_for(key, mode), None, "{key:?} in {mode:?}");
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
            action_for(plain(KeyCode::Char('?')), Mode::Surface(Fields::One)),
            Some(Action::Insert('?'))
        );
    }

    #[test]
    fn every_surface_hint_names_a_key_that_shape_of_surface_answers() {
        // The strip teaches keys by name, so a hint naming a key the surface does
        // not answer teaches a key that does nothing. These are not the editing
        // actions' hints: a row offers those, and while a surface is open no row
        // offers anything.
        for fields in Fields::ALL.iter().copied() {
            let hints = footer_hints_surface(fields);
            for hint in &hints {
                assert!(
                    action_for(key_named(leading(hint)), Mode::Surface(fields)).is_some(),
                    "{hint:?} names a key {fields:?} ignores"
                );
            }
            // And the field keys are hinted exactly where they are bound: a shape
            // with fields to move between says so, and the one without does not
            // teach a key it ignores.
            assert_eq!(
                hints.iter().any(|hint| leading(hint) == "Tab"),
                fields == Fields::Several,
                "{fields:?}: {hints:?}"
            );
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
