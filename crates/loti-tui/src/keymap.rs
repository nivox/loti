//! Keys to intents — the only place in the crate that names bindings.
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

    // The key list is the binding map. Dispatching through its entries means an
    // entry cannot disappear from help while its key remains live, nor can help
    // advertise a key without also making it live here.
    HELP.iter()
        .find_map(|entry| action_for_binding(entry.binding, key, mode))
        .or_else(|| surface_character(key, mode))
}

/// Plain characters are field content, not browser bindings. Every named
/// binding is dispatched through [`HELP`] above; this fallback exists solely so
/// text fields can accept their unbounded input alphabet.
fn surface_character(key: KeyEvent, mode: Mode) -> Option<Action> {
    matches!(mode, Mode::Surface(_))
        .then(
            || match (key.code, key.modifiers.contains(KeyModifiers::CONTROL)) {
                (KeyCode::Char(c), false) => Some(Action::Insert(c)),
                _ => None,
            },
        )
        .flatten()
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

/// A group of bindings described by one key-list row. The group lives on its
/// entry so dispatch and presentation share one source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpBinding {
    Motion,
    FirstLast,
    Descend,
    Ascend,
    Unwind,
    Interrupt,
    PreviewPaging,
    PreviewEnds,
    Layout,
    StartWriting,
    AddDelete,
    EditFields,
    StateClaim,
    Save,
    FieldMotion,
    ExternalEditor,
    LineMotion,
    Reload,
    Help,
    Quit,
}

/// The action one listed binding carries in the mode where it applies.
fn action_for_binding(binding: HelpBinding, key: KeyEvent, mode: Mode) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let browsing = matches!(mode, Mode::Browse | Mode::Editing);
    let surface = match mode {
        Mode::Surface(shape) => Some(shape),
        Mode::Browse | Mode::Editing | Mode::Dialog(_) => None,
    };

    match binding {
        HelpBinding::Motion => match (key.code, ctrl, surface) {
            (KeyCode::Char('j'), false, None) | (KeyCode::Down, false, None) if browsing => {
                Some(Action::CursorDown)
            }
            (KeyCode::Char('k'), false, None) | (KeyCode::Up, false, None) if browsing => {
                Some(Action::CursorUp)
            }
            (KeyCode::Up, _, Some(shape)) | (KeyCode::Char('p'), true, Some(shape))
                if matches!(shape.kind, FieldKind::ManyLines | FieldKind::Pick) =>
            {
                Some(Action::MoveUp)
            }
            (KeyCode::Down, _, Some(shape)) | (KeyCode::Char('n'), true, Some(shape))
                if matches!(shape.kind, FieldKind::ManyLines | FieldKind::Pick) =>
            {
                Some(Action::MoveDown)
            }
            _ => None,
        },
        HelpBinding::FirstLast if browsing => match (key.code, ctrl) {
            (KeyCode::Char('g'), false) => Some(Action::CursorFirst),
            (KeyCode::Char('G'), false) => Some(Action::CursorLast),
            _ => None,
        },
        HelpBinding::Descend => match (key.code, ctrl, surface) {
            (KeyCode::Enter, _, None)
            | (KeyCode::Char('l'), false, None)
            | (KeyCode::Right, false, None)
                if browsing =>
            {
                Some(Action::Descend)
            }
            (KeyCode::Enter, _, Some(shape)) if matches!(shape.kind, FieldKind::ManyLines) => {
                Some(Action::Insert('\n'))
            }
            (KeyCode::Right, _, Some(_)) => Some(Action::MoveRight),
            _ => None,
        },
        HelpBinding::Ascend => match (key.code, ctrl, surface) {
            (KeyCode::Backspace, _, None)
            | (KeyCode::Char('h'), false, None)
            | (KeyCode::Left, false, None)
                if browsing =>
            {
                Some(Action::Ascend)
            }
            (KeyCode::Backspace, _, Some(_)) => Some(Action::DeleteBefore),
            (KeyCode::Left, _, Some(_)) => Some(Action::MoveLeft),
            _ => None,
        },
        HelpBinding::Unwind if matches!(key.code, KeyCode::Esc) => Some(Action::Unwind),
        HelpBinding::Interrupt if matches!((key.code, ctrl), (KeyCode::Char('c'), true)) => {
            Some(if matches!(mode, Mode::Browse) {
                Action::Quit
            } else {
                Action::Unwind
            })
        }
        HelpBinding::PreviewPaging if browsing => match (key.code, ctrl) {
            (KeyCode::Char('d'), true) => Some(Action::PreviewHalfDown),
            (KeyCode::Char('u'), true) => Some(Action::PreviewHalfUp),
            (KeyCode::PageDown, _) | (KeyCode::Char(' '), false) => Some(Action::PreviewPageDown),
            (KeyCode::PageUp, _) => Some(Action::PreviewPageUp),
            _ => None,
        },
        HelpBinding::PreviewEnds => match (key.code, surface) {
            (KeyCode::Home, None) if browsing => Some(Action::PreviewTop),
            (KeyCode::End, None) if browsing => Some(Action::PreviewBottom),
            (KeyCode::Home, Some(_)) => Some(Action::MoveToStart),
            (KeyCode::End, Some(_)) => Some(Action::MoveToEnd),
            _ => None,
        },
        HelpBinding::Layout if browsing => match (key.code, ctrl) {
            (KeyCode::Char('<'), false) => Some(Action::ShrinkNav),
            (KeyCode::Char('>'), false) => Some(Action::GrowNav),
            (KeyCode::Char('='), false) => Some(Action::ResetSplit),
            (KeyCode::Char('z'), false) => Some(Action::ToggleZoom),
            _ => None,
        },
        HelpBinding::StartWriting if browsing => match (key.code, ctrl) {
            (KeyCode::Char('e'), false) => Some(Action::EnterEditing),
            (KeyCode::Char('N'), false) => Some(Action::CreateEpic),
            _ => None,
        },
        HelpBinding::AddDelete if matches!(mode, Mode::Editing) => match (key.code, ctrl) {
            (KeyCode::Char('a'), false) => Some(Action::Add),
            (KeyCode::Char('d'), false) => Some(Action::Delete),
            _ => None,
        },
        HelpBinding::EditFields if matches!(mode, Mode::Editing) => match (key.code, ctrl) {
            (KeyCode::Char('n'), false) => Some(Action::Edit(FreeForm::Name)),
            (KeyCode::Char('S'), false) => Some(Action::Edit(FreeForm::Summary)),
            (KeyCode::Char('b'), false) => Some(Action::Edit(FreeForm::Body)),
            _ => None,
        },
        HelpBinding::StateClaim if matches!(mode, Mode::Editing) => match (key.code, ctrl) {
            (KeyCode::Char('s'), false) => Some(Action::SetState),
            (KeyCode::Char('c'), false) => Some(Action::TakeClaim),
            (KeyCode::Char('C'), false) => Some(Action::ReleaseClaim),
            (KeyCode::Char('w'), false) => Some(Action::RunAgent),
            _ => None,
        },
        HelpBinding::Save
            if surface.is_some() && matches!((key.code, ctrl), (KeyCode::Char('s'), true)) =>
        {
            Some(Action::Accept)
        }
        HelpBinding::FieldMotion if matches!(surface, Some(shape) if matches!(shape.fields, Fields::Several)) => {
            match key.code {
                KeyCode::Tab => Some(Action::NextField),
                KeyCode::BackTab => Some(Action::PreviousField),
                _ => None,
            }
        }
        HelpBinding::ExternalEditor
            if matches!(surface, Some(shape) if matches!(shape.kind, FieldKind::OneLine | FieldKind::ManyLines))
                && matches!((key.code, ctrl), (KeyCode::Char('g'), true)) =>
        {
            Some(Action::ExternalEditor)
        }
        HelpBinding::LineMotion if surface.is_some() => match (key.code, ctrl) {
            (KeyCode::Char('a'), true) => Some(Action::MoveToStart),
            (KeyCode::Char('e'), true) => Some(Action::MoveToEnd),
            (KeyCode::Char('b'), true) => Some(Action::MoveLeft),
            (KeyCode::Char('f'), true) => Some(Action::MoveRight),
            (KeyCode::Delete, _) => Some(Action::DeleteAfter),
            _ => None,
        },
        HelpBinding::Reload
            if browsing && matches!((key.code, ctrl), (KeyCode::Char('r'), false)) =>
        {
            Some(Action::Reload)
        }
        HelpBinding::Help => match (key.code, surface) {
            (KeyCode::Char('?'), None) if browsing => Some(Action::ToggleHelp),
            (KeyCode::F(1), _) => Some(Action::ToggleHelp),
            _ => None,
        },
        HelpBinding::Quit
            if browsing && matches!((key.code, ctrl), (KeyCode::Char('q'), false)) =>
        {
            Some(Action::Quit)
        }
        _ => None,
    }
}

/// One row in the key list. `requires_write` keeps the list honest on a store
/// this browser cannot write: a binding that could not reach a surface is not a
/// capability the reader can use there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelpEntry {
    pub keys: &'static str,
    pub description: &'static str,
    requires_write: bool,
    binding: HelpBinding,
}

/// The bindings as the help overlay presents them: one row per group. The list is
/// also filtered from this one table when the store is read-only, so the overlay
/// cannot offer a write that the browser knows it will refuse.
pub const HELP: &[HelpEntry] = &[
    HelpEntry {
        keys: "j / k / ↓ / ↑ / Ctrl-P / Ctrl-N",
        description: "move; ↑/↓ or Ctrl-P/N in a field",
        requires_write: false,
        binding: HelpBinding::Motion,
    },
    HelpEntry {
        keys: "g / G",
        description: "first / last row",
        requires_write: false,
        binding: HelpBinding::FirstLast,
    },
    // Enter is a line break where breaks are content and nothing at all in a
    // one-line field or picker; it never accepts a surface.
    HelpEntry {
        keys: "Enter / l / →",
        description: "open; newline in text area, else ignored",
        requires_write: false,
        binding: HelpBinding::Descend,
    },
    HelpEntry {
        keys: "Backspace / h / ←",
        description: "leave the level",
        requires_write: false,
        binding: HelpBinding::Ascend,
    },
    HelpEntry {
        keys: "Esc",
        description: "leave the level, a field, or editing mode",
        requires_write: false,
        binding: HelpBinding::Unwind,
    },
    HelpEntry {
        keys: "Ctrl-C",
        description: "quit while browsing; otherwise like Esc",
        requires_write: false,
        binding: HelpBinding::Interrupt,
    },
    HelpEntry {
        keys: "PgDn / PgUp / Space",
        description: "page preview; Ctrl-D / Ctrl-U half",
        requires_write: false,
        binding: HelpBinding::PreviewPaging,
    },
    HelpEntry {
        keys: "Home / End",
        description: "preview start / end; field line ends",
        requires_write: false,
        binding: HelpBinding::PreviewEnds,
    },
    // These layout keys share one row so the list remains wholly visible at the
    // shortest supported terminal height; their descriptions stay on the row.
    HelpEntry {
        keys: "< / > / = / z",
        description: "resize; z fills width, releases mouse",
        requires_write: false,
        binding: HelpBinding::Layout,
    },
    HelpEntry {
        keys: "e / N",
        description: "edit a row / new epic from epics",
        requires_write: true,
        binding: HelpBinding::StartWriting,
    },
    HelpEntry {
        keys: "a / d",
        description: "edit: add member / confirmed removal",
        requires_write: true,
        binding: HelpBinding::AddDelete,
    },
    // The name and summary are short floats; the long-form text is the preview
    // pane. Keeping the three keys together preserves a whole row for each binding.
    HelpEntry {
        keys: "n / S / b",
        description: "edit name/summary floats; text in preview",
        requires_write: true,
        binding: HelpBinding::EditFields,
    },
    HelpEntry {
        keys: "s / c / C / w",
        description: "edit: state / take / release / workflow",
        requires_write: true,
        binding: HelpBinding::StateClaim,
    },
    HelpEntry {
        keys: "Ctrl-S",
        description: "save, whichever field you are in",
        requires_write: true,
        binding: HelpBinding::Save,
    },
    HelpEntry {
        keys: "Tab / Shift-Tab",
        description: "next / previous field",
        requires_write: true,
        binding: HelpBinding::FieldMotion,
    },
    HelpEntry {
        keys: "Ctrl-G",
        description: "edit the open field in $EDITOR",
        requires_write: true,
        binding: HelpBinding::ExternalEditor,
    },
    HelpEntry {
        keys: "Ctrl-A / Ctrl-E",
        description: "line start/end; Ctrl-B/F or ←/→ moves one",
        requires_write: true,
        binding: HelpBinding::LineMotion,
    },
    HelpEntry {
        keys: "r",
        description: "re-read the store",
        requires_write: false,
        binding: HelpBinding::Reload,
    },
    HelpEntry {
        keys: "? / F1",
        description: "these keys; F1 works inside a field",
        requires_write: false,
        binding: HelpBinding::Help,
    },
    HelpEntry {
        keys: "q",
        description: "quit while browsing",
        requires_write: false,
        binding: HelpBinding::Quit,
    },
];

/// The list entries a browser in this store state may teach. The returned rows
/// still originate in [`HELP`], so the writable and read-only overlays cannot
/// acquire separate wording or a different binding order.
pub fn help(can_write: bool) -> impl Iterator<Item = &'static HelpEntry> {
    HELP.iter()
        .filter(move |entry| can_write || !entry.requires_write)
}

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

/// The two browse-mode entries into writing. They lead the droppable strip when
/// they apply so an ordinary-width terminal teaches how to begin a write; the
/// essential pair still wins at widths that cannot hold them.
const FOOTER_HINT_ENTER_EDITING: &str = "e edit";
const FOOTER_HINT_CREATE_EPIC: &str = "N new epic";

/// Browse hints filtered to what the browser can perform at the level on screen.
/// The state machine owns whether a row exists and whether the store can be
/// written; this table owns the binding words and their order.
pub fn footer_hints_browse(can_enter_editing: bool, can_create_epic: bool) -> Vec<&'static str> {
    let mut hints = Vec::with_capacity(FOOTER_HINTS.len() + 2);
    if can_enter_editing {
        hints.push(FOOTER_HINT_ENTER_EDITING);
    }
    if can_create_epic {
        hints.push(FOOTER_HINT_CREATE_EPIC);
    }
    hints.extend_from_slice(FOOTER_HINTS);
    hints
}

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
    (EditingAction::RunAgent, "w workflow"),
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
    fn browse_write_hints_are_shown_only_where_the_browser_can_start_a_write() {
        let cases = [
            (true, true, vec!["e edit", "N new epic"]),
            (true, false, vec!["e edit"]),
            (false, true, vec!["N new epic"]),
            (false, false, vec![]),
        ];
        for (can_enter, can_create, expected) in cases {
            let hints = footer_hints_browse(can_enter, can_create);
            for hint in ["e edit", "N new epic"] {
                assert_eq!(
                    hints.contains(&hint),
                    expected.contains(&hint),
                    "{can_enter}/{can_create}: {hints:?}"
                );
            }
        }
        // The two labels are derived from the bindings the reader presses, so a
        // strip entry cannot teach a key that does nothing in browse mode.
        assert_eq!(
            action_for(key_named("e"), Mode::Browse),
            Some(Action::EnterEditing)
        );
        assert_eq!(
            action_for(key_named("N"), Mode::Browse),
            Some(Action::CreateEpic)
        );
    }

    #[test]
    fn the_key_list_hides_every_write_binding_on_a_read_only_store() {
        let writable: Vec<_> = help(true).collect();
        let read_only: Vec<_> = help(false).collect();
        assert!(
            writable.iter().any(|entry| entry.keys == "e / N"),
            "the writable list lost the way into writing"
        );
        for entry in &read_only {
            assert!(
                writable.contains(entry),
                "the read-only list invented {:?}",
                entry.keys
            );
            assert!(
                !entry.requires_write,
                "the read-only list teaches {:?}",
                entry.keys
            );
        }
        assert!(
            writable
                .iter()
                .any(|entry| entry.requires_write && !read_only.contains(entry)),
            "no write binding was removed from the read-only list"
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
    fn reload_is_live_where_the_key_list_says_it_is() {
        // The list is the dispatch table, not a second catalogue. Removing this
        // row therefore removes the binding too; this test pins the browser
        // behaviour the row exists to expose.
        for mode in [Mode::Browse, Mode::Editing] {
            assert_eq!(
                action_for(plain(KeyCode::Char('r')), mode),
                Some(Action::Reload),
                "{mode:?}"
            );
        }
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
    fn workflow_is_an_editing_action_bound_and_hinted_by_w() {
        // The offer table decides whether this action is available on a frozen
        // row; this layer pins only the intent and the words teaching its key.
        assert_eq!(
            action_for(plain(KeyCode::Char('w')), Mode::Editing),
            Some(Action::RunAgent)
        );
        assert_eq!(action_for(plain(KeyCode::Char('w')), Mode::Browse), None);
        assert!(FOOTER_HINTS_EDITING.contains(&(EditingAction::RunAgent, "w workflow")));
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
    fn an_acknowledgement_lists_each_named_dismissal_key() {
        // `Esc / Enter` is one answer string, so checking only its leading token
        // would leave the reflex key free to disappear while the dialog still
        // looked answerable. Both names must reach the set that lists them.
        let listed = dialog_answers(
            Answers::Acknowledge,
            AnswerWords {
                affirmative: None,
                dismissal: "dismiss",
            },
        );
        assert_eq!(listed, vec!["Esc / Enter dismiss"]);
        for key in [key_named("Esc"), key_named("Enter")] {
            assert_eq!(
                action_for(key, Mode::Dialog(Answers::Acknowledge)),
                Some(Action::Unwind),
                "{key:?} is listed but not admitted"
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
