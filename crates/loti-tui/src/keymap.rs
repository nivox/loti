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
            Mode::Editing => Action::Unwind,
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

        // Session.
        (KeyCode::Char('e'), false) => Action::EnterEditing,
        (KeyCode::Char('r'), false) => Action::Reload,
        (KeyCode::Char('?'), false) => Action::ToggleHelp,

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

/// The droppable hints of editing mode: the actions the frozen row offers, in
/// this map's own order rather than reordered per row.
///
/// Empty while the mode carries no action at all: an action slice adds its own
/// hint here, so the strip only ever lists what the row can really do.
pub const FOOTER_HINTS_EDITING: &[&str] = &[];

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
        // A mode chooses which intent a bound key carries, never whether it is
        // bound at all: an unbound key is unbound in every mode, so no mode can
        // smuggle in a binding of its own.
        for mode in [Mode::Browse, Mode::Editing] {
            assert_eq!(action_for(plain(KeyCode::Char('x')), mode), None);
            assert_eq!(action_for(plain(KeyCode::F(5)), mode), None);
        }
    }
}
