//! `loti` binary entrypoint.
//!
//! This parses the single declarative command-tree (see [`cli`]), plumbs
//! `--root`, and routes to the shared content-input helper. `init` is wired
//! end-to-end; the remaining verbs are still stubs that report what they
//! *would* do.

mod cli;
mod content_input;

use anyhow::Context;
use clap::Parser;

use cli::{
    AssetCommand, Cli, Command, CommentCommand, EpicCommand, InitArgs, LabelCommand, TicketCommand,
};
use loti_core::store;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let root = cli
        .root
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<discovered>".to_string());

    match &cli.command {
        Command::Init(args) => run_init(args)?,
        Command::Skill => stub("skill (prints the static SKILL.md)"),
        Command::MigrateStore => stub("migrate-store"),
        Command::Epic(epic) => match &epic.command {
            EpicCommand::Create(a) => {
                let _body = content_input::read_content(a.content.file.as_deref(), false)?;
                stub(&format!("epic create {} (root={root})", a.epic_id));
            }
            EpicCommand::Show(a) => stub(&format!("epic show {} (root={root})", a.reference)),
            EpicCommand::Edit(a) => {
                let _body = content_input::read_content(a.content.file.as_deref(), false)?;
                stub(&format!("epic edit {} (root={root})", a.id));
            }
            EpicCommand::Status(a) => stub(&format!("epic status {} (root={root})", a.id)),
            EpicCommand::Label(a) => stub(&format!(
                "epic label {:?} (root={root})",
                verb_of_label(&a.command)
            )),
            EpicCommand::Comment(a) => dispatch_comment(&a.command, &root)?,
            EpicCommand::Asset(a) => dispatch_asset(&a.command, &root)?,
            EpicCommand::List(_) => stub(&format!("epic list (root={root})")),
        },
        Command::Ticket(ticket) => match &ticket.command {
            TicketCommand::Create(a) => {
                let _body = content_input::read_content(a.content.file.as_deref(), false)?;
                stub(&format!("ticket create {} (root={root})", a.epic_id));
            }
            TicketCommand::Show(a) => stub(&format!("ticket show {} (root={root})", a.reference)),
            TicketCommand::Edit(a) => {
                let _body = content_input::read_content(a.content.file.as_deref(), false)?;
                stub(&format!("ticket edit {} (root={root})", a.reference));
            }
            TicketCommand::Status(a) => {
                stub(&format!("ticket status {} (root={root})", a.reference))
            }
            TicketCommand::Label(a) => stub(&format!(
                "ticket label {:?} (root={root})",
                verb_of_label(&a.command)
            )),
            TicketCommand::Comment(a) => dispatch_comment(&a.command, &root)?,
            TicketCommand::Asset(a) => dispatch_asset(&a.command, &root)?,
            TicketCommand::List(a) => stub(&format!("ticket list {} (root={root})", a.scope)),
        },
    }

    Ok(())
}

/// Create a store, then report where its markers landed.
///
/// Init is the one command that runs without an existing store, so it resolves
/// its location from the current directory rather than upward discovery.
fn run_init(args: &InitArgs) -> anyhow::Result<()> {
    let here = std::env::current_dir().context("determining the current directory")?;

    // A store is best kept at a repository's root so a whole checkout shares it.
    if store::inside_git_repo_but_not_root(&here) {
        eprintln!(
            "loti: warning: creating a store here, which is inside a git repository \
             but not at its top level; consider running this at the repository root \
             so the whole checkout shares one store"
        );
    }

    let outcome = store::init(&here, args.dir.as_deref())?;
    println!("loti: initialised a store at {}", outcome.root.display());
    if let Some(pointer) = &outcome.config_pointer {
        println!("loti: wrote a pointer to it at {}", pointer.display());
    }
    Ok(())
}

fn dispatch_comment(cmd: &CommentCommand, root: &str) -> anyhow::Result<()> {
    match cmd {
        CommentCommand::Add(a) => {
            // Comment text is required content.
            let _text = content_input::read_content(a.content.file.as_deref(), true)?;
            stub(&format!("comment add {} (root={root})", a.reference));
        }
        CommentCommand::Edit(a) => {
            let _text = content_input::read_content(a.content.file.as_deref(), true)?;
            stub(&format!(
                "comment edit {} #{} (root={root})",
                a.reference, a.comment_id
            ));
        }
        CommentCommand::Delete(a) => stub(&format!(
            "comment delete {} #{} (root={root})",
            a.reference, a.comment_id
        )),
        CommentCommand::List(a) => stub(&format!("comment list {} (root={root})", a.reference)),
    }
    Ok(())
}

fn dispatch_asset(cmd: &AssetCommand, root: &str) -> anyhow::Result<()> {
    match cmd {
        AssetCommand::Add(a) => {
            let _data = content_input::read_content(a.content.file.as_deref(), false)?;
            stub(&format!("asset add {} (root={root})", a.reference));
        }
        AssetCommand::Delete(a) => stub(&format!(
            "asset delete {} {} (root={root})",
            a.reference, a.name
        )),
        AssetCommand::List(a) => stub(&format!("asset list {} (root={root})", a.reference)),
    }
    Ok(())
}

fn verb_of_label(cmd: &LabelCommand) -> &'static str {
    match cmd {
        LabelCommand::Add(_) => "add",
        LabelCommand::Remove(_) => "remove",
        LabelCommand::List(_) => "list",
    }
}

fn stub(what: &str) {
    eprintln!("loti: {what}: not yet implemented");
}
