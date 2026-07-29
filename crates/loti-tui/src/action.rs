//! Every user intent the browser can carry out, named independently of the keys
//! that trigger it.
//!
//! Invariant: this enum is the only vocabulary the application state
//! understands. A new capability is a new variant plus one binding in
//! [`crate::keymap`]; a rebinding touches the keymap alone. Nothing here knows
//! about key codes, and nothing in the state machine matches on a key.

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
    /// Toggle the key-binding overlay.
    ToggleHelp,
    /// Leave the browser.
    Quit,
}
