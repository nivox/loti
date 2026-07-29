//! Keys to intents — the only place in the crate that names a key.
//!
//! The two panes never compete for a key, so there is no focus to switch: the
//! navigation cursor owns the plain motion keys and the preview owns the paging
//! keys. `Esc` leaves a level rather than the application, so a mis-hit while
//! browsing can never discard the session; `q` is the only way out.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;

/// The intent a key press carries, or `None` if it is not bound.
pub fn action_for(key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match (key.code, ctrl) {
        // Quitting. Raw mode delivers Ctrl-C as a key press rather than a
        // signal, so it has to be bound explicitly to be honoured at all.
        (KeyCode::Char('q'), false) | (KeyCode::Char('c'), true) => Action::Quit,

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
        (KeyCode::Backspace, _)
        | (KeyCode::Esc, _)
        | (KeyCode::Char('h'), false)
        | (KeyCode::Left, false) => Action::Ascend,

        // Layout.
        (KeyCode::Char('<'), false) => Action::ShrinkNav,
        (KeyCode::Char('>'), false) => Action::GrowNav,
        (KeyCode::Char('='), false) => Action::ResetSplit,
        (KeyCode::Char('z'), false) => Action::ToggleZoom,

        // Session.
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
    ("Backspace / Esc / h / ←", "leave the level"),
    ("Ctrl-D / Ctrl-U", "scroll the preview half a screen"),
    ("PgDn / PgUp / Space", "scroll the preview a screen"),
    ("Home / End", "preview start / end"),
    ("< / > / =", "narrow / widen / reset the panes"),
    ("z", "preview fills the width; mouse released"),
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

/// The hints that survive any width: the overlay is how every other binding is
/// discovered, and quitting must never be a hint the reader has to guess.
pub const FOOTER_ESSENTIAL: &[&str] = &["? keys", "q quit"];

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
    fn escape_leaves_a_level_and_never_the_application() {
        assert_eq!(action_for(plain(KeyCode::Esc)), Some(Action::Ascend));
        assert_eq!(action_for(plain(KeyCode::Char('q'))), Some(Action::Quit));
    }

    #[test]
    fn ctrl_c_quits_because_raw_mode_delivers_it_as_a_key() {
        assert_eq!(action_for(ctrl('c')), Some(Action::Quit));
    }

    #[test]
    fn preview_paging_is_not_shadowed_by_the_plain_motions() {
        assert_eq!(action_for(ctrl('d')), Some(Action::PreviewHalfDown));
        assert_eq!(action_for(ctrl('u')), Some(Action::PreviewHalfUp));
        assert_eq!(
            action_for(plain(KeyCode::Char('j'))),
            Some(Action::CursorDown)
        );
    }

    #[test]
    fn unbound_keys_are_ignored() {
        assert_eq!(action_for(plain(KeyCode::Char('x'))), None);
        assert_eq!(action_for(plain(KeyCode::F(5))), None);
    }
}
