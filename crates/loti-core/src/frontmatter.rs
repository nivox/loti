//! Splitting and joining the two regions of every store file.
//!
//! A store file is a YAML frontmatter block delimited by a line containing
//! exactly `---`, immediately followed by the free-form body. The body is
//! verbatim and has no managed sections: whatever is below the closing
//! delimiter is returned untouched and re-emitted untouched.
//!
//! Invariants enforced here:
//!   * The file must open with a `---` delimiter line; a file without an
//!     opening delimiter has no frontmatter and is rejected as malformed.
//!   * The frontmatter ends at the first subsequent `---` delimiter line; the
//!     remainder (after that line's newline) is the body, byte-for-byte.
//!   * Re-emitting always writes `---\n<frontmatter>---\n<body>`, so the split
//!     is stable across a read/write cycle.

use thiserror::Error;

/// A store file separated into its YAML frontmatter and free-form body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitFile {
    /// The raw YAML text between the opening and closing `---` delimiters,
    /// without either delimiter line. May be empty.
    pub frontmatter: String,
    /// Everything below the closing delimiter, verbatim. May be empty.
    pub body: String,
}

/// Failure to locate the frontmatter delimiters.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrontmatterError {
    /// The file does not begin with a `---` delimiter line.
    #[error("file does not start with a '---' frontmatter delimiter")]
    MissingOpen,
    /// The opening delimiter is never closed by a later `---` line.
    #[error("frontmatter opened with '---' is never closed by a matching '---'")]
    Unterminated,
}

/// A line is a delimiter when, ignoring a trailing `\r`, it is exactly `---`.
fn is_delimiter(line: &str) -> bool {
    line.strip_suffix('\r').unwrap_or(line) == "---"
}

/// Split raw file text into frontmatter and body.
///
/// The opening `---` must be the first line; the frontmatter runs to the next
/// `---` line, and the body is everything after that line's newline.
pub fn split(text: &str) -> Result<SplitFile, FrontmatterError> {
    let mut lines = text.split_inclusive('\n');

    let first = lines.next().ok_or(FrontmatterError::MissingOpen)?;
    if !is_delimiter(first.strip_suffix('\n').unwrap_or(first)) {
        return Err(FrontmatterError::MissingOpen);
    }

    let mut frontmatter = String::new();
    for line in lines.by_ref() {
        let content = line.strip_suffix('\n').unwrap_or(line);
        if is_delimiter(content) {
            // Everything the iterator has not yet yielded is the body.
            let body: String = lines.collect();
            return Ok(SplitFile { frontmatter, body });
        }
        frontmatter.push_str(line);
    }

    Err(FrontmatterError::Unterminated)
}

/// Join frontmatter and body back into file text.
///
/// Always emits `---\n<frontmatter>---\n<body>`. A trailing newline is ensured
/// on the frontmatter so the closing delimiter sits on its own line even when
/// the serialiser omitted it.
pub fn join(frontmatter: &str, body: &str) -> String {
    let mut out = String::with_capacity(frontmatter.len() + body.len() + 8);
    out.push_str("---\n");
    out.push_str(frontmatter);
    if !frontmatter.is_empty() && !frontmatter.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("---\n");
    out.push_str(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter_and_body() {
        let text = "---\nname: hi\n---\nbody line\nmore\n";
        let split = split(text).unwrap();
        assert_eq!(split.frontmatter, "name: hi\n");
        assert_eq!(split.body, "body line\nmore\n");
    }

    #[test]
    fn empty_body_is_allowed() {
        let text = "---\nname: hi\n---\n";
        let split = split(text).unwrap();
        assert_eq!(split.body, "");
    }

    #[test]
    fn body_delimiters_are_not_confused_for_the_close() {
        // A `---` inside the body must not re-split: the close is the first
        // `---` after the opening one, everything else is verbatim body.
        let text = "---\nname: hi\n---\nintro\n---\nsecond section\n";
        let split = split(text).unwrap();
        assert_eq!(split.frontmatter, "name: hi\n");
        assert_eq!(split.body, "intro\n---\nsecond section\n");
    }

    #[test]
    fn missing_open_delimiter_is_an_error() {
        assert_eq!(split("name: hi\n"), Err(FrontmatterError::MissingOpen));
        assert_eq!(split(""), Err(FrontmatterError::MissingOpen));
    }

    #[test]
    fn unterminated_frontmatter_is_an_error() {
        assert_eq!(
            split("---\nname: hi\nno close\n"),
            Err(FrontmatterError::Unterminated)
        );
    }

    #[test]
    fn join_is_the_inverse_of_split() {
        let text = "---\nname: hi\nlabels:\n- a\n---\nbody\n";
        let split = split(text).unwrap();
        assert_eq!(join(&split.frontmatter, &split.body), text);
    }

    #[test]
    fn join_ensures_a_newline_before_the_close_delimiter() {
        // A serialiser that omits the trailing newline must not swallow the
        // closing delimiter onto the same line.
        assert_eq!(join("name: hi", "body\n"), "---\nname: hi\n---\nbody\n");
    }

    #[test]
    fn carriage_returns_on_delimiters_are_tolerated() {
        let text = "---\r\nname: hi\r\n---\r\nbody\r\n";
        let split = split(text).unwrap();
        assert_eq!(split.frontmatter, "name: hi\r\n");
        assert_eq!(split.body, "body\r\n");
    }
}
