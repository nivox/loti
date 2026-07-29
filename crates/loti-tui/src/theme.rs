//! Colours and status glyphs.
//!
//! The status palette is not defined here: it is read from `loti-core`, so this
//! surface and the plain text listings cannot disagree about which state is
//! which colour. This module only maps a hue to the colour type the widgets
//! want, and pairs each state with a glyph.
//!
//! Two rules:
//!   * the background is never painted, so the terminal's own theme shows
//!     through instead of a hard black rectangle on a light profile;
//!   * with `NO_COLOR` set every hue collapses to the default foreground, which
//!     is legible only because the glyph — not the colour — is what identifies a
//!     state.

use loti_core::render::{self, Hue};
use ratatui::style::Color;
use ratatui_markdown::theme::{Generation, RichTextTheme};

/// Whether colour may be emitted at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    color: bool,
}

impl Theme {
    /// A theme that honours `NO_COLOR`: any non-empty value disables colour, as
    /// the convention specifies.
    pub fn from_env() -> Self {
        let color = std::env::var_os("NO_COLOR")
            .map(|v| v.is_empty())
            .unwrap_or(true);
        Self { color }
    }

    /// A theme with colour forced on or off, for tests and callers that already
    /// know.
    pub fn with_color(color: bool) -> Self {
        Self { color }
    }

    /// The colour for a hue, or the default foreground when colour is off.
    pub fn hue(self, hue: Hue) -> Color {
        if !self.color {
            return Color::Reset;
        }
        match hue {
            // The terminal's own foreground, not white: "not started" must stay
            // readable on a light profile too.
            Hue::Pending => Color::Reset,
            Hue::Active => Color::Cyan,
            Hue::Attention => Color::Yellow,
            Hue::Resolved => Color::Green,
            Hue::Abandoned => Color::DarkGray,
        }
    }

    /// The colour for a node status in its wire form.
    pub fn node_status(self, status: &str) -> Color {
        self.hue(render::node_status_hue(status))
    }

    /// The colour for an epic state in its wire form.
    pub fn epic_status(self, state: &str) -> Color {
        self.hue(render::epic_status_hue(state))
    }

    /// A muted colour for secondary columns (child counts, identifiers).
    pub fn muted(self) -> Color {
        if self.color {
            Color::DarkGray
        } else {
            Color::Reset
        }
    }

    /// The accent used for borders and the breadcrumb.
    pub fn accent(self) -> Color {
        if self.color {
            Color::Cyan
        } else {
            Color::Reset
        }
    }
}

/// The glyph identifying a state. Glyphs are distinct shapes, not shades, so the
/// list stays readable with colour disabled or on a palette that renders two
/// hues alike.
///
/// Invariant: a glyph and its colour say the same thing. Every state that reads
/// as resolved gets the resolved glyph, whether it is a node's `done` or an
/// epic's `completed` — a green circle would claim "not started" in shape and
/// "finished" in colour.
pub fn glyph(status: &str) -> &'static str {
    match status {
        "in-progress" => "◐",
        "blocked" => "⊘",
        "done" | "completed" => "✓",
        "closed" => "✗",
        // `open` (an epic) and `to-do` (a node) are both "not started".
        _ => "○",
    }
}

impl RichTextTheme for Theme {
    fn generation(&self) -> Generation {
        // The palette is fixed for the life of the process, so one generation
        // covers it and the renderer's caches never need invalidating.
        Generation(1)
    }

    fn get_text_color(&self) -> Color {
        Color::Reset
    }

    fn get_muted_text_color(&self) -> Color {
        self.muted()
    }

    fn get_primary_color(&self) -> Color {
        self.accent()
    }

    fn get_popup_selected_background(&self) -> Color {
        if self.color {
            Color::DarkGray
        } else {
            Color::Reset
        }
    }

    fn get_border_color(&self) -> Color {
        self.muted()
    }

    fn get_focused_border_color(&self) -> Color {
        self.accent()
    }

    fn get_secondary_color(&self) -> Color {
        self.hue(Hue::Resolved)
    }

    fn get_info_color(&self) -> Color {
        self.accent()
    }

    fn get_json_key_color(&self) -> Color {
        self.accent()
    }

    fn get_json_string_color(&self) -> Color {
        self.hue(Hue::Resolved)
    }

    fn get_json_number_color(&self) -> Color {
        self.hue(Hue::Attention)
    }

    fn get_json_bool_color(&self) -> Color {
        self.hue(Hue::Attention)
    }

    fn get_json_null_color(&self) -> Color {
        self.muted()
    }

    fn get_accent_yellow(&self) -> Color {
        self.hue(Hue::Attention)
    }

    fn get_background_color(&self) -> Color {
        // Never paint a background: the terminal's own is correct on both light
        // and dark profiles, and the library's default (black) is not.
        Color::Reset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_collapses_every_hue_to_the_default_foreground() {
        let plain = Theme::with_color(false);
        for status in ["to-do", "in-progress", "blocked", "done", "closed"] {
            assert_eq!(plain.node_status(status), Color::Reset);
        }
    }

    #[test]
    fn glyphs_distinguish_every_state_without_colour() {
        let glyphs: Vec<&str> = ["to-do", "in-progress", "blocked", "done", "closed"]
            .iter()
            .map(|s| glyph(s))
            .collect();
        let mut unique = glyphs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), glyphs.len());
    }

    #[test]
    fn a_glyph_never_contradicts_its_colour() {
        let theme = Theme::with_color(true);
        // Every state, node or epic, in its wire form.
        for status in [
            "to-do",
            "in-progress",
            "blocked",
            "done",
            "closed",
            "open",
            "completed",
        ] {
            let hue = if ["open", "completed"].contains(&status) {
                render::epic_status_hue(status)
            } else {
                render::node_status_hue(status)
            };
            let shape = glyph(status);
            let expected = match hue {
                Hue::Pending => "○",
                Hue::Active => {
                    // An epic's active state is "open" (nothing started yet), a
                    // node's is "in-progress"; both are legitimate shapes here.
                    if status == "open" {
                        "○"
                    } else {
                        "◐"
                    }
                }
                Hue::Attention => "⊘",
                Hue::Resolved => "✓",
                Hue::Abandoned => "✗",
            };
            assert_eq!(
                shape, expected,
                "{status} is painted {hue:?} but drawn {shape}"
            );
            let _ = theme.hue(hue);
        }
    }

    #[test]
    fn colours_follow_the_core_palette() {
        let theme = Theme::with_color(true);
        // The mapping is core's, not this crate's: blocked is the attention hue.
        assert_eq!(theme.node_status("blocked"), theme.hue(Hue::Attention));
        assert_eq!(theme.node_status("done"), theme.hue(Hue::Resolved));
        assert_eq!(theme.epic_status("completed"), theme.hue(Hue::Resolved));
    }

    #[test]
    fn the_background_is_left_to_the_terminal() {
        assert_eq!(
            RichTextTheme::get_background_color(&Theme::with_color(true)),
            Color::Reset
        );
    }
}
