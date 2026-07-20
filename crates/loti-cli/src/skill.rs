//! The static skill document.
//!
//! `loti skill` prints this verbatim. The content is fully hand-authored prose
//! embedded into the binary at compile time — there is no generation, no
//! templating, and no splicing from the command model or any runtime file. It
//! deliberately ships self-contained: an agent can learn the concepts and the
//! workflow from this text alone, then turn to `loti --help-full` for the exact
//! command surface.

/// The hand-authored skill document, embedded at compile time and printed
/// verbatim by `loti skill`.
pub const SKILL: &str = include_str!("skill.md");

#[cfg(test)]
mod tests {
    use super::SKILL;

    #[test]
    fn has_the_seven_sections_in_order() {
        // The document must carry exactly this section shape, in this order:
        // frontmatter -> what/when -> hard rule -> concepts -> workflow ->
        // gotchas -> help-full handoff.
        let anchors = [
            "---",                                // frontmatter opens
            "name: loti",                         // frontmatter body
            "## What this is and when to use it", // what / when
            "## The one hard rule",               // the hard rule
            "## Core concepts",                   // distilled concepts
            "## Lifecycle & workflow",            // lifecycle / workflow
            "## Gotchas",                         // gotchas
            "## The whole command surface",       // --help-full handoff
        ];
        let mut cursor = 0usize;
        for anchor in anchors {
            let found = SKILL[cursor..]
                .find(anchor)
                .unwrap_or_else(|| panic!("missing or out-of-order section: {anchor}"));
            cursor += found + anchor.len();
        }
    }

    #[test]
    fn carries_the_prominent_hard_rule() {
        // The bypass prohibition must be present, stated as a rule, and cover
        // reads as well as writes so a "just reading" excuse cannot sidestep it.
        assert!(
            SKILL.contains(
                "Never touch the store files directly — for reading or writing. Every\n\
                 operation goes through the `loti` CLI."
            ),
            "the hard rule line must be present verbatim"
        );
    }

    #[test]
    fn distilled_concepts_ship_self_contained() {
        // An agent must be able to work from this text alone: the core
        // vocabulary is all here.
        for concept in [
            "Epic",
            "Subticket",
            "Node",
            "<epic-id>/<number>",
            "to-do",
            "in-progress",
            "blocked",
            "done",
            "closed",
            "Terminal",
            "blocked-by",
            "completed",
            "actor",
            "Comments are the sole attribution channel",
        ] {
            assert!(
                SKILL.contains(concept),
                "concept missing from skill: {concept}"
            );
        }
    }

    #[test]
    fn hands_off_to_help_full() {
        assert!(
            SKILL.contains("loti --help-full"),
            "the skill must hand off to the full command surface"
        );
    }

    #[test]
    fn carries_no_spec_or_ticket_references() {
        // User-facing text must not reference spec sections or tracker tickets:
        // no section-sign, no 'SPEC', no 'loti-impl/'/'loti-spec/' pointers, and
        // no bare design-ticket ids like 'T5'.
        assert!(
            !SKILL.contains('\u{00a7}'),
            "skill must not contain a section sign"
        );
        assert!(!SKILL.contains("SPEC"), "skill must not mention SPEC");
        assert!(
            !SKILL.contains("loti-impl/") && !SKILL.contains("loti-spec/"),
            "skill must not reference tracker tickets"
        );
        // Design-ticket ids of the shape T<digit> must not appear.
        let has_ticket_id = SKILL
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|tok| {
                let mut chars = tok.chars();
                matches!(chars.next(), Some('T'))
                    && tok.len() >= 2
                    && chars.all(|c| c.is_ascii_digit())
            });
        assert!(
            !has_ticket_id,
            "skill must not contain a design-ticket id like T5"
        );
    }
}
