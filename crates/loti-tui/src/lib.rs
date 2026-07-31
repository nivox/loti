//! `loti-tui` — the full-screen browser for a loti store.
//!
//! A file-browser view of the tracker: epics are the top level, entering one
//! lists its tickets, entering a ticket lists its subtickets, and a preview pane
//! shows the same document `loti epic show` / `loti ticket show` print.
//!
//! An epic's and a ticket's own meta — its labels, comments, dependencies and
//! assets — are rows on that same level, so a comment or a blocker is reached by
//! the same two keys as a subticket rather than by a vocabulary of its own.
//!
//! The crate owns the terminal and nothing else. Store access lives in
//! [`data`], the position in [`nav`], the state machine in [`app`], and drawing
//! in [`ui`], so none of those needs a terminal to be exercised.
//!
//! # What ends a session
//!
//! A browser is most useful on a store that cannot be read in full, so **nothing
//! the store does to a read ends a running session**. A member the store lists and
//! the browser cannot read is a row that says so; a document that cannot be built
//! says so in the preview pane; a level that cannot be listed at all leaves the
//! reader where they were and reports why. A corrupt or partly-missing store is
//! browsed, not exited.
//!
//! Two classes remain, and neither of them is the store's contents:
//!
//! * **Before the first frame** the browser refuses to open, saying why in
//!   ordinary output: no store here, a format this binary cannot read, or an epic
//!   roster that cannot be listed. There is no screen yet to report into, and
//!   nothing to browse.
//! * **The terminal itself** — drawing, reading input, giving the screen to an
//!   external editor and taking it back. A browser that cannot use the terminal
//!   cannot tell the reader anything, so it stops and lets the failure be printed.

pub mod action;
pub mod app;
pub mod data;
pub mod keymap;
pub mod nav;
pub mod theme;
pub mod ui;

use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::panic;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEvent, KeyEventKind, MouseButton,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::Terminal;

use crate::action::Mode;
use crate::app::App;
use crate::theme::Theme;

/// Browse the store for the current directory, or the one at `root`.
///
/// The store is opened — and a format this binary cannot read is refused —
/// before the terminal is touched, so a failure leaves the screen as it was.
pub fn run(root: Option<&Path>) -> Result<()> {
    if !io::stdout().is_terminal() {
        bail!("the browser needs an interactive terminal; it cannot be piped or redirected");
    }
    let store = data::open(root)?;
    let app = App::new(store, Theme::from_env())?;

    let mut terminal = enter()?;
    let outcome = event_loop(&mut terminal, app);
    leave(&mut terminal)?;
    outcome
}

type Tui = Terminal<CrosstermBackend<io::Stdout>>;

/// Take over the terminal, and make sure a panic gives it back.
///
/// The release profile aborts on panic, so there is no unwinding to clean up
/// behind us: the hook is the only chance to restore the terminal, and it must
/// run before the default hook reports the panic — otherwise the report is
/// printed into a raw-mode alternate screen and lost.
fn enter() -> Result<Tui> {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

/// Give the terminal back.
fn leave(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Best-effort restore for the panic path, where errors can no longer be
/// reported and a half-restored terminal is worse than a silent failure.
fn restore() {
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
    let _ = stdout.flush();
}

/// One way the browser holds the terminal, and whether it is holding it.
///
/// Named as data because an external editor inherits the terminal: whatever the
/// browser is still holding is something the editor's own input has to fight, so
/// what is let go of has to be a rule that can be read off rather than a sequence
/// buried in a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    /// The screen the browser draws on, which the editor must not draw over.
    AlternateScreen(bool),
    /// Keys delivered unprocessed, which an editor sets up for itself.
    RawMode(bool),
    /// Mouse reports delivered as events. An editor that inherits capture reads
    /// those reports as input and what the reader typed is corrupted.
    MouseCapture(bool),
}

/// Everything let go of before an external editor runs: the alternate screen, raw
/// mode **and mouse capture**, none of which an editor can work around.
const RELEASED_FOR_THE_EDITOR: &[Hold] = &[
    Hold::MouseCapture(false),
    Hold::AlternateScreen(false),
    Hold::RawMode(false),
];

/// Everything taken back once the editor has exited, in the order the browser
/// takes it at startup.
///
/// Invariant: exactly what [`RELEASED_FOR_THE_EDITOR`] gave up, so a round-trip
/// cannot leave the browser running with less of the terminal than it began with.
const RECLAIMED_AFTER_THE_EDITOR: &[Hold] = &[
    Hold::RawMode(true),
    Hold::AlternateScreen(true),
    Hold::MouseCapture(true),
];

/// The terminal work an external-editor round-trip has done on its behalf.
///
/// A seam rather than direct calls, so what the round-trip performs — and in which
/// order — can be read off and checked: the browser's implementation drives the
/// real terminal, and a test's records what it was asked for.
trait Handoff {
    /// Let go of, or take back, one part of the terminal.
    fn hold(&mut self, step: Hold) -> Result<()>;

    /// Repaint from scratch. The editor drew over the screen it was given, so
    /// nothing the browser last drew can be assumed to still be there and a frame
    /// diffed against it would leave the leftovers on screen.
    fn repaint(&mut self) -> Result<()>;
}

/// A terminal the round-trip performs on for real. Generic over the backend and
/// over where the control sequences go, because neither can be looked at on the
/// browser's own terminal — and an implementation nothing can look at is one that
/// can be gutted while a seam's own tests stay green.
struct Screen<'t, B: Backend, W: Write>(&'t mut Terminal<B>, W);

impl<B: Backend, W: Write> Handoff for Screen<'_, B, W> {
    fn hold(&mut self, step: Hold) -> Result<()> {
        hold_step(step, &mut self.1)
    }

    fn repaint(&mut self) -> Result<()> {
        self.0.clear()?;
        Ok(())
    }
}

/// Let go of, or take back, one part of the terminal, writing whatever control
/// sequences that takes to `out`.
///
/// Raw mode is the one step that writes nothing: it is a mode of the terminal
/// device rather than a sequence sent to it, so it is set through the device and
/// not through `out`.
fn hold_step(step: Hold, out: &mut impl Write) -> Result<()> {
    match step {
        Hold::AlternateScreen(true) => execute!(out, EnterAlternateScreen)?,
        Hold::AlternateScreen(false) => execute!(out, LeaveAlternateScreen)?,
        Hold::RawMode(true) => enable_raw_mode()?,
        Hold::RawMode(false) => disable_raw_mode()?,
        Hold::MouseCapture(true) => execute!(out, EnableMouseCapture)?,
        Hold::MouseCapture(false) => execute!(out, DisableMouseCapture)?,
    }
    Ok(())
}

/// Give the terminal away, run `editor`, and take the terminal back.
///
/// Invariant: every part named by [`RELEASED_FOR_THE_EDITOR`] is let go of before
/// the editor runs, one step per part, and every part named by
/// [`RECLAIMED_AFTER_THE_EDITOR`] is taken back afterwards **whatever the editor
/// did** — which is why the editor's outcome is carried past the reclaim rather
/// than propagated at once: a failure that returned early would leave the browser
/// drawing with raw mode off onto a screen it no longer owns.
fn around_the_editor<T>(
    handoff: &mut impl Handoff,
    editor: impl FnOnce() -> Result<T>,
) -> Result<T> {
    for step in RELEASED_FOR_THE_EDITOR {
        handoff.hold(*step)?;
    }
    let outcome = editor();
    for step in RECLAIMED_AFTER_THE_EDITOR {
        handoff.hold(*step)?;
    }
    handoff.repaint()?;
    outcome
}

/// Hand `text` to the reader's editor and bring back what they saved, or `None`
/// where the editor exited unsuccessfully — which is how an editor says the edit
/// was abandoned, and leaves the buffer as it was.
fn edit_externally(terminal: &mut Tui, text: &str) -> Result<Option<String>> {
    // Looked up before anything is given away: a setting that is not there is not
    // worth blanking the reader's screen for.
    let editor = editor_setting(env::var("VISUAL").ok(), env::var("EDITOR").ok())?;
    around_the_editor(&mut Screen(terminal, io::stdout()), || {
        run_editor(&editor, text)
    })
}

/// The editor the reader asked for, out of the two settings that can name one.
///
/// `visual` wins where both name an editor, and a setting holding nothing but
/// whitespace counts as unset — that is what every tool a reader compares this to
/// does with a variable that is exported empty, so refusing on one would be a
/// difference they cannot see on screen.
///
/// Neither set is refused rather than guessed at: an editor chosen on the reader's
/// behalf could be one they cannot get out of, and this hands over the whole
/// terminal.
fn editor_setting(visual: Option<String>, editor: Option<String>) -> Result<String> {
    [visual, editor]
        .into_iter()
        .flatten()
        .find(|setting| !setting.trim().is_empty())
        .ok_or_else(|| anyhow!("no editor is set: set EDITOR (or VISUAL) to the editor you want"))
}

/// Run `editor` over a file holding `text`, and read back what it saved.
fn run_editor(editor: &str, text: &str) -> Result<Option<String>> {
    // An editor setting may carry flags, so the first word is the program and the
    // rest are arguments. Quoting is not honoured: a path with spaces in it belongs
    // in a wrapper script rather than in a variable this splits.
    let mut words = editor.split_whitespace();
    let program = words
        .next()
        .ok_or_else(|| anyhow!("the editor setting is blank"))?;

    // A fixed suffix, because an editor picks its mode from the name and a random
    // temp name would leave it guessing. Markdown is the syntax of the long-form
    // fields this round-trip exists for; a single-line field carries no markup and
    // loses nothing by being shown in that mode.
    let file = tempfile::Builder::new()
        .prefix("loti-")
        .suffix(".md")
        .tempfile()
        .context("creating the file to hand to the editor")?;
    fs::write(file.path(), text).context("writing the text for the editor")?;

    let status = Command::new(program)
        .args(words)
        .arg(file.path())
        .status()
        .with_context(|| format!("running the editor {program}"))?;
    if !status.success() {
        // An editor that exits unsuccessfully has abandoned the edit — `:cq` is how
        // that is said — so what it left in the file is not what the reader meant.
        return Ok(None);
    }
    let edited = fs::read_to_string(file.path()).context("reading the text back")?;
    Ok(Some(edited))
}

/// A failure together with every cause under it.
///
/// The outermost context names only what the browser was attempting — which editor
/// it tried to run — and the cause beneath it is the part the reader can act on: a
/// program that is not installed, a name with a typo in it, a file that could not be
/// written. A dialog is raised only for something that must be acted on, so it
/// carries the whole chain rather than the attempt alone.
fn failure_and_causes(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

/// Take an intent's outcome back to the reader, and say whether the session ends.
///
/// A store the browser cannot fully read is reported and browsed on: a level that
/// could not be listed leaves the reader on the level they were already looking
/// at, with a dialog naming what failed. This is the one place a failure could
/// have ended the session, so it is the one place the rule is applied — see the
/// crate documentation for the classes that do end one.
fn intent_outcome(app: &mut App, outcome: Result<bool>) -> bool {
    match outcome {
        Ok(exit) => exit,
        Err(e) => {
            app.store_unreadable(failure_and_causes(&e));
            false
        }
    }
}

/// Take an editor round-trip's outcome back to the surface it was called from.
///
/// A failure to run an editor is reported to the reader rather than ending the
/// session: the buffer it was called from is still open and still theirs.
fn editor_outcome(app: &mut App, outcome: Result<Option<String>>) {
    match outcome {
        Ok(Some(edited)) => app.editor_returned(&edited),
        // The editor abandoned the edit, so the buffer keeps what it had.
        Ok(None) => {}
        Err(e) => app.editor_failed(failure_and_causes(&e)),
    }
}

/// How often the loop wakes with no input to read.
///
/// A persistent tick is what lets anything timed appear at all: waiting on input
/// alone, a message that clears itself has no wakeup to clear it on. Coarse
/// enough to cost nothing, fine enough that a timed change never overstays its
/// deadline by more than a quarter second.
const TICK: Duration = Duration::from_millis(250);

/// The action a key press carries to [`App::apply`]: the key map's own intent for
/// this key and mode where it names one, and [`action::Action::Unbound`] where it
/// does not.
///
/// This is the whole of the conversion the event-loop boundary owns, and it is
/// what lets an ignored key reach application handling as a decision of its own,
/// rather than being dropped here — before the state machine that owns the
/// context a silence answers for is ever consulted. [`keymap::action_for`] keeps
/// its `Option` return, so a key-table test still asks it directly whether a key
/// is bound at all; this is the one place that turns "not bound" into an action.
fn dispatch(key: KeyEvent, mode: Mode) -> action::Action {
    keymap::action_for(key, mode).unwrap_or(action::Action::Unbound)
}

fn event_loop(terminal: &mut Tui, mut app: App) -> Result<()> {
    // Mouse capture is what makes the divider draggable, but it also takes
    // click-drag text selection away from the terminal. Zoom is the way out:
    // while zoomed there is no divider to drag, so capture is released and
    // selecting text from the preview works again.
    let mut captured = true;
    loop {
        // Every pass, not only the passes where the wait below timed out: that
        // wait is re-armed by every event, so a timed notice checked there alone
        // would overstay under a sustained stream of them. The sweep is what asks
        // for the frame that brings the hint strip back.
        app.expire_flash();

        if app.take_redraw_request() {
            terminal.draw(|f| ui::draw(f, &mut app))?;
        }

        let wanted = !app.zoomed();
        if wanted != captured {
            let mut stdout = io::stdout();
            if wanted {
                execute!(stdout, EnableMouseCapture)?;
            } else {
                execute!(stdout, DisableMouseCapture)?;
            }
            captured = wanted;
        }

        // A tick with nothing to read is the only wakeup that leaves the frame
        // to the request. Every real event — key, mouse, resize alike — asks for
        // one before dispatch, so no handler can leave the screen stale behind
        // the reader's own input.
        if !event::poll(TICK)? {
            continue;
        }
        app.request_redraw();

        match event::read()? {
            // Key repeats and releases would otherwise apply an action several
            // times on the terminals that report them.
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                // Any key retires a live notice, bound or not: the reader has
                // moved on, and a notice's lifetime is a maximum. Before
                // dispatch, so a key that raises one of its own keeps it.
                app.clear_flash();
                // The mode is an input to the mapping, so one key can be the way
                // out of a mode here and the way out of the browser there. Every
                // key reaches `apply`, bound or not — see `dispatch`.
                let action = dispatch(key, app.mode());
                let outcome = app.apply(action);
                if intent_outcome(&mut app, outcome) {
                    return Ok(());
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    app.press(mouse.column);
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    let width = terminal.size()?.width;
                    app.drag(mouse.column, width);
                }
                MouseEventKind::Up(MouseButton::Left) => app.release(),
                MouseEventKind::ScrollDown => {
                    let outcome = app.apply(action::Action::CursorDown);
                    intent_outcome(&mut app, outcome);
                }
                MouseEventKind::ScrollUp => {
                    let outcome = app.apply(action::Action::CursorUp);
                    intent_outcome(&mut app, outcome);
                }
                _ => {}
            },
            _ => {}
        }

        // Only the loop owns the terminal, so an editor can only be run from here.
        if let Some(text) = app.take_editor_handoff() {
            editor_outcome(&mut app, edit_externally(terminal, &text));
            // The terminal was handed back with everything reclaimed, whatever the
            // divider state was before, and the whole screen has to be repainted.
            captured = true;
            app.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;

    use crate::action::Action;
    use crate::app::Modal;

    use super::*;

    /// Which part of the terminal a hold is about, without whether it is held: two
    /// lists are compared for covering the same parts, not the same states.
    fn part(hold: Hold) -> &'static str {
        match hold {
            Hold::AlternateScreen(_) => "alternate screen",
            Hold::RawMode(_) => "raw mode",
            Hold::MouseCapture(_) => "mouse capture",
        }
    }

    fn held(hold: Hold) -> bool {
        match hold {
            Hold::AlternateScreen(held) | Hold::RawMode(held) | Hold::MouseCapture(held) => held,
        }
    }

    #[test]
    fn an_external_editor_is_given_the_whole_terminal_and_handed_it_all_back() {
        // Mouse capture is the one a reader would blame on their editor rather than
        // on the browser: an editor that inherits it reads the terminal's mouse
        // reports as typed input. So it is named here beside the other two.
        for released in [
            Hold::MouseCapture(false),
            Hold::AlternateScreen(false),
            Hold::RawMode(false),
        ] {
            assert!(
                RELEASED_FOR_THE_EDITOR.contains(&released),
                "{released:?} is still held while the editor runs"
            );
        }
        // A release list that held something, or a reclaim list that released
        // something, would be a handover in name only.
        assert!(RELEASED_FOR_THE_EDITOR.iter().copied().all(|h| !held(h)));
        assert!(RECLAIMED_AFTER_THE_EDITOR.iter().copied().all(held));

        // The same parts both ways: a round-trip cannot leave the browser running
        // with less of the terminal than it began with.
        let mut released: Vec<&str> = RELEASED_FOR_THE_EDITOR.iter().copied().map(part).collect();
        let mut reclaimed: Vec<&str> = RECLAIMED_AFTER_THE_EDITOR
            .iter()
            .copied()
            .map(part)
            .collect();
        released.sort_unstable();
        reclaimed.sort_unstable();
        assert_eq!(released, reclaimed);
        // And no part named twice, so neither list can cover a missing part by
        // repeating another one.
        released.dedup();
        assert_eq!(released.len(), RELEASED_FOR_THE_EDITOR.len());
    }

    /// One thing a round-trip asked of the terminal, or of the editor.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Asked {
        Hold(Hold),
        Editor,
        Repaint,
    }

    /// A terminal that performs nothing and remembers everything, so the order the
    /// round-trip does things in is a value a test can look at.
    #[derive(Clone, Default)]
    struct Recorder(Rc<RefCell<Vec<Asked>>>);

    impl Recorder {
        fn note(&self, asked: Asked) {
            self.0.borrow_mut().push(asked);
        }

        fn asked(&self) -> Vec<Asked> {
            self.0.borrow().clone()
        }

        fn at(&self, asked: &Asked) -> usize {
            self.asked()
                .iter()
                .position(|other| other == asked)
                .unwrap_or_else(|| panic!("{asked:?} never happened: {:?}", self.asked()))
        }
    }

    impl Handoff for Recorder {
        fn hold(&mut self, step: Hold) -> Result<()> {
            self.note(Asked::Hold(step));
            Ok(())
        }

        fn repaint(&mut self) -> Result<()> {
            self.note(Asked::Repaint);
            Ok(())
        }
    }

    /// Everything taken back after the editor, ending in the repaint.
    fn reclaiming() -> Vec<Asked> {
        RECLAIMED_AFTER_THE_EDITOR
            .iter()
            .copied()
            .map(Asked::Hold)
            .chain([Asked::Repaint])
            .collect()
    }

    #[test]
    fn the_editor_runs_with_none_of_the_terminal_held_and_every_part_comes_back() {
        let terminal = Recorder::default();
        let editor = terminal.clone();
        let saved = around_the_editor(&mut terminal.clone(), || {
            editor.note(Asked::Editor);
            Ok("saved")
        })
        .unwrap();
        assert_eq!(saved, "saved");

        let ran = terminal.at(&Asked::Editor);
        // Anything still held is something the editor's own input has to fight: an
        // inherited mouse capture is read as typed characters, and an editor drawing
        // inside the alternate screen with raw mode on is the corruption the
        // round-trip exists to avoid.
        for step in RELEASED_FOR_THE_EDITOR {
            assert!(
                terminal.at(&Asked::Hold(*step)) < ran,
                "{step:?} was still held while the editor ran"
            );
        }
        // And nothing is taken back while the editor is still using it.
        for step in RECLAIMED_AFTER_THE_EDITOR {
            assert!(
                terminal.at(&Asked::Hold(*step)) > ran,
                "{step:?} was taken back before the editor was done with it"
            );
        }
        // The repaint comes last, once the alternate screen is back: a frame diffed
        // against what the editor left on screen keeps the editor's leftovers.
        assert!(
            terminal.at(&Asked::Repaint) > terminal.at(&Asked::Hold(Hold::AlternateScreen(true)))
        );

        // One step per part, and no step twice: a part released twice would hide a
        // part never released at all.
        let expected: Vec<Asked> = RELEASED_FOR_THE_EDITOR
            .iter()
            .copied()
            .map(Asked::Hold)
            .chain([Asked::Editor])
            .chain(reclaiming())
            .collect();
        assert_eq!(terminal.asked(), expected);
    }

    #[test]
    fn an_editor_that_could_not_run_still_gets_the_whole_terminal_handed_back() {
        let terminal = Recorder::default();
        let editor = terminal.clone();
        let failure = around_the_editor(&mut terminal.clone(), || {
            editor.note(Asked::Editor);
            Err::<(), _>(anyhow!("no editor is set"))
        })
        .expect_err("the editor's failure is the round-trip's failure");
        assert!(
            failure.to_string().contains("no editor is set"),
            "{failure}"
        );

        // This is the reachable path — an editor that is not installed, a temp file
        // that could not be written — so it is the one that must not leave a
        // half-given-away terminal: the outcome is reported only after everything is
        // taken back, never instead of taking it back.
        let asked = terminal.asked();
        let ran = terminal.at(&Asked::Editor);
        assert_eq!(asked[ran + 1..], reclaiming()[..]);
    }

    #[test]
    fn taking_the_screen_back_repaints_it_rather_than_drawing_over_the_editor() {
        // What the editor drew is still there when the browser gets the screen back,
        // so the repaint throws the whole screen away: a frame diffed against a
        // buffer the editor overwrote leaves the editor's own lines showing.
        let mut terminal = Terminal::new(TestBackend::new(12, 1)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(Paragraph::new("leftovers"), frame.area()))
            .unwrap();
        let on_screen = |terminal: &Terminal<TestBackend>| -> String {
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect()
        };
        assert!(on_screen(&terminal).contains("leftovers"));

        Screen(&mut terminal, Vec::new()).repaint().unwrap();
        assert!(
            !on_screen(&terminal).contains("leftovers"),
            "the screen the editor drew on was kept: {:?}",
            on_screen(&terminal)
        );
    }

    #[test]
    fn each_part_of_the_screen_is_let_go_of_and_taken_back_by_a_sequence_of_its_own() {
        // Raw mode is absent on purpose: it is a mode of the terminal device rather
        // than a sequence sent to it, and a test has no terminal device to set.
        let mut sent: Vec<(Hold, Vec<u8>)> = Vec::new();
        for step in [
            Hold::AlternateScreen(false),
            Hold::AlternateScreen(true),
            Hold::MouseCapture(false),
            Hold::MouseCapture(true),
        ] {
            let mut out = Vec::new();
            hold_step(step, &mut out).unwrap();
            // A step that writes nothing leaves that part exactly as it was while
            // the screen goes on looking right.
            assert!(!out.is_empty(), "{step:?} sent the terminal nothing");
            for (other, sequence) in &sent {
                assert_ne!(*sequence, out, "{step:?} sends what {other:?} sends");
            }
            sent.push((step, out));
        }

        // And the terminal the browser really performs on sends exactly that, for
        // every step: a seam whose own implementation is unobservable can be gutted
        // while the tests that read the seam stay green.
        let mut terminal = Terminal::new(TestBackend::new(4, 1)).unwrap();
        for (step, sequence) in &sent {
            let mut screen = Screen(&mut terminal, Vec::new());
            screen.hold(*step).unwrap();
            assert_eq!(
                &screen.1, sequence,
                "the real terminal does not perform {step:?}"
            );
        }

        // Polarity, against the terminal protocol rather than against the code that
        // implements it: releasing a part must send what turns it off, and taking it
        // back what turns it on. Inverted, the round-trip would enable mouse capture
        // on the way into the editor — whose input is then read as typed characters.
        let sequence = |step: Hold| -> String {
            let mut out = Vec::new();
            hold_step(step, &mut out).unwrap();
            String::from_utf8(out).expect("a control sequence is text")
        };
        assert!(sequence(Hold::AlternateScreen(true)).ends_with("h"));
        assert!(sequence(Hold::AlternateScreen(false)).ends_with("l"));
        assert!(sequence(Hold::MouseCapture(true)).ends_with("h"));
        assert!(sequence(Hold::MouseCapture(false)).ends_with("l"));
    }

    #[test]
    fn what_the_editor_saved_reaches_the_field_and_an_abandoned_edit_leaves_it_alone() {
        // What a field of text holds. A picker is a failure rather than an empty
        // answer: an editor's result must not be checked against a field nothing can
        // be typed into.
        fn text_of(field: &app::Field) -> &str {
            match field.shown() {
                app::Shown::Text { value, .. } => value,
                app::Shown::Pick { .. } => panic!("a picker holds no text"),
            }
        }
        // Driven through the wiring the loop uses, not through the surface's own
        // method: an outcome arm that drops the reader's text loses it silently, and
        // the surface cannot tell the difference between a blank return and none.
        let fx = data::fixture::Fixture::build();
        let open = |fx: &data::fixture::Fixture| {
            let mut app = App::new(fx.store.clone(), Theme::with_color(false)).unwrap();
            app.apply(Action::Descend).unwrap();
            app.apply(Action::EnterEditing).unwrap();
            app.apply(Action::Add).unwrap();
            app
        };

        let mut app = open(&fx);
        editor_outcome(&mut app, Ok(Some("carried".into())));
        let surface = app.surface().expect("the buffer is still open");
        assert_eq!(text_of(&surface.fields()[surface.focus()]), "carried");
        // Text arriving from the editor is a change like any other, so leaving
        // without saving has to warn rather than throw it away in silence.
        assert!(surface.fields()[surface.focus()].is_dirty());

        // An editor that exited unsuccessfully is how a reader abandons an edit, so
        // the buffer keeps exactly what it had — including having been untouched.
        let mut app = open(&fx);
        editor_outcome(&mut app, Ok(None));
        let surface = app.surface().expect("the buffer is still open");
        assert_eq!(text_of(&surface.fields()[surface.focus()]), "");
        assert!(!surface.fields()[surface.focus()].is_dirty());
        assert!(app.modal().is_none(), "{:?}", app.modal());
    }

    #[test]
    fn the_editor_setting_prefers_visual_and_counts_a_blank_one_as_unset() {
        let setting = |visual: Option<&str>, editor: Option<&str>| {
            editor_setting(visual.map(String::from), editor.map(String::from))
        };

        // `VISUAL` names the full-screen editor and `EDITOR` the line editor to fall
        // back to, and this hands over the whole screen: so `VISUAL` wins wherever
        // both name one.
        assert_eq!(setting(Some("nvim"), Some("ed")).unwrap(), "nvim");
        assert_eq!(setting(None, Some("ed")).unwrap(), "ed");
        assert_eq!(setting(Some("nvim"), None).unwrap(), "nvim");

        // A variable exported empty is what the tools a reader compares this to
        // treat as unset, so it falls through to the other one instead of refusing.
        assert_eq!(setting(Some(""), Some("ed")).unwrap(), "ed");
        assert_eq!(setting(Some("   "), Some("ed")).unwrap(), "ed");

        // With neither naming an editor it is refused rather than guessed at: an
        // editor chosen on the reader's behalf could be one they cannot get out of,
        // and the refusal has to say which variables to set.
        for (visual, editor) in [(None, None), (Some(""), Some("   "))] {
            let refusal = setting(visual, editor)
                .expect_err("an editor is never chosen for the reader")
                .to_string();
            assert!(refusal.contains("EDITOR"), "{refusal}");
            assert!(refusal.contains("VISUAL"), "{refusal}");
        }
    }

    #[test]
    fn a_level_that_cannot_be_listed_is_reported_and_the_session_goes_on() {
        // Driven through the loop's own wiring rather than through the state
        // machine's method: this is the boundary an intent's failure used to leave
        // the session by, so the claim is about what the boundary does with one.
        let fx = data::fixture::Fixture::build();
        let mut app = App::new(fx.store.clone(), Theme::with_color(false)).unwrap();
        // The epic is on screen and its own file goes underneath the browser: its
        // level can no longer be listed at all, which is the failure a row of its
        // own cannot report.
        fx.remove_the_epics_file();
        // Derived from the seam, not from the boundary under test, so a message
        // built from something else than the failure fails this.
        let expected = failure_and_causes(
            &data::rows(&fx.store, &data::Level::Epic(fx.epic.clone()))
                .expect_err("the epic's own file is gone"),
        );

        let outcome = app.apply(Action::Descend);
        assert!(
            outcome.is_err(),
            "the level listed without the epic's file: nothing here is being tested"
        );
        assert!(
            !intent_outcome(&mut app, outcome),
            "a store that cannot be read in full ended the session"
        );

        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("nothing said why the level did not open: {:?}", app.modal())
        };
        assert_eq!(dialog.message(), expected);
        // And the reader is left where they were, on the level that still lists.
        assert_eq!(app.nav().crumbs(), vec!["epics"]);
    }

    #[test]
    fn a_failed_editor_tells_the_reader_why_in_the_dialog_it_raises() {
        let (_dir, store) = data::fixture::empty_store();
        let mut app = App::new(store, Theme::with_color(false)).unwrap();

        // The likeliest failure by far is a setting with a typo in it or an editor
        // that is not installed. What the browser was attempting names the program
        // and nothing else; only the cause under it says what went wrong, and a
        // dialog is reserved for what the reader has to act on.
        let failure = run_editor("loti-no-such-editor", "kept")
            .expect_err("a missing program is not a saved edit");
        let cause = failure
            .chain()
            .last()
            .expect("the system said why")
            .to_string();
        assert_ne!(
            cause,
            failure.to_string(),
            "this failure carries no cause, so it cannot show one reaching the dialog"
        );

        editor_outcome(&mut app, run_editor("loti-no-such-editor", "kept"));
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("a failed editor said nothing: {:?}", app.modal())
        };
        assert!(
            dialog.message().contains("loti-no-such-editor"),
            "{dialog:?}"
        );
        assert!(dialog.message().contains(&cause), "{dialog:?}");
    }

    #[test]
    fn an_editor_gets_the_text_in_a_file_and_what_it_saved_comes_back() {
        // A file is the only interface every editor has, and `touch` is an editor
        // that saves what it was given: what comes back is what went in, so the text
        // really made the round trip rather than being re-read from the buffer.
        assert_eq!(
            run_editor("touch", "a line\n").unwrap().as_deref(),
            Some("a line\n")
        );

        // A setting may carry flags, and they reach the editor: this one empties the
        // file, which is a reader deleting everything and saving.
        assert_eq!(
            run_editor("truncate --size 0", "gone").unwrap().as_deref(),
            Some("")
        );

        // An editor that exits unsuccessfully has abandoned the edit, so nothing
        // comes back and the buffer keeps what it had.
        assert_eq!(run_editor("false", "kept").unwrap(), None);

        // An editor that cannot be run at all is a failure to report, not an empty
        // result that would silently blank the field.
        let failure = run_editor("loti-no-such-editor", "kept")
            .expect_err("a missing program is not a saved edit")
            .to_string();
        assert!(failure.contains("loti-no-such-editor"), "{failure}");
    }

    #[test]
    fn a_blank_editor_setting_is_refused_rather_than_guessed_at() {
        // An editor chosen on the reader's behalf could be one they cannot get out
        // of, and this hands over the whole terminal.
        let refusal = run_editor("   ", "kept")
            .expect_err("a blank setting names no program")
            .to_string();
        assert!(refusal.contains("blank"), "{refusal}");
    }

    #[test]
    fn a_key_the_table_does_not_bind_dispatches_as_the_unbound_action() {
        // No terminal is needed: `dispatch` is the whole of the boundary's own
        // decision, so it is asked directly rather than through a loop iteration.
        // `keymap::action_for` keeps saying `None` for a key it does not bind —
        // that table is exhaustively swept in `keymap`'s own tests — and this is
        // the one place that turns `None` into an action the state machine sees.
        let unbound = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        for mode in [Mode::Browse, Mode::Editing] {
            assert_eq!(keymap::action_for(unbound, mode), None, "{mode:?}");
            assert_eq!(dispatch(unbound, mode), Action::Unbound, "{mode:?}");
        }

        // A key the table does bind still carries its own intent through —
        // dispatch adds a case, it does not replace the map's answer.
        let bound = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(keymap::action_for(bound, Mode::Browse), Some(Action::Quit));
        assert_eq!(dispatch(bound, Mode::Browse), Action::Quit);
    }
}
