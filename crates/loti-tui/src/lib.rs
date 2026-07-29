//! `loti-tui` — the full-screen browser for a loti store.
//!
//! A file-browser view of the tracker: epics are the top level, entering one
//! lists its tickets, entering a ticket lists its subtickets, and a preview pane
//! shows the same document `loti epic show` / `loti ticket show` print.
//!
//! The crate owns the terminal and nothing else. Store access lives in
//! [`data`], the position in [`nav`], the state machine in [`app`], and drawing
//! in [`ui`], so none of those needs a terminal to be exercised.

pub mod action;
pub mod app;
pub mod data;
pub mod keymap;
pub mod nav;
pub mod theme;
pub mod ui;

use std::io::{self, IsTerminal, Write};
use std::panic;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

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

/// How often the loop wakes with no input to read.
///
/// A persistent tick is what lets anything timed appear at all: waiting on input
/// alone, a message that clears itself has no wakeup to clear it on. Coarse
/// enough to cost nothing, fine enough that a timed change never overstays its
/// deadline by more than a quarter second.
const TICK: Duration = Duration::from_millis(250);

fn event_loop(terminal: &mut Tui, mut app: App) -> Result<()> {
    // Mouse capture is what makes the divider draggable, but it also takes
    // click-drag text selection away from the terminal. Zoom is the way out:
    // while zoomed there is no divider to drag, so capture is released and
    // selecting text from the preview works again.
    let mut captured = true;
    loop {
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
                if let Some(action) = keymap::action_for(key) {
                    if app.apply(action)? {
                        return Ok(());
                    }
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
                    app.apply(action::Action::CursorDown)?;
                }
                MouseEventKind::ScrollUp => {
                    app.apply(action::Action::CursorUp)?;
                }
                _ => {}
            },
            _ => {}
        }
    }
}
