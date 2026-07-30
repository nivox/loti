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
    /// A destructive question is open. It is answered by the same letter that
    /// asks for a deletion, so no key a hurried reader presses by reflex can be
    /// what destroys something: the way out answers it safely, and the key that
    /// normally means "yes, go on" is bound to nothing here at all.
    Confirm,
    /// A dialog with nothing at stake is open — it reports rather than asks — so
    /// it is dismissed rather than answered.
    Acknowledge,
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
    /// Delete the row editing mode is acting on, which asks first. On the
    /// confirmation that asks, the same intent is the confirming answer: one
    /// letter answers everything destructive, so it is learned once.
    Delete,
    /// Toggle the key-binding overlay.
    ToggleHelp,
    /// Leave the browser.
    Quit,
}
