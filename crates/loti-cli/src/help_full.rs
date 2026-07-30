//! The global full-tree help.
//!
//! Invariant: this renders the **whole** command surface from the one `clap`
//! command model in [`crate::cli`] by *walking* it — it never hand-maintains a
//! parallel list of commands, flags, or help text. Every per-flag input rule
//! ("stdin or --file") and the actor requirement on comment operations already
//! live as help text in that model, so walking the model reproduces them for
//! free and they cannot drift.
//!
//! The walk is ordered for an agent reading top to bottom — nouns in workflow
//! order (`init`, then `epic` with its verbs and collections, then `ticket`,
//! then `skill`, then `migrate-store`), each command followed by the commands
//! nested under it. Within a command, `clap`'s own rendered help supplies the
//! usage line and the annotated argument list.

use clap::Command;

/// Render the complete command tree as one page, walking the `clap` model.
///
/// `root` is the top-level [`Command`] (from `Cli::command()`). The output is a
/// single pass: a short header, then each command in workflow order with its
/// full argument help, so an agent gets the entire annotated surface at once.
pub fn render(root: &mut Command) -> String {
    // `clap` computes some rendering lazily; build once so help strings and
    // subcommand metadata are populated before we walk.
    root.build();

    let mut out = String::new();
    out.push_str(&root.render_long_help().to_string());
    out.push_str("\n\n");
    out.push_str(
        "===============================================================================\n",
    );
    out.push_str(" FULL COMMAND TREE\n");
    out.push_str(
        "===============================================================================\n",
    );
    out.push_str(
        "Every command below in workflow order. Each entry shows its usage and the\n\
         full annotated flag list, including the input rule (inline / stdin / --file)\n\
         on payload flags and the actor requirement on comment operations.\n\n",
    );

    // Walk the top-level subcommands in a deliberate workflow order rather than
    // clap's declaration or alphabetical order.
    let bin = root.get_name().to_string();
    let bin_path = [bin];
    for name in workflow_order(root) {
        if let Some(sub) = find_subcommand_mut(root, &name) {
            render_command(sub, &bin_path, &mut out);
        }
    }

    out
}

/// The workflow order for the top-level nouns/commands: setup first, then the
/// two work nouns with everything nested under each, then the reference/admin
/// commands. Any command present in the model but not named here is appended
/// afterwards so nothing is ever silently dropped.
fn workflow_order(root: &Command) -> Vec<String> {
    let preferred = ["init", "epic", "ticket", "skill", "migrate-store"];
    let present: Vec<String> = root
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();

    let mut ordered: Vec<String> = Vec::new();
    for name in preferred {
        if present.iter().any(|p| p == name) {
            ordered.push(name.to_string());
        }
    }
    // Preserve completeness: append any command the preferred list did not
    // mention, so adding a new top-level command still shows up.
    for name in present {
        if !ordered.contains(&name) {
            ordered.push(name);
        }
    }
    ordered
}

/// Render one command and then, depth-first, the commands nested under it.
///
/// `path` is the chain of names from the binary down to this command's parent,
/// so the heading reads as the full invocation (for example
/// `loti ticket comment add`).
fn render_command(cmd: &mut Command, path: &[String], out: &mut String) {
    cmd.build();

    let name = cmd.get_name().to_string();
    let mut full_path = path.to_vec();
    full_path.push(name.clone());
    let heading = full_path.join(" ");

    out.push_str(
        "-------------------------------------------------------------------------------\n",
    );
    out.push_str(&format!("  {heading}\n"));
    out.push_str(
        "-------------------------------------------------------------------------------\n",
    );
    out.push_str(&cmd.render_long_help().to_string());
    out.push_str("\n\n");

    // Recurse into nested commands (verbs under a noun, collection actions under
    // a collection), preserving the model's declaration order within a level.
    let child_names: Vec<String> = cmd
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();
    for child in child_names {
        if let Some(sub) = find_subcommand_mut(cmd, &child) {
            render_command(sub, &full_path, out);
        }
    }
}

/// Borrow a mutable subcommand by name. `clap` exposes mutable subcommand
/// access only via `find_subcommand_mut`, which is what lets us `build()` and
/// render each nested command.
fn find_subcommand_mut<'a>(cmd: &'a mut Command, name: &str) -> Option<&'a mut Command> {
    cmd.find_subcommand_mut(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::CommandFactory;

    fn full() -> String {
        render(&mut Cli::command())
    }

    #[test]
    fn emits_every_noun_verb_and_collection_in_one_pass() {
        let text = full();
        // Top-level nouns/commands.
        for anchor in [
            "loti init",
            "loti epic",
            "loti ticket",
            "loti skill",
            "loti migrate-store",
        ] {
            assert!(text.contains(anchor), "missing command heading: {anchor}");
        }
        // A representative verb under each noun.
        for anchor in [
            "loti epic create",
            "loti epic show",
            "loti ticket create",
            "loti ticket status",
            "loti ticket list",
        ] {
            assert!(text.contains(anchor), "missing verb heading: {anchor}");
        }
        // Collections nested under a noun, including the three-level comment add.
        for anchor in [
            "loti epic label",
            "loti epic comment",
            "loti epic asset",
            "loti ticket comment add",
            "loti ticket asset delete",
        ] {
            assert!(
                text.contains(anchor),
                "missing collection heading: {anchor}"
            );
        }
    }

    #[test]
    fn carries_the_actor_requirement_on_comment_add() {
        let text = full();
        // The comment-add help must state that an actor is required. The
        // annotation lives once, in the clap model, and surfaces here by
        // walking it.
        let idx = text
            .find("loti ticket comment add")
            .expect("comment add heading present");
        let after = &text[idx..];
        assert!(
            after.contains("ACTOR REQUIRED"),
            "comment add must carry the actor requirement"
        );
    }

    #[test]
    fn carries_the_input_rule_on_payload_flags() {
        let text = full();
        // A body/comment/asset payload flag must carry the stdin/--file rule.
        assert!(
            text.contains("stdin or --file") || text.contains("stdin/--file"),
            "payload flags must carry the input rule"
        );
    }

    /// The rendered help for one command: everything from its heading up to the
    /// next command's separator, so an assertion about a command's flags cannot
    /// be satisfied by a different command's.
    fn section<'a>(text: &'a str, heading: &str) -> &'a str {
        let marker = format!("  {heading}\n");
        let start = text
            .find(&marker)
            .unwrap_or_else(|| panic!("missing heading: {heading}"))
            + marker.len();
        // A heading line is followed by its own closing separator line; the help
        // body runs from there to the next command's separator.
        let rest = &text[start..];
        let body_at = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
        let body = &rest[body_at..];
        let end = body.find("-----").unwrap_or(body.len());
        &body[..end]
    }

    #[test]
    fn carries_the_write_precondition_on_the_whole_field_replacements() {
        let text = full();
        // Every command that replaces a whole field offers the precondition, and
        // its help says what a mismatch does rather than merely naming the flag.
        for heading in [
            "loti epic edit",
            "loti ticket edit",
            "loti epic comment edit",
            "loti ticket comment edit",
        ] {
            let s = section(&text, heading);
            assert!(
                s.contains("--expect-updated <STAMP>"),
                "{heading} must offer the write precondition, got: {s}"
            );
            assert!(
                s.contains("refuse and write nothing"),
                "{heading} must state what a mismatch does, got: {s}"
            );
        }
        // An append replaces nobody's text, so it must not advertise a stamp.
        assert!(
            !section(&text, "loti ticket comment add").contains("--expect-updated"),
            "appending a comment takes no expected stamp"
        );
    }

    #[test]
    fn shows_the_root_options_including_help_full() {
        let text = full();
        // The global flags are part of the surface an agent needs.
        assert!(text.contains("--help-full"), "root help lists --help-full");
        assert!(text.contains("--root"), "root help lists --root");
    }
}
