//! `loti` binary entrypoint.
//!
//! This parses the single declarative command-tree (see [`cli`]) and routes to
//! [`dispatch`], the thin adapter that resolves the data root, reads any
//! stdin/`--file` payload, calls the UI-agnostic core operation, and renders a
//! short result line. All the business logic lives in `loti-core`; this crate
//! stays a presentation shell so the same operations back any future surface.

mod cli;
mod content_input;
mod dispatch;

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use clap::Parser;

use cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdin = io::stdin();
    let stdin_is_tty = stdin.is_terminal();
    // Colour is only ever emitted to an interactive terminal; piped/redirected
    // output stays plain. The AutoStream wrapper strips any stray escape codes
    // when the destination is not a terminal, as a second line of defence.
    let stdout_is_tty = io::stdout().is_terminal();
    let mut out = anstream::AutoStream::auto(io::stdout());
    let mut err = io::stderr();

    match dispatch::run(
        &cli,
        &mut stdin.lock(),
        stdin_is_tty,
        stdout_is_tty,
        &mut out,
        &mut err,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // A failed operation prints a plain message to stderr and exits
            // non-zero; user output carries no spec/ticket references.
            let _ = writeln!(err, "loti: {e:#}");
            ExitCode::FAILURE
        }
    }
}
