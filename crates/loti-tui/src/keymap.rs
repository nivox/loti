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

use crate::action::{Action, Mode};

/// The intent a key press carries in a mode, or `None` if it is not bound there.
pub fn action_for(key: KeyEvent, mode: Mode) -> Option<Action> {
    // A dialog answers for itself: only the answers it lists are bound while it
    // is open, so nothing underneath it can be moved, reloaded or quit by a key
    // pressed at a question.
    if matches!(mode, Mode::Confirm | Mode::Acknowledge) {
        return dialog_action(key, mode);
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
            // A dialog's own answers are settled before this table is reached, so
            // only browsing and editing arrive here.
            Mode::Editing | Mode::Confirm | Mode::Acknowledge => Action::Unwind,
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
        (KeyCode::Char('d'), false) if matches!(mode, Mode::Editing) => Action::Delete,

        // Session.
        (KeyCode::Char('e'), false) => Action::EnterEditing,
        (KeyCode::Char('r'), false) => Action::Reload,
        (KeyCode::Char('?'), false) => Action::ToggleHelp,

        _ => return None,
    })
}

/// The answers a dialog admits, which are exactly the answers it lists.
///
/// `d` answers anything destructive and `Esc` is the one answer that is never
/// destructive, here as everywhere else. On a destructive question `Enter` is
/// bound to nothing at all: a reader arrives at one in a hurry, so a reflex press
/// must not be the thing that destroys something. Where nothing is at stake
/// `Enter` dismisses alongside `Esc`, because a second key is then kindness.
fn dialog_action(key: KeyEvent, mode: Mode) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match (key.code, ctrl) {
        (KeyCode::Char('d'), false) if matches!(mode, Mode::Confirm) => Action::Delete,
        (KeyCode::Enter, _) if matches!(mode, Mode::Acknowledge) => Action::Unwind,
        (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => Action::Unwind,
        _ => return None,
    })
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
    ("Esc", "leave the level, or editing mode while it is on"),
    ("Ctrl-D / Ctrl-U", "scroll the preview half a screen"),
    ("PgDn / PgUp / Space", "scroll the preview a screen"),
    ("Home / End", "preview start / end"),
    ("< / > / =", "narrow / widen / reset the panes"),
    ("z", "preview fills the width; mouse released"),
    ("e", "editing mode, on the highlighted row"),
    ("d", "editing mode: remove the label, with a confirmation"),
    ("r", "re-read the store"),
    ("?", "these keys"),
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
/// Each hint travels with the intent it names, because the strip lists the subset
/// the frozen row offers and only the state machine knows which that is.
pub const FOOTER_HINTS_EDITING: &[(Action, &str)] = &[(Action::Delete, "d remove")];

/// The answers a destructive question lists, in the order it lists them. A dialog
/// says how to answer it, so the way out of one never depends on the hint strip a
/// notice or a narrow terminal may have taken.
pub const DIALOG_ANSWERS_CONFIRM: &[&str] = &["d remove", "Esc cancel"];

/// The answers a dialog that only reports lists; see [`DIALOG_ANSWERS_CONFIRM`].
pub const DIALOG_ANSWERS_ACKNOWLEDGE: &[&str] = &["Esc / Enter dismiss"];

/// Editing mode's essential pair. Neither browse hint applies — `q` does not quit
/// while the mode is on, and the way out of the mode is not the way out of a
/// level — so without a pair of its own a narrow terminal would show a mode with
/// no visible way out of it.
pub const FOOTER_ESSENTIAL_EDITING: &[&str] = &["Esc leave", "? keys"];

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
        for essential in [FOOTER_ESSENTIAL, FOOTER_ESSENTIAL_EDITING] {
            // Clipping eats the tail, so the rank is the order: the way out
            // first, help second.
            assert_eq!(essential.len(), 2);
            assert!(essential[1].contains('?'), "{essential:?}");
        }
        // And the way out of a mode is not the way out of the browser.
        assert_ne!(FOOTER_ESSENTIAL[0], FOOTER_ESSENTIAL_EDITING[0]);
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
        for mode in [
            Mode::Browse,
            Mode::Editing,
            Mode::Confirm,
            Mode::Acknowledge,
        ] {
            assert_eq!(action_for(plain(KeyCode::Char('x')), mode), None);
            assert_eq!(action_for(plain(KeyCode::F(5)), mode), None);
        }
    }

    #[test]
    fn an_editing_letter_is_bound_inside_the_mode_only() {
        // Browse mode is where a reader's fingers rest, so a letter that removes
        // something must not be one stray keystroke away from a write there.
        assert_eq!(
            action_for(plain(KeyCode::Char('d')), Mode::Editing),
            Some(Action::Delete)
        );
        assert_eq!(action_for(plain(KeyCode::Char('d')), Mode::Browse), None);
        // And the shifted-out variant keeps its own meaning in both: the letter
        // did not take a modifier combination with it.
        for mode in [Mode::Browse, Mode::Editing] {
            assert_eq!(action_for(ctrl('d'), mode), Some(Action::PreviewHalfDown));
        }
    }

    #[test]
    fn the_destructive_answer_is_the_same_letter_and_never_the_reflex_key() {
        // The letter that asks for a deletion is the letter that answers for it.
        assert_eq!(
            action_for(plain(KeyCode::Char('d')), Mode::Confirm),
            action_for(plain(KeyCode::Char('d')), Mode::Editing)
        );
        // A reader arrives at a destructive question in a hurry: `Enter` is bound
        // to nothing at all there, so a reflex press cannot be what destroys.
        assert_eq!(action_for(plain(KeyCode::Enter), Mode::Confirm), None);
        // `Esc` is the answer that is never destructive, anywhere.
        assert_eq!(
            action_for(plain(KeyCode::Esc), Mode::Confirm),
            Some(Action::Unwind)
        );
        assert_eq!(action_for(ctrl('c'), Mode::Confirm), Some(Action::Unwind));
    }

    #[test]
    fn a_dialog_that_only_reports_is_dismissed_by_either_key() {
        // Nothing is at stake, so a second key is kindness.
        for code in [KeyCode::Esc, KeyCode::Enter] {
            assert_eq!(
                action_for(plain(code), Mode::Acknowledge),
                Some(Action::Unwind)
            );
        }
        // And nothing else: neither answer destroys anything, so there is no
        // destructive answer to offer.
        assert_eq!(
            action_for(plain(KeyCode::Char('d')), Mode::Acknowledge),
            None
        );
    }

    #[test]
    fn a_dialog_binds_nothing_that_reaches_past_it() {
        // A question is critical by construction — the flash carries everything
        // that is not — so no key pressed at one may move, reload or end the
        // session underneath it.
        for mode in [Mode::Confirm, Mode::Acknowledge] {
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
                Some(*action),
                "{hint:?}"
            );
        }
    }
}
