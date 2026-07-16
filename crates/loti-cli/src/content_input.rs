//! Shared content-input helper.
//!
//! Rules enforced here:
//!   * Free-form / binary payloads — epic & ticket `body`, comment text, asset
//!     data — are **never** passed inline; they come from `--file <path>` or,
//!     when that is absent, from piped **stdin**.
//!   * The reader **must never block on a TTY**: an interactive stdin is
//!     treated as "no source".
//!   * An absent source is empty for an optional payload and an error for a
//!     required one.

use std::fs;
use std::io::{IsTerminal, Read};
use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// Resolve a free-form/binary payload following the content-input rules above.
///
/// Precedence: `--file` wins over stdin. When no file is given, stdin is read
/// only if it is *not* a TTY (piped/redirected), so the call never blocks on an
/// interactive terminal. `stdin_is_tty` is injected to keep this pure/testable;
/// [`read_content`] wires it to the real process stdin.
///
/// - `Some(bytes)` — a source was present (may be empty if a file/pipe was empty).
/// - `None` — no source; caller treats as empty (optional) after the
///   `required` gate.
///
/// Errors if `required` and no source is present, or on file-read failure.
pub fn resolve_content<R: Read>(
    file: Option<&Path>,
    stdin: &mut R,
    stdin_is_tty: bool,
    required: bool,
) -> Result<Option<Vec<u8>>> {
    if let Some(path) = file {
        let bytes = fs::read(path)
            .with_context(|| format!("reading content from --file {}", path.display()))?;
        return Ok(Some(bytes));
    }

    if !stdin_is_tty {
        let mut buf = Vec::new();
        stdin
            .read_to_end(&mut buf)
            .context("reading content from stdin")?;
        return Ok(Some(buf));
    }

    // No --file and stdin is an interactive TTY: never block on it.
    if required {
        return Err(anyhow!(
            "content is required: provide it via stdin (pipe/redirect) or --file <path> \
             (content is never passed inline)"
        ));
    }
    Ok(None)
}

/// Convenience wrapper over [`resolve_content`] wired to the real process stdin
/// and its TTY detection.
pub fn read_content(file: Option<&Path>, required: bool) -> Result<Option<Vec<u8>>> {
    let stdin = std::io::stdin();
    let is_tty = stdin.is_terminal();
    let mut lock = stdin.lock();
    resolve_content(file, &mut lock, is_tty, required)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn file_beats_stdin() {
        let dir = std::env::temp_dir();
        let path = dir.join("loti_content_input_test.txt");
        fs::write(&path, b"from file").unwrap();
        let mut stdin = Cursor::new(b"from stdin".to_vec());
        let out = resolve_content(Some(&path), &mut stdin, false, true).unwrap();
        assert_eq!(out, Some(b"from file".to_vec()));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn reads_piped_stdin() {
        let mut stdin = Cursor::new(b"piped body".to_vec());
        let out = resolve_content(None, &mut stdin, false, false).unwrap();
        assert_eq!(out, Some(b"piped body".to_vec()));
    }

    #[test]
    fn tty_without_file_is_absent_for_optional() {
        let mut stdin = Cursor::new(Vec::new());
        let out = resolve_content(None, &mut stdin, true, false).unwrap();
        assert_eq!(out, None);
    }

    #[test]
    fn tty_without_file_errors_when_required() {
        let mut stdin = Cursor::new(Vec::new());
        let err = resolve_content(None, &mut stdin, true, true).unwrap_err();
        assert!(err.to_string().contains("content is required"));
    }

    #[test]
    fn empty_pipe_is_a_present_but_empty_source() {
        let mut stdin = Cursor::new(Vec::new());
        let out = resolve_content(None, &mut stdin, false, true).unwrap();
        assert_eq!(out, Some(Vec::new()));
    }

    #[test]
    fn missing_file_is_an_error() {
        let mut stdin = Cursor::new(Vec::new());
        let missing = Path::new("/definitely/not/here/loti-xyz");
        assert!(resolve_content(Some(missing), &mut stdin, true, false).is_err());
    }
}
