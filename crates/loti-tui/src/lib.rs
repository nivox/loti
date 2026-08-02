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

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::panic;
use std::path::Path;
use std::process::{Command, Stdio};
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

    let mut stdout = io::stdout();
    let mut raw_mode = CrosstermRawMode;
    for step in HELD_BY_BROWSER {
        hold_step(*step, &mut stdout, &mut raw_mode)?;
    }
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

/// Give the terminal back.
fn leave(terminal: &mut Tui) -> Result<()> {
    CrosstermRawMode.set(false)?;
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
    let _ = CrosstermRawMode.set(false);
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

/// Everything let go of before an external process runs: the alternate screen,
/// raw mode **and mouse capture**, none of which an inherited-stdio child can
/// work around.
const RELEASED_FOR_EXTERNAL_PROCESS: &[Hold] = &[
    Hold::MouseCapture(false),
    Hold::AlternateScreen(false),
    Hold::RawMode(false),
];

/// Everything the browser holds after startup, in the order it takes each part.
///
/// This is also the reclaim order after an external process. One list decides
/// both boundaries, so the browser cannot start in one terminal state and return
/// from an editor in another by letting two orderings drift apart.
const HELD_BY_BROWSER: &[Hold] = &[
    Hold::RawMode(true),
    Hold::AlternateScreen(true),
    Hold::MouseCapture(true),
];

/// The terminal work an external-process handoff has done on its behalf.
///
/// A seam rather than direct calls, so what the handoff performs — and in which
/// order — can be read off and checked: the browser's implementation drives the
/// real terminal, and a test's records what it was asked for.
trait TerminalHandoff {
    /// Let go of, or take back, one part of the terminal.
    fn hold(&mut self, step: Hold) -> Result<()>;

    /// Repaint from scratch. The editor drew over the screen it was given, so
    /// nothing the browser last drew can be assumed to still be there and a frame
    /// diffed against it would leave the leftovers on screen.
    fn repaint(&mut self) -> Result<()>;
}

/// The terminal device's raw-mode switch.
///
/// Control sequences make the other handoff steps visible in the output sink.
/// Raw mode changes the device instead, so it has its own seam: without it, a
/// round-trip could forget to restore raw mode while every observable sequence
/// still looked correct.
trait RawMode {
    fn set(&mut self, enabled: bool) -> Result<()>;
}

/// The browser's real raw-mode switch.
struct CrosstermRawMode;

impl RawMode for CrosstermRawMode {
    fn set(&mut self, enabled: bool) -> Result<()> {
        if enabled {
            enable_raw_mode()?;
        } else {
            disable_raw_mode()?;
        }
        Ok(())
    }
}

/// A terminal the round-trip performs on for real. Generic over the backend,
/// output sink, and raw-mode device so each externally visible handoff effect is
/// exercised by the same implementation production uses.
struct Screen<'t, B: Backend, W: Write, R: RawMode = CrosstermRawMode> {
    terminal: &'t mut Terminal<B>,
    output: W,
    raw_mode: R,
}

impl<'t, B: Backend, W: Write> Screen<'t, B, W> {
    fn new(terminal: &'t mut Terminal<B>, output: W) -> Self {
        Self::with_raw_mode(terminal, output, CrosstermRawMode)
    }
}

impl<'t, B: Backend, W: Write, R: RawMode> Screen<'t, B, W, R> {
    fn with_raw_mode(terminal: &'t mut Terminal<B>, output: W, raw_mode: R) -> Self {
        Self {
            terminal,
            output,
            raw_mode,
        }
    }
}

impl<B: Backend, W: Write, R: RawMode> TerminalHandoff for Screen<'_, B, W, R> {
    fn hold(&mut self, step: Hold) -> Result<()> {
        hold_step(step, &mut self.output, &mut self.raw_mode)
    }

    fn repaint(&mut self) -> Result<()> {
        self.terminal.clear()?;
        Ok(())
    }
}

/// Let go of, or take back, one part of the terminal, writing whatever control
/// sequences that takes to `out` and changing raw mode through its device seam.
fn hold_step(step: Hold, out: &mut impl Write, raw_mode: &mut impl RawMode) -> Result<()> {
    match step {
        Hold::AlternateScreen(true) => execute!(out, EnterAlternateScreen)?,
        Hold::AlternateScreen(false) => execute!(out, LeaveAlternateScreen)?,
        Hold::RawMode(enabled) => raw_mode.set(enabled)?,
        Hold::MouseCapture(true) => execute!(out, EnableMouseCapture)?,
        Hold::MouseCapture(false) => execute!(out, DisableMouseCapture)?,
    }
    Ok(())
}

/// Give the terminal away, run one inherited-stdio process, and take it back.
///
/// Invariant: every part named by [`RELEASED_FOR_EXTERNAL_PROCESS`] is let go of
/// before the process runs, one step per part, and every part named by
/// [`HELD_BY_BROWSER`] is taken back afterwards **whatever the process did** —
/// which is why its outcome is carried past the reclaim rather than propagated at
/// once: a failure that returned early would leave the browser drawing with raw
/// mode off onto a screen it no longer owns.
fn around_external_process<T>(
    handoff: &mut impl TerminalHandoff,
    process: impl FnOnce() -> Result<T>,
) -> Result<T> {
    for step in RELEASED_FOR_EXTERNAL_PROCESS {
        handoff.hold(*step)?;
    }
    let outcome = process();
    reclaim_after_external_process(handoff)?;
    outcome
}

/// Restore every terminal part even if an earlier restore step failed.
///
/// The first restoration failure remains the result because the browser cannot
/// safely continue without the terminal it owns, but no later part is abandoned
/// while reporting it. Repaint is part of that best effort too: the editor may
/// have drawn over the ordinary screen even when one device operation failed.
fn reclaim_after_external_process(handoff: &mut impl TerminalHandoff) -> Result<()> {
    let mut failure = None;
    for step in HELD_BY_BROWSER {
        if let Err(error) = handoff.hold(*step) {
            if failure.is_none() {
                failure = Some(error);
            }
        }
    }
    if let Err(error) = handoff.repaint() {
        if failure.is_none() {
            failure = Some(error);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The normalized outcome of a prepared foreground child. A child status is kept
/// as the operating system describes it, which also names a signal termination.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChildOutcome {
    ZeroExit,
    NonZeroExit(String),
    SpawnError(String),
}

/// The child-running half of the external-process seam. The production runner
/// executes the prepared direct plan; a recorder can prove the exact plan passed
/// across the terminal handoff without starting a real process.
trait ChildRunner {
    fn run(&mut self, plan: &loti_core::launch::LaunchPlan) -> ChildOutcome;
}

/// The production direct child runner. `status` inherits stdin, stdout and stderr
/// explicitly: an interactive agent receives the terminal the browser released.
struct DirectChild;

impl ChildRunner for DirectChild {
    fn run(&mut self, plan: &loti_core::launch::LaunchPlan) -> ChildOutcome {
        match Command::new(&plan.program)
            .args(&plan.args)
            .current_dir(&plan.cwd)
            .env_clear()
            .envs(&plan.env)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
        {
            Ok(status) if status.success() => ChildOutcome::ZeroExit,
            Ok(status) => ChildOutcome::NonZeroExit(status.to_string()),
            Err(error) => ChildOutcome::SpawnError(error.to_string()),
        }
    }
}

/// Run one validated agent plan through the shared terminal lifecycle.
fn handoff_agent(
    handoff: &mut impl TerminalHandoff,
    runner: &mut impl ChildRunner,
    plan: &loti_core::launch::LaunchPlan,
) -> Result<ChildOutcome> {
    around_external_process(handoff, || Ok(runner.run(plan)))
}

/// Prepare a queued selection before terminal release, then run it through the
/// shared handoff. Preparation refusals leave the picker open and cause no
/// terminal or child effect; all child outcomes are reported only after reclaim.
fn launch_queued_agent(
    app: &mut App,
    environment: BTreeMap<String, String>,
    handoff: &mut impl TerminalHandoff,
    runner: &mut impl ChildRunner,
) -> Result<bool> {
    let Some(request) = app.take_launch_request() else {
        return Ok(false);
    };
    let profile = request.profile.clone();
    let plan = match app.prepare_agent_launch(&request, environment) {
        Ok(plan) => plan,
        Err(error) => {
            app.agent_launch_refused(failure_and_causes(&error));
            return Ok(true);
        }
    };

    app.agent_launch_prepared();
    match handoff_agent(handoff, runner, &plan)? {
        ChildOutcome::ZeroExit => {}
        ChildOutcome::NonZeroExit(status) => {
            app.agent_launch_failed(format!("agent profile '{profile}' exited with {status}"))
        }
        ChildOutcome::SpawnError(error) => app.agent_launch_failed(format!(
            "agent profile '{profile}' could not start: {error}"
        )),
    }
    Ok(true)
}

/// Hand `text` to the reader's editor and bring back what they saved, or `None`
/// where the editor exited unsuccessfully — which is how an editor says the edit
/// was abandoned, and leaves the buffer as it was.
fn edit_externally(terminal: &mut Tui, text: &str) -> Result<Option<String>> {
    // Looked up before anything is given away: a setting that is not there is not
    // worth blanking the reader's screen for.
    let editor = editor_setting(env::var("VISUAL").ok(), env::var("EDITOR").ok())?;
    around_external_process(&mut Screen::new(terminal, io::stdout()), || {
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

/// The action a wheel event carries to [`App::apply`], or `None` for a mouse
/// event this loop does not turn into one.
///
/// A wheel is not a key, so it carries its own intent rather than the one a key
/// press would: see [`action::Action::WheelDown`] for why editing mode has to
/// tell the two apart. This is the whole of that boundary's own decision, kept
/// as one place a mutated pairing — `ScrollDown` mapped to the wrong action —
/// would be caught directly, rather than only through a loop iteration a unit
/// test cannot drive without a terminal.
fn wheel_action(kind: MouseEventKind) -> Option<action::Action> {
    match kind {
        MouseEventKind::ScrollDown => Some(action::Action::WheelDown),
        MouseEventKind::ScrollUp => Some(action::Action::WheelUp),
        _ => None,
    }
}

/// Apply one terminal event to the browser and say whether it ends the session.
///
/// Every received event owes a frame before its meaning is considered. A handler
/// that does nothing therefore cannot leave the screen stale behind terminal input.
/// The width belongs to the terminal boundary, but passing it in keeps this
/// dispatch path independent of a terminal for the drag rule and its tests.
fn dispatch_event(app: &mut App, event: Event, width: u16) -> bool {
    app.request_redraw();

    match event {
        // Key repeats and releases would otherwise apply an action several times
        // on the terminals that report them.
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            // Any key retires a live notice, bound or not: the reader has moved
            // on, and a notice's lifetime is a maximum. Before dispatch, so a key
            // that raises one of its own keeps it.
            app.clear_flash();
            // The mode is an input to the mapping, so one key can be the way out
            // of a mode here and the way out of the browser there. Every key
            // reaches `apply`, bound or not — see `dispatch`.
            let action = dispatch(key, app.mode());
            let outcome = app.apply(action);
            intent_outcome(app, outcome)
        }
        Event::Key(_) => false,
        Event::Mouse(mouse) => {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    app.press(mouse.column);
                }
                MouseEventKind::Drag(MouseButton::Left) => app.drag(mouse.column, width),
                MouseEventKind::Up(MouseButton::Left) => app.release(),
                kind => {
                    if let Some(action) = wheel_action(kind) {
                        let outcome = app.apply(action);
                        intent_outcome(app, outcome);
                    }
                }
            }
            false
        }
        // Focus, paste and resize events have no browser intent, but no incoming
        // terminal event may leave the browser displaying a frame that predates it.
        Event::FocusGained | Event::FocusLost | Event::Paste(_) | Event::Resize(_, _) => false,
    }
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
        let event = event::read()?;
        let width = terminal.size()?.width;
        if dispatch_event(&mut app, event, width) {
            return Ok(());
        }

        // Only the loop owns the terminal, so an editor can only be run from here.
        if let Some(text) = app.take_editor_handoff() {
            editor_outcome(&mut app, edit_externally(terminal, &text));
            // The terminal was handed back with everything reclaimed, whatever the
            // divider state was before, and the whole screen has to be repainted.
            captured = true;
            app.request_redraw();
        }

        // The loop alone owns terminal capability, so it is also the sole
        // consumer of a selected launch. An absent request is a no-op.
        if launch_queued_agent(
            &mut app,
            env::vars().collect(),
            &mut Screen::new(terminal, io::stdout()),
            &mut DirectChild,
        )? {
            // A successful plan always crossed the terminal handoff; reclaim resets
            // mouse capture and clears the frame regardless of the child's outcome.
            captured = true;
            app.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::rc::Rc;

    /// The source before unit tests, which may construct stores as fixtures but
    /// does not ship in the browser.
    fn production_source(source: &str) -> &str {
        source
            .split_once("\n#[cfg(test)]")
            .map_or(source, |(code, _)| code)
    }

    /// Every browser source module, including nested and newly added modules,
    /// so no new path can silently bypass either core seam.
    fn browser_sources() -> Vec<(String, String)> {
        let source_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        collect_browser_sources(&source_dir, &source_dir, &mut sources);
        sources.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        sources
    }

    fn collect_browser_sources(
        root: &std::path::Path,
        directory: &std::path::Path,
        sources: &mut Vec<(String, String)>,
    ) {
        for entry in fs::read_dir(directory)
            .expect("the browser source directory is readable")
            .map(|entry| entry.expect("the browser source entry is readable"))
        {
            let path = entry.path();
            if path.is_dir() {
                collect_browser_sources(root, &path, sources);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                let module = path
                    .strip_prefix(root)
                    .expect("every browser source belongs to its source directory")
                    .display()
                    .to_string();
                let source = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
                sources.push((module, source));
            }
        }
    }

    /// Removing whitespace makes a call guard insensitive to idiomatic line
    /// wrapping without mistaking the opaque `Store` type in `App` for a call.
    fn compact(source: &str) -> String {
        source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect()
    }

    #[test]
    fn only_the_data_seam_calls_store_methods() {
        // Derive the guarded API from Store itself. A new Store method therefore
        // joins the boundary automatically instead of relying on this list being
        // manually kept in step.
        let methods: Vec<_> = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../loti-core/src/store.rs"
        ))
        .lines()
        .filter_map(|line| line.strip_prefix("    pub fn "))
        .map(|signature| {
            signature
                .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
                .next()
                .expect("a public Store method has a name")
        })
        .collect();
        assert!(!methods.is_empty(), "the Store API was not found");

        for (module, source) in browser_sources() {
            if module == "data.rs" {
                continue;
            }
            let source = compact(production_source(&source));
            for method in &methods {
                let calls = [
                    format!(".{method}("),
                    format!(".{method}::<"),
                    format!("Store::{method}"),
                    format!("store::Store::{method}"),
                    format!("loti_core::store::Store::{method}"),
                ];
                assert!(
                    calls.iter().all(|call| !source.contains(call)),
                    "{module} calls Store::{method}; only data.rs may call the store"
                );
            }
        }
    }

    #[test]
    fn only_the_theme_seam_names_the_status_palette() {
        // Hue and these two lookup functions are the complete status palette
        // exported by core. Naming any one outside theme would make presentation
        // vocabulary spread with no single place to change it.
        let palette = ["Hue", "node_status_hue", "epic_status_hue"];
        for (module, source) in browser_sources() {
            if module == "theme.rs" {
                continue;
            }
            let source = production_source(&source);
            for symbol in palette {
                assert!(
                    !source.contains(symbol),
                    "{module} names render::{symbol}; only theme.rs may name the status palette"
                );
            }
        }
    }

    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseEvent};
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
    fn an_external_process_is_given_the_whole_terminal_and_handed_it_all_back() {
        // Mouse capture is the one a reader would blame on their child rather than
        // on the browser: an inherited-stdio process reads its reports as input.
        for released in [
            Hold::MouseCapture(false),
            Hold::AlternateScreen(false),
            Hold::RawMode(false),
        ] {
            assert!(
                RELEASED_FOR_EXTERNAL_PROCESS.contains(&released),
                "{released:?} is still held while the child runs"
            );
        }
        // A release list that held something, or a reclaim list that released
        // something, would be a handover in name only.
        assert!(RELEASED_FOR_EXTERNAL_PROCESS
            .iter()
            .copied()
            .all(|h| !held(h)));
        assert!(HELD_BY_BROWSER.iter().copied().all(held));

        // The same parts both ways: a round-trip cannot leave the browser running
        // with less of the terminal than it began with.
        let mut released: Vec<&str> = RELEASED_FOR_EXTERNAL_PROCESS
            .iter()
            .copied()
            .map(part)
            .collect();
        let mut reclaimed: Vec<&str> = HELD_BY_BROWSER.iter().copied().map(part).collect();
        released.sort_unstable();
        reclaimed.sort_unstable();
        assert_eq!(released, reclaimed);
        // And no part named twice, so neither list can cover a missing part by
        // repeating another one.
        released.dedup();
        assert_eq!(released.len(), RELEASED_FOR_EXTERNAL_PROCESS.len());
    }

    /// One thing a round-trip asked of the terminal, or of the editor.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Asked {
        Hold(Hold),
        Editor,
        Agent(loti_core::launch::LaunchPlan),
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

    impl TerminalHandoff for Recorder {
        fn hold(&mut self, step: Hold) -> Result<()> {
            self.note(Asked::Hold(step));
            Ok(())
        }

        fn repaint(&mut self) -> Result<()> {
            self.note(Asked::Repaint);
            Ok(())
        }
    }

    /// A terminal device that records raw-mode state instead of changing the test
    /// runner's terminal.
    #[derive(Clone, Default)]
    struct RawRecorder(Rc<RefCell<Vec<bool>>>);

    impl RawRecorder {
        fn states(&self) -> Vec<bool> {
            self.0.borrow().clone()
        }
    }

    impl RawMode for RawRecorder {
        fn set(&mut self, enabled: bool) -> Result<()> {
            self.0.borrow_mut().push(enabled);
            Ok(())
        }
    }

    /// A terminal that fails configured restore operations after recording them.
    struct FailingReclaim {
        recorder: Recorder,
        failures: Vec<(Hold, &'static str)>,
    }

    impl TerminalHandoff for FailingReclaim {
        fn hold(&mut self, step: Hold) -> Result<()> {
            self.recorder.note(Asked::Hold(step));
            if let Some((_, message)) = self.failures.iter().find(|(failed, _)| *failed == step) {
                bail!("cannot restore {step:?}: {message}");
            }
            Ok(())
        }

        fn repaint(&mut self) -> Result<()> {
            self.recorder.note(Asked::Repaint);
            Ok(())
        }
    }

    /// A child boundary that records the validated payload it received instead of
    /// starting a process. It is the same production handoff seam uses at runtime.
    struct FakeChild {
        recorder: Recorder,
        outcome: ChildOutcome,
    }

    impl ChildRunner for FakeChild {
        fn run(&mut self, plan: &loti_core::launch::LaunchPlan) -> ChildOutcome {
            self.recorder.note(Asked::Agent(plan.clone()));
            self.outcome.clone()
        }
    }

    /// Everything taken back after a process, ending in the repaint.
    fn reclaiming() -> Vec<Asked> {
        HELD_BY_BROWSER
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
        let saved = around_external_process(&mut terminal.clone(), || {
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
        for step in RELEASED_FOR_EXTERNAL_PROCESS {
            assert!(
                terminal.at(&Asked::Hold(*step)) < ran,
                "{step:?} was still held while the editor ran"
            );
        }
        // And nothing is taken back while the editor is still using it.
        for step in HELD_BY_BROWSER {
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

        // Startup and reclaim share `HELD_BY_BROWSER`; this literal keeps that
        // one source pinned to the terminal order the browser must actually hold.
        assert_eq!(
            terminal.asked(),
            vec![
                Asked::Hold(Hold::MouseCapture(false)),
                Asked::Hold(Hold::AlternateScreen(false)),
                Asked::Hold(Hold::RawMode(false)),
                Asked::Editor,
                Asked::Hold(Hold::RawMode(true)),
                Asked::Hold(Hold::AlternateScreen(true)),
                Asked::Hold(Hold::MouseCapture(true)),
                Asked::Repaint,
            ]
        );
    }

    #[test]
    fn reclaiming_one_part_still_attempts_every_later_part() {
        let recorder = Recorder::default();
        let editor = recorder.clone();
        let mut terminal = FailingReclaim {
            recorder: recorder.clone(),
            failures: vec![(Hold::RawMode(true), "cannot restore raw mode")],
        };

        let failure = around_external_process(&mut terminal, || {
            editor.note(Asked::Editor);
            Ok(())
        })
        .expect_err("a failed reclaim must reach the terminal caller");
        assert!(failure.to_string().contains("RawMode(true)"), "{failure}");

        // The first reclaim is allowed to fail, but the alternate screen and
        // mouse capture that follow it still have to be restored before that
        // failure reaches the caller.
        assert_eq!(
            recorder.asked(),
            vec![
                Asked::Hold(Hold::MouseCapture(false)),
                Asked::Hold(Hold::AlternateScreen(false)),
                Asked::Hold(Hold::RawMode(false)),
                Asked::Editor,
                Asked::Hold(Hold::RawMode(true)),
                Asked::Hold(Hold::AlternateScreen(true)),
                Asked::Hold(Hold::MouseCapture(true)),
                Asked::Repaint,
            ]
        );
    }

    #[test]
    fn multiple_reclaim_failures_restore_every_part_and_report_the_first() {
        let recorder = Recorder::default();
        let editor = recorder.clone();
        let mut terminal = FailingReclaim {
            recorder: recorder.clone(),
            failures: vec![
                (Hold::RawMode(true), "raw mode did not return"),
                (
                    Hold::AlternateScreen(true),
                    "alternate screen did not return",
                ),
            ],
        };

        let failure = around_external_process(&mut terminal, || {
            editor.note(Asked::Editor);
            Ok(())
        })
        .expect_err("a failed reclaim must reach the terminal caller");
        assert_eq!(
            failure.to_string(),
            "cannot restore RawMode(true): raw mode did not return"
        );

        // Both failed operations are observable, as are the later mouse restore
        // and repaint: reclaiming keeps trying after an error rather than returning
        // at the first failed device operation.
        assert_eq!(
            recorder.asked(),
            vec![
                Asked::Hold(Hold::MouseCapture(false)),
                Asked::Hold(Hold::AlternateScreen(false)),
                Asked::Hold(Hold::RawMode(false)),
                Asked::Editor,
                Asked::Hold(Hold::RawMode(true)),
                Asked::Hold(Hold::AlternateScreen(true)),
                Asked::Hold(Hold::MouseCapture(true)),
                Asked::Repaint,
            ]
        );
    }

    #[test]
    fn a_reclaim_failure_outranks_an_editor_failure_after_restoring_everything() {
        let recorder = Recorder::default();
        let editor = recorder.clone();
        let mut terminal = FailingReclaim {
            recorder: recorder.clone(),
            failures: vec![(Hold::RawMode(true), "raw mode did not return")],
        };

        let failure = around_external_process(&mut terminal, || {
            editor.note(Asked::Editor);
            Err::<(), _>(anyhow!("editor could not start"))
        })
        .expect_err("the editor and reclaim both fail");
        assert_eq!(
            failure.to_string(),
            "cannot restore RawMode(true): raw mode did not return"
        );
        assert_ne!(failure.to_string(), "editor could not start");

        // The editor's failure remains pending while every reclaim operation and
        // repaint are attempted; the terminal failure is the result because the
        // browser cannot safely continue without recovering its terminal state.
        assert_eq!(
            recorder.asked(),
            vec![
                Asked::Hold(Hold::MouseCapture(false)),
                Asked::Hold(Hold::AlternateScreen(false)),
                Asked::Hold(Hold::RawMode(false)),
                Asked::Editor,
                Asked::Hold(Hold::RawMode(true)),
                Asked::Hold(Hold::AlternateScreen(true)),
                Asked::Hold(Hold::MouseCapture(true)),
                Asked::Repaint,
            ]
        );
    }

    #[test]
    fn an_editor_that_could_not_run_still_gets_the_whole_terminal_handed_back() {
        let terminal = Recorder::default();
        let editor = terminal.clone();
        let failure = around_external_process(&mut terminal.clone(), || {
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

    /// The two navigation rows that may start an agent launch.
    #[derive(Clone, Copy)]
    enum QueuedTarget {
        Epic,
        Ticket,
    }

    /// A selected agent picker backed by a local, direct profile. The directory
    /// stays alive because preparation resolves the resources again at acceptance.
    fn queued_agent(target: QueuedTarget) -> (data::fixture::Fixture, tempfile::TempDir, App) {
        let fixture = data::fixture::Fixture::build();
        let resources = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(resources.path().join("workflows")).unwrap();
        std::fs::create_dir_all(resources.path().join("agents")).unwrap();
        std::fs::write(
            resources.path().join(".loti.conf"),
            "workflow-root = \"workflows\"\nagent-root = \"agents\"\n",
        )
        .unwrap();
        std::fs::write(
            resources.path().join("workflows").join("review.md"),
            "# Review\n",
        )
        .unwrap();
        std::fs::write(
            resources.path().join("agents").join("agent.toml"),
            "command = \"agent\"\nargs = [\"{{ loti_prompt }}\"]\n",
        )
        .unwrap();
        let mut app = App::at_working_directory(
            fixture.store.clone(),
            Theme::with_color(false),
            resources.path(),
        )
        .unwrap();
        match target {
            QueuedTarget::Epic => {
                app.apply(Action::EnterEditing).unwrap();
            }
            QueuedTarget::Ticket => {
                app.apply(Action::Descend).unwrap();
                let ticket = app
                    .nav()
                    .rows()
                    .iter()
                    .position(|row| matches!(row.kind, data::RowKind::Work { .. }))
                    .expect("the fixture epic has a ticket row");
                app.apply(Action::CursorFirst).unwrap();
                for _ in 0..ticket {
                    app.apply(Action::CursorDown).unwrap();
                }
                app.apply(Action::EnterEditing).unwrap();
            }
        }
        app.apply(Action::RunAgent).unwrap();
        app.apply(Action::Accept).unwrap();
        (fixture, resources, app)
    }

    fn agent_events(plan: loti_core::launch::LaunchPlan) -> Vec<Asked> {
        RELEASED_FOR_EXTERNAL_PROCESS
            .iter()
            .copied()
            .map(Asked::Hold)
            .chain([Asked::Agent(plan)])
            .chain(reclaiming())
            .collect()
    }

    /// A browser frame as real terminal rows, drawn through the headless backend.
    fn drawn_frame(app: &mut App) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// A failed child returns to the ordinary browser before its dialog is drawn.
    fn assert_restored_browser(app: &mut App, notice: &[&str]) {
        let frame = drawn_frame(app);
        let text = frame.join("\n");
        assert!(frame[0].trim().starts_with("epics"), "{frame:#?}");
        assert!(frame[1].contains("navigation"), "{frame:#?}");
        for words in notice {
            assert!(text.contains(words), "{words:?}: {frame:#?}");
        }
    }

    #[test]
    fn launch_preparation_refuses_before_the_terminal_or_child_runs() {
        let (fixture, resources, mut app) = queued_agent(QueuedTarget::Epic);
        // The picker read this profile before it was selected. Re-resolving now
        // catches the changed value and core rejects it before the screen is lost.
        std::fs::write(
            resources.path().join("agents").join("agent.toml"),
            "command = \"\"\nargs = [\"{{ loti_prompt }}\"]\n",
        )
        .unwrap();
        let before = fixture.tracker_state();
        let recorder = Recorder::default();
        let mut child = FakeChild {
            recorder: recorder.clone(),
            outcome: ChildOutcome::ZeroExit,
        };

        assert!(
            launch_queued_agent(&mut app, BTreeMap::new(), &mut recorder.clone(), &mut child)
                .unwrap()
        );
        assert!(recorder.asked().is_empty(), "{:#?}", recorder.asked());
        assert!(app.surface().is_some(), "preparation closed the picker");
        assert!(app.editing_target().is_some(), "preparation ended editing");
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("the preparation refusal opened no dialog")
        };
        assert!(dialog
            .message()
            .contains("profile command must not be empty"));
        let frame = drawn_frame(&mut app);
        let refusal = frame.join("\n");
        assert!(
            refusal.contains("profile command must not be empty"),
            "{frame:#?}"
        );
        assert!(refusal.contains("back to the picker"), "{frame:#?}");
        assert_eq!(
            fixture.tracker_state(),
            before,
            "launch preparation changed tracker data"
        );
    }

    #[test]
    fn an_invalid_selected_workflow_is_refused_before_the_terminal_or_child_runs() {
        let (fixture, resources, mut app) = queued_agent(QueuedTarget::Epic);
        // A local resource keeps shadowing a global resource with the same id, so
        // invalidating it proves acceptance re-resolves the picker selection.
        std::fs::write(resources.path().join("workflows").join("review.md"), [0xff]).unwrap();
        let before = fixture.tracker_state();
        let recorder = Recorder::default();
        let mut child = FakeChild {
            recorder: recorder.clone(),
            outcome: ChildOutcome::ZeroExit,
        };

        assert!(
            launch_queued_agent(&mut app, BTreeMap::new(), &mut recorder.clone(), &mut child)
                .unwrap()
        );
        assert!(recorder.asked().is_empty(), "{:#?}", recorder.asked());
        assert!(app.surface().is_some(), "preparation closed the picker");
        assert!(app.editing_target().is_some(), "preparation ended editing");
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("the preparation refusal opened no dialog")
        };
        assert_eq!(dialog.message(), "workflow 'review' is invalid");
        let frame = drawn_frame(&mut app);
        let refusal = frame.join("\n");
        assert!(
            refusal.contains("workflow 'review' is invalid"),
            "{frame:#?}"
        );
        assert!(refusal.contains("back to the picker"), "{frame:#?}");
        assert_eq!(
            fixture.tracker_state(),
            before,
            "launch preparation changed tracker data"
        );
    }

    #[test]
    fn a_zero_exit_runs_the_prepared_payload_and_restores_the_browser_silently() {
        for target in [QueuedTarget::Epic, QueuedTarget::Ticket] {
            let (fixture, _resources, mut app) = queued_agent(target);
            let (kind, reference) = match target {
                QueuedTarget::Epic => ("epic", fixture.epic.clone()),
                QueuedTarget::Ticket => (
                    "ticket",
                    format!("{}/{}", fixture.node.epic_id, fixture.node.number),
                ),
            };
            let before = fixture.tracker_state();
            let recorder = Recorder::default();
            let mut child = FakeChild {
                recorder: recorder.clone(),
                outcome: ChildOutcome::ZeroExit,
            };

            launch_queued_agent(&mut app, BTreeMap::new(), &mut recorder.clone(), &mut child)
                .unwrap();
            let plan = recorder
                .asked()
                .into_iter()
                .find_map(|asked| match asked {
                    Asked::Agent(plan) => Some(plan),
                    Asked::Hold(_) | Asked::Editor | Asked::Repaint => None,
                })
                .expect("the child was not run");
            assert_eq!(plan.program, "agent");
            assert_eq!(plan.cwd, fixture.store.root());
            assert_eq!(
                plan.env.get(loti_core::launch::SESSION_ENV_VAR),
                Some(&reference)
            );
            assert_eq!(
                plan.env.get(loti_core::launch::WORKFLOW_ENV_VAR),
                Some(&"review".to_string())
            );
            assert!(
                plan.args[0].contains(&format!("workflow \"review\" on {kind} \"{reference}\"")),
                "{:?}",
                plan.args
            );
            assert_eq!(recorder.asked(), agent_events(plan));
            assert!(app.modal().is_none(), "a zero exit raised a report");
            assert!(app.surface().is_none());
            assert!(app.editing_target().is_none());
            let frame = drawn_frame(&mut app);
            assert!(frame[0].trim().starts_with("epics"), "{frame:#?}");
            assert!(frame[1].contains("navigation"), "{frame:#?}");
            assert_eq!(
                fixture.tracker_state(),
                before,
                "a successful {kind} agent exit changed tracker data"
            );
        }
    }

    #[test]
    fn a_nonzero_agent_exit_is_reported_after_reclaiming_the_terminal() {
        let (fixture, _resources, mut app) = queued_agent(QueuedTarget::Epic);
        let before = fixture.tracker_state();
        let recorder = Recorder::default();
        let mut child = FakeChild {
            recorder: recorder.clone(),
            outcome: ChildOutcome::NonZeroExit("exit status: 7".to_string()),
        };

        launch_queued_agent(&mut app, BTreeMap::new(), &mut recorder.clone(), &mut child).unwrap();
        let asked = recorder.asked();
        assert!(matches!(asked.last(), Some(Asked::Repaint)), "{asked:?}");
        let run = asked
            .iter()
            .position(|event| matches!(event, Asked::Agent(_)))
            .unwrap();
        let reclaim = asked
            .iter()
            .position(|event| *event == Asked::Hold(Hold::RawMode(true)))
            .unwrap();
        assert!(run < reclaim, "{asked:?}");
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("a non-zero child raised no report")
        };
        assert_eq!(
            dialog.message(),
            "agent profile 'agent' exited with exit status: 7"
        );
        assert_restored_browser(&mut app, &["agent profile 'agent'", "exit status: 7"]);
        assert_eq!(
            fixture.tracker_state(),
            before,
            "a failed agent exit changed tracker data"
        );
    }

    #[test]
    fn a_spawn_failure_is_reported_after_reclaiming_the_terminal() {
        let (fixture, _resources, mut app) = queued_agent(QueuedTarget::Epic);
        let before = fixture.tracker_state();
        let recorder = Recorder::default();
        let mut child = FakeChild {
            recorder: recorder.clone(),
            outcome: ChildOutcome::SpawnError("missing executable".to_string()),
        };

        launch_queued_agent(&mut app, BTreeMap::new(), &mut recorder.clone(), &mut child).unwrap();
        let asked = recorder.asked();
        assert!(matches!(asked.last(), Some(Asked::Repaint)), "{asked:?}");
        let Some(Modal::Dialog(dialog)) = app.modal() else {
            panic!("a spawn failure raised no report")
        };
        assert_eq!(
            dialog.message(),
            "agent profile 'agent' could not start: missing executable"
        );
        assert_restored_browser(
            &mut app,
            &[
                "agent profile 'agent'",
                "could not start",
                "missing",
                "executable",
            ],
        );
        assert_eq!(
            fixture.tracker_state(),
            before,
            "a spawn failure changed tracker data"
        );
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

        Screen::new(&mut terminal, Vec::new()).repaint().unwrap();
        assert!(
            !on_screen(&terminal).contains("leftovers"),
            "the screen the editor drew on was kept: {:?}",
            on_screen(&terminal)
        );
    }

    #[test]
    fn each_control_sequence_part_of_the_screen_is_let_go_of_and_taken_back() {
        let mut raw_mode = RawRecorder::default();
        let mut sent: Vec<(Hold, Vec<u8>)> = Vec::new();
        for step in [
            Hold::AlternateScreen(false),
            Hold::AlternateScreen(true),
            Hold::MouseCapture(false),
            Hold::MouseCapture(true),
        ] {
            let mut out = Vec::new();
            hold_step(step, &mut out, &mut raw_mode).unwrap();
            assert!(!out.is_empty(), "{step:?} sent the terminal nothing");
            for (other, sequence) in &sent {
                assert_ne!(*sequence, out, "{step:?} sends what {other:?} sends");
            }
            sent.push((step, out));
        }

        // The terminal the browser really performs on sends exactly those
        // sequences. A production implementation that does not perform its seam
        // would otherwise leave this protocol test green.
        let mut terminal = Terminal::new(TestBackend::new(4, 1)).unwrap();
        for (step, sequence) in &sent {
            let mut screen = Screen::new(&mut terminal, Vec::new());
            screen.hold(*step).unwrap();
            assert_eq!(
                &screen.output, sequence,
                "the real terminal does not perform {step:?}"
            );
        }

        // Polarity, against the terminal protocol rather than against the code that
        // implements it: releasing a part must send what turns it off, and taking it
        // back what turns it on. Inverted, the round-trip would enable mouse capture
        // on the way into the editor — whose input is then read as typed characters.
        let mut sequence = |step: Hold| -> String {
            let mut out = Vec::new();
            hold_step(step, &mut out, &mut raw_mode).unwrap();
            String::from_utf8(out).expect("a control sequence is text")
        };
        assert!(sequence(Hold::AlternateScreen(true)).ends_with("h"));
        assert!(sequence(Hold::AlternateScreen(false)).ends_with("l"));
        assert!(sequence(Hold::MouseCapture(true)).ends_with("h"));
        assert!(sequence(Hold::MouseCapture(false)).ends_with("l"));
    }

    #[test]
    fn the_screen_sets_raw_mode_off_for_the_editor_and_back_on_afterwards() {
        let mut terminal = Terminal::new(TestBackend::new(4, 1)).unwrap();
        let raw_mode = RawRecorder::default();
        let observed = raw_mode.clone();
        let mut screen = Screen::with_raw_mode(&mut terminal, Vec::new(), raw_mode);

        screen.hold(Hold::RawMode(false)).unwrap();
        screen.hold(Hold::RawMode(true)).unwrap();

        assert_eq!(observed.states(), [false, true]);
        assert!(
            screen.output.is_empty(),
            "raw mode is a device setting, not a control sequence"
        );
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

    #[test]
    fn every_received_event_variant_owes_a_redraw() {
        // Built from each crossterm event the loop receives. Focus, paste and
        // resize deliberately have no browser intent, so their rows prove the
        // redraw request belongs before dispatch rather than inside a handler.
        let events = [
            ("focus gained", Event::FocusGained),
            ("focus lost", Event::FocusLost),
            (
                "key press",
                Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            ),
            (
                "key repeat",
                Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Char('x'),
                    KeyModifiers::NONE,
                    KeyEventKind::Repeat,
                )),
            ),
            (
                "key release",
                Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Char('x'),
                    KeyModifiers::NONE,
                    KeyEventKind::Release,
                )),
            ),
            (
                "mouse",
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: 0,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                }),
            ),
            ("paste", Event::Paste("pasted".into())),
            ("resize", Event::Resize(100, 24)),
        ];

        for (kind, event) in events {
            let fixture = data::fixture::Fixture::build();
            let mut app = App::new(fixture.store.clone(), Theme::with_color(false)).unwrap();
            assert!(app.take_redraw_request(), "the opening frame is owed");

            assert!(
                !dispatch_event(&mut app, event, 100),
                "{kind} unexpectedly ended the session"
            );
            assert!(
                app.take_redraw_request(),
                "{kind} did not request the frame it owes"
            );
        }
    }

    #[test]
    fn a_key_retires_an_earlier_notice_before_its_own_dispatch() {
        let fixture = data::fixture::Fixture::build();
        let mut app = App::new(fixture.store.clone(), Theme::with_color(false)).unwrap();
        assert!(app.take_redraw_request(), "the opening frame is owed");
        app.apply(Action::EnterEditing).unwrap();
        app.flash("an earlier notice");
        assert_eq!(app.flash_message(), Some("an earlier notice"));
        assert!(app.take_redraw_request(), "the earlier notice is visible");

        // `j` is a key the editing mode cannot apply to this frozen epic row, so
        // dispatch replaces the earlier notice with the mode's own explanation.
        assert!(!dispatch_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            100,
        ));
        assert_eq!(
            app.flash_message(),
            Some("not an editing action — Esc to leave")
        );
    }

    #[test]
    fn a_bound_or_unbound_key_retires_a_notice_when_it_raises_no_replacement() {
        let keys = [
            (
                "bound layout key",
                KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
                Some(Action::ToggleZoom),
            ),
            (
                "unbound key",
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
                None,
            ),
        ];

        for (kind, key, expected) in keys {
            // Both paths are deliberately silent in browse mode, so any notice
            // after dispatch could only be the one this key was required to retire.
            assert_eq!(keymap::action_for(key, Mode::Browse), expected, "{kind}");
            let fixture = data::fixture::Fixture::build();
            let mut app = App::new(fixture.store.clone(), Theme::with_color(false)).unwrap();
            app.flash("an earlier notice");
            assert_eq!(app.flash_message(), Some("an earlier notice"));

            assert!(
                !dispatch_event(&mut app, Event::Key(key), 100),
                "{kind} unexpectedly ended the session"
            );
            assert_eq!(app.flash_message(), None, "{kind} kept the earlier notice");
        }
    }

    #[test]
    fn a_wheel_event_carries_its_own_direction_not_a_cursor_move() {
        // No terminal is needed: `wheel_action` is the whole of this boundary's
        // own decision, so it is asked directly rather than through a loop
        // iteration. Built from real crossterm events — not the `MouseEventKind`
        // variants alone — so the test exercises the same value the event loop
        // reads off the wire.
        let scroll_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let scroll_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(wheel_action(scroll_down.kind), Some(Action::WheelDown));
        assert_eq!(wheel_action(scroll_up.kind), Some(Action::WheelUp));

        // Down must land on down and up on up: swapping the pairing is the exact
        // mutation this test exists to catch, so both directions are checked
        // against each other, not only against a variant each on its own.
        assert_ne!(wheel_action(scroll_down.kind), wheel_action(scroll_up.kind));

        // A press or drag is not a wheel event, so it carries no wheel action at
        // all — this boundary only ever adds a case, it never invents one.
        let press = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(wheel_action(press.kind), None);
    }
}
