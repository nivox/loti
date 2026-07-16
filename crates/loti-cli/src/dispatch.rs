//! The command adapter: parsed args → core operation → rendered result.
//!
//! This is the only place the CLI grammar meets `loti-core`. Every arm follows
//! the same shape: resolve the data root, read any stdin/`--file` payload,
//! translate flags into the typed inputs a core op expects, call it, and print
//! a short success line. Rules and persistence live entirely in the core, so
//! this module is a thin, testable shell — it takes its stdin and its output
//! sinks by reference, so a test can drive a whole command without a real TTY.
//!
//! Read-side rendering (`show`/`list`) and filtering are out of scope here and
//! remain minimal stubs.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{anyhow, Context, Result};

use loti_core::domain::NodeRef;
use loti_core::ops::{
    self, CommentView, EpicEdits, NewEpic, NewNode, NodeEdits, NodeStatusChange, Target,
};
use loti_core::store::{self, Store};
use loti_core::Actor;

use crate::cli::{
    ActorArg, AssetCommand, Cli, Command, CommentCommand, EpicCommand, InitArgs, LabelCommand,
    TicketCommand,
};
use crate::content_input;

/// Run one parsed command. `stdin`/`stdin_is_tty` feed the content-input
/// helper; `out`/`err` are the success and diagnostic sinks. Errors propagate
/// to the caller, which maps them to a non-zero exit.
pub fn run<R: Read, O: Write, E: Write>(
    cli: &Cli,
    stdin: &mut R,
    stdin_is_tty: bool,
    out: &mut O,
    err: &mut E,
) -> Result<()> {
    match &cli.command {
        Command::Init(args) => run_init(args, out, err),
        Command::Skill => {
            writeln!(err, "loti: skill: not yet implemented")?;
            Ok(())
        }
        Command::MigrateStore => {
            writeln!(err, "loti: migrate-store: not yet implemented")?;
            Ok(())
        }
        Command::Epic(epic) => run_epic(cli, &epic.command, stdin, stdin_is_tty, out, err),
        Command::Ticket(ticket) => run_ticket(cli, &ticket.command, stdin, stdin_is_tty, out, err),
    }
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

/// Create a store, reporting where its markers landed. Init is the one command
/// that runs without an existing store, so it resolves its location from the
/// current directory rather than upward discovery.
fn run_init<O: Write, E: Write>(args: &InitArgs, out: &mut O, err: &mut E) -> Result<()> {
    let here = std::env::current_dir().context("determining the current directory")?;
    if store::inside_git_repo_but_not_root(&here) {
        writeln!(
            err,
            "loti: warning: creating a store here, which is inside a git repository \
             but not at its top level; consider running this at the repository root \
             so the whole checkout shares one store"
        )?;
    }
    let outcome = store::init(&here, args.dir.as_deref())?;
    writeln!(
        out,
        "loti: initialised a store at {}",
        outcome.root.display()
    )?;
    if let Some(pointer) = &outcome.config_pointer {
        writeln!(out, "loti: wrote a pointer to it at {}", pointer.display())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// root resolution
// ---------------------------------------------------------------------------

/// Resolve the data root for a mutating command: an explicit `--root` wins,
/// otherwise discovery walks upward from the current directory. A marker/config
/// disagreement is surfaced as a warning but does not fail the command.
fn open_store<E: Write>(cli: &Cli, err: &mut E) -> Result<Store> {
    let start = std::env::current_dir().context("determining the current directory")?;
    let discovered = loti_core::discovery::resolve(&start, cli.root.as_deref())?;
    if let Some(d) = &discovered.disagreement {
        writeln!(
            err,
            "loti: warning: a config file names {} but a marker directory implies {}; \
             using the config file",
            d.config_root.display(),
            d.marker_root.display()
        )?;
    }
    Ok(Store::at(discovered.root))
}

// ---------------------------------------------------------------------------
// epic verbs
// ---------------------------------------------------------------------------

fn run_epic<R: Read, O: Write, E: Write>(
    cli: &Cli,
    cmd: &EpicCommand,
    stdin: &mut R,
    stdin_is_tty: bool,
    out: &mut O,
    err: &mut E,
) -> Result<()> {
    match cmd {
        EpicCommand::Create(a) => {
            let store = open_store(cli, err)?;
            let body = read_text(a.content.file.as_deref(), stdin, stdin_is_tty, false)?;
            let epic = ops::create_epic(
                &store,
                NewEpic {
                    epic_id: a.epic_id.clone(),
                    name: a.name.clone(),
                    summary: a.summary.clone(),
                    labels: a.label.clone(),
                    body,
                },
            )?;
            writeln!(out, "loti: created epic {}", epic.frontmatter.id)?;
        }
        EpicCommand::Edit(a) => {
            let store = open_store(cli, err)?;
            let body = read_optional_text(a.content.file.as_deref(), stdin, stdin_is_tty)?;
            ops::edit_epic(
                &store,
                &a.id,
                EpicEdits {
                    name: a.name.clone(),
                    summary: a.summary.clone(),
                    body,
                },
            )?;
            writeln!(out, "loti: updated epic {}", a.id)?;
        }
        EpicCommand::Status(a) => {
            let store = open_store(cli, err)?;
            // The clap group guarantees exactly one of closed/open.
            let closed = a.state.closed;
            ops::set_epic_closed(&store, &a.id, closed, a.reason.clone())?;
            writeln!(
                out,
                "loti: epic {} is now {}",
                a.id,
                if closed { "closed" } else { "open" }
            )?;
        }
        EpicCommand::Label(a) => run_label(cli, &a.command, Kind::Epic, out, err)?,
        EpicCommand::Comment(a) => {
            run_comment(cli, &a.command, Kind::Epic, stdin, stdin_is_tty, out, err)?
        }
        EpicCommand::Asset(a) => {
            run_asset(cli, &a.command, Kind::Epic, stdin, stdin_is_tty, out, err)?
        }
        EpicCommand::Show(a) => {
            writeln!(err, "loti: epic show {}: not yet implemented", a.reference)?;
        }
        EpicCommand::List(_) => {
            writeln!(err, "loti: epic list: not yet implemented")?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ticket verbs
// ---------------------------------------------------------------------------

fn run_ticket<R: Read, O: Write, E: Write>(
    cli: &Cli,
    cmd: &TicketCommand,
    stdin: &mut R,
    stdin_is_tty: bool,
    out: &mut O,
    err: &mut E,
) -> Result<()> {
    match cmd {
        TicketCommand::Create(a) => {
            let store = open_store(cli, err)?;
            let body = read_text(a.content.file.as_deref(), stdin, stdin_is_tty, false)?;
            let parent = a
                .parent
                .as_deref()
                .map(NodeRef::parse)
                .transpose()
                .context("parsing --parent")?;
            let node = ops::create_node(
                &store,
                NewNode {
                    epic_id: a.epic_id.clone(),
                    parent,
                    name: a.name.clone(),
                    summary: a.summary.clone(),
                    labels: a.label.clone(),
                    body,
                },
            )?;
            writeln!(
                out,
                "loti: created ticket {}/{}",
                a.epic_id, node.frontmatter.number
            )?;
        }
        TicketCommand::Edit(a) => {
            let store = open_store(cli, err)?;
            let node_ref = NodeRef::parse(&a.reference)?;
            let body = read_optional_text(a.content.file.as_deref(), stdin, stdin_is_tty)?;
            let parent = a.parent.as_deref().map(NodeRef::parse).transpose()?;
            ops::edit_node(
                &store,
                &node_ref,
                NodeEdits {
                    name: a.name.clone(),
                    summary: a.summary.clone(),
                    parent,
                    body,
                },
            )?;
            writeln!(out, "loti: updated ticket {node_ref}")?;
        }
        TicketCommand::Status(a) => {
            let store = open_store(cli, err)?;
            let node_ref = NodeRef::parse(&a.reference)?;
            let change = status_change_from_args(a)?;
            let node = ops::set_node_status(&store, &node_ref, change)?;
            writeln!(
                out,
                "loti: ticket {node_ref} is now {}",
                node.frontmatter.status.wire_name()
            )?;
        }
        TicketCommand::Label(a) => run_label(cli, &a.command, Kind::Ticket, out, err)?,
        TicketCommand::Comment(a) => {
            run_comment(cli, &a.command, Kind::Ticket, stdin, stdin_is_tty, out, err)?
        }
        TicketCommand::Asset(a) => {
            run_asset(cli, &a.command, Kind::Ticket, stdin, stdin_is_tty, out, err)?
        }
        TicketCommand::Show(a) => {
            writeln!(
                err,
                "loti: ticket show {}: not yet implemented",
                a.reference
            )?;
        }
        TicketCommand::List(a) => {
            writeln!(err, "loti: ticket list {}: not yet implemented", a.scope)?;
        }
    }
    Ok(())
}

/// Translate the ticket status flags into the typed core change. The clap group
/// guarantees exactly one state flag is set; `--blocked-by`/`--reason`/
/// `--cascade` are constrained by clap to their owning state.
fn status_change_from_args(a: &crate::cli::TicketStatusArgs) -> Result<NodeStatusChange> {
    let s = &a.state;
    if s.to_do {
        Ok(NodeStatusChange::ToDo)
    } else if s.in_progress {
        Ok(NodeStatusChange::InProgress)
    } else if s.blocked {
        let refs = match &a.blocked_by {
            Some(list) => list
                .split(',')
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .map(NodeRef::parse)
                .collect::<Result<Vec<_>, _>>()
                .context("parsing --blocked-by")?,
            None => Vec::new(),
        };
        Ok(NodeStatusChange::Blocked {
            refs,
            reason: a.reason.clone(),
        })
    } else if s.done {
        Ok(NodeStatusChange::Done)
    } else if s.closed {
        Ok(NodeStatusChange::Closed {
            reason: a.reason.clone(),
            cascade: a.cascade,
        })
    } else {
        // Unreachable: the clap group requires exactly one state.
        Err(anyhow!("no target state was given"))
    }
}

// ---------------------------------------------------------------------------
// shared collections: label / comment / asset
// ---------------------------------------------------------------------------

/// Which noun a collection op is under, so the shared reference resolves to the
/// right kind of target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Epic,
    Ticket,
}

/// Resolve a collection's `<REF>` to a [`Target`]: an epic id under `epic`, a
/// parsed node reference under `ticket`.
fn target_of(kind: Kind, reference: &str) -> Result<Target> {
    Ok(match kind {
        Kind::Epic => Target::Epic(reference.to_string()),
        Kind::Ticket => Target::Node(NodeRef::parse(reference)?),
    })
}

fn run_label<O: Write, E: Write>(
    cli: &Cli,
    cmd: &LabelCommand,
    kind: Kind,
    out: &mut O,
    err: &mut E,
) -> Result<()> {
    match cmd {
        LabelCommand::Add(a) => {
            let store = open_store(cli, err)?;
            let target = target_of(kind, &a.reference)?;
            let labels = ops::add_labels(&store, &target, &a.labels)?;
            writeln!(out, "loti: labels: {}", render_list(&labels))?;
        }
        LabelCommand::Remove(a) => {
            let store = open_store(cli, err)?;
            let target = target_of(kind, &a.reference)?;
            let labels = ops::remove_labels(&store, &target, &a.labels)?;
            writeln!(out, "loti: labels: {}", render_list(&labels))?;
        }
        LabelCommand::List(a) => {
            let store = open_store(cli, err)?;
            let target = target_of(kind, &a.reference)?;
            let labels = ops::list_labels(&store, &target)?;
            for l in labels {
                writeln!(out, "{l}")?;
            }
        }
    }
    Ok(())
}

fn run_comment<R: Read, O: Write, E: Write>(
    cli: &Cli,
    cmd: &CommentCommand,
    kind: Kind,
    stdin: &mut R,
    stdin_is_tty: bool,
    out: &mut O,
    err: &mut E,
) -> Result<()> {
    match cmd {
        CommentCommand::Add(a) => {
            let store = open_store(cli, err)?;
            let target = target_of(kind, &a.reference)?;
            let actor = actor_of(&a.actor)?;
            let text = read_text(a.content.file.as_deref(), stdin, stdin_is_tty, true)?;
            let c = ops::add_comment(&store, &target, actor, text)?;
            writeln!(out, "loti: added comment #{}", c.id)?;
        }
        CommentCommand::Edit(a) => {
            let store = open_store(cli, err)?;
            let target = target_of(kind, &a.reference)?;
            let actor = actor_of(&a.actor)?;
            let text = read_text(a.content.file.as_deref(), stdin, stdin_is_tty, true)?;
            ops::edit_comment(&store, &target, a.comment_id, actor, text)?;
            writeln!(out, "loti: edited comment #{}", a.comment_id)?;
        }
        CommentCommand::Delete(a) => {
            let store = open_store(cli, err)?;
            let target = target_of(kind, &a.reference)?;
            let actor = actor_of(&a.actor)?;
            ops::delete_comment(&store, &target, a.comment_id, actor)?;
            writeln!(out, "loti: deleted comment #{}", a.comment_id)?;
        }
        CommentCommand::List(a) => {
            let store = open_store(cli, err)?;
            let target = target_of(kind, &a.reference)?;
            let views = ops::list_comments(&store, &target, a.include_deleted)?;
            for v in views {
                writeln!(out, "{}", render_comment(&v))?;
            }
        }
    }
    Ok(())
}

fn run_asset<R: Read, O: Write, E: Write>(
    cli: &Cli,
    cmd: &AssetCommand,
    kind: Kind,
    stdin: &mut R,
    stdin_is_tty: bool,
    out: &mut O,
    err: &mut E,
) -> Result<()> {
    match cmd {
        AssetCommand::Add(a) => {
            let store = open_store(cli, err)?;
            let target = target_of(kind, &a.reference)?;
            // The name defaults to the --file basename when --name is omitted.
            let name = resolve_asset_name(a.name.as_deref(), a.content.file.as_deref())?;
            let bytes = read_bytes(a.content.file.as_deref(), stdin, stdin_is_tty, true)?;
            let entry = ops::add_asset(&store, &target, &name, a.description.clone(), &bytes)?;
            writeln!(out, "loti: added asset {}", entry.name)?;
        }
        AssetCommand::Delete(a) => {
            let store = open_store(cli, err)?;
            let target = target_of(kind, &a.reference)?;
            ops::delete_asset(&store, &target, &a.name)?;
            writeln!(out, "loti: deleted asset {}", a.name)?;
        }
        AssetCommand::List(a) => {
            let store = open_store(cli, err)?;
            let target = target_of(kind, &a.reference)?;
            for asset in ops::list_assets(&store, &target)? {
                match &asset.description {
                    Some(d) => writeln!(out, "{}\t{d}", asset.name)?,
                    None => writeln!(out, "{}", asset.name)?,
                }
            }
        }
    }
    Ok(())
}

/// The asset name: explicit `--name`, else the basename of `--file`. Absent
/// both is a clean error (stdin data with no name has nothing to derive from).
fn resolve_asset_name(name: Option<&str>, file: Option<&Path>) -> Result<String> {
    if let Some(n) = name {
        return Ok(n.to_string());
    }
    if let Some(path) = file {
        if let Some(base) = path.file_name().and_then(|s| s.to_str()) {
            if !base.is_empty() {
                return Ok(base.to_string());
            }
        }
    }
    Err(anyhow!(
        "an asset needs a name: pass --name, or --file so its basename can be used"
    ))
}

// ---------------------------------------------------------------------------
// actor & content helpers
// ---------------------------------------------------------------------------

/// Map the clap actor group to a core [`Actor`]. The group guarantees exactly
/// one of `-u`/`-a` is present, so this cannot fail for parsed input.
fn actor_of(arg: &ActorArg) -> Result<Actor> {
    if arg.user {
        Ok(Actor::Human)
    } else if let Some(name) = &arg.agent {
        Ok(Actor::Agent(name.clone()))
    } else {
        Err(anyhow!(
            "an actor is required: pass -u/--user or -a/--agent <name>"
        ))
    }
}

/// Read a payload as UTF-8 text, following the content-input rules. `required`
/// errors when no source is present.
fn read_text<R: Read>(
    file: Option<&Path>,
    stdin: &mut R,
    stdin_is_tty: bool,
    required: bool,
) -> Result<String> {
    let bytes = read_bytes(file, stdin, stdin_is_tty, required)?;
    String::from_utf8(bytes).context("content must be valid UTF-8 text")
}

/// Read an optional text payload: `None` when no source was present (the caller
/// leaves the field unchanged), `Some(text)` otherwise.
fn read_optional_text<R: Read>(
    file: Option<&Path>,
    stdin: &mut R,
    stdin_is_tty: bool,
) -> Result<Option<String>> {
    match content_input::resolve_content(file, stdin, stdin_is_tty, false)? {
        Some(bytes) => Ok(Some(
            String::from_utf8(bytes).context("content must be valid UTF-8 text")?,
        )),
        None => Ok(None),
    }
}

/// Read a payload as raw bytes, following the content-input rules. An absent
/// optional source yields empty bytes.
fn read_bytes<R: Read>(
    file: Option<&Path>,
    stdin: &mut R,
    stdin_is_tty: bool,
    required: bool,
) -> Result<Vec<u8>> {
    Ok(content_input::resolve_content(file, stdin, stdin_is_tty, required)?.unwrap_or_default())
}

// ---------------------------------------------------------------------------
// tiny renderers (full read-side rendering is a later ticket)
// ---------------------------------------------------------------------------

/// Render a label list inline, or a placeholder when empty.
fn render_list(labels: &[String]) -> String {
    if labels.is_empty() {
        "(none)".to_string()
    } else {
        labels.join(", ")
    }
}

/// Render one comment view: a live comment as `#id author: text`, a tombstone
/// with its text withheld.
fn render_comment(view: &CommentView) -> String {
    match view {
        CommentView::Live(c) => format!("#{} {}: {}", c.id, c.author, c.text),
        CommentView::Tombstone { id, author, .. } => {
            format!("#{id} {author}: (deleted)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;
    use std::io::Cursor;
    use std::path::PathBuf;

    /// Build a `Cli` from argv, injecting `--root <root>` so tests never touch
    /// discovery or the current directory.
    fn cli_with_root(root: &Path, argv: &[&str]) -> Cli {
        let mut full = vec!["loti", "--root", root.to_str().unwrap()];
        full.extend_from_slice(argv);
        Cli::try_parse_from(full).expect("args should parse")
    }

    /// Run a command with the given stdin bytes, treating stdin as piped (not a
    /// TTY), and return `(stdout, stderr, result)`. A failed operation returns
    /// its error (as `main` would render it), not written into `stderr`.
    fn invoke(cli: &Cli, stdin: &[u8]) -> (String, String, Result<()>) {
        let mut input = Cursor::new(stdin.to_vec());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = run(cli, &mut input, false, &mut out, &mut err);
        (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            result,
        )
    }

    /// A store initialised at a fresh temp root, with the root path returned.
    fn init_store() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // Write metadata so the version gate sees a current store.
        loti_core::meta::write(&root, &loti_core::meta::Meta::current()).unwrap();
        (dir, root)
    }

    #[test]
    fn create_epic_and_ticket_end_to_end() {
        let (_d, root) = init_store();

        let cli = cli_with_root(
            &root,
            &["epic", "create", "e", "--name", "n", "--summary", "s"],
        );
        let (out, _e, r) = invoke(&cli, b"epic body from stdin");
        assert!(r.is_ok(), "epic create should succeed");
        assert!(out.contains("created epic e"), "got: {out}");

        // The body came from stdin.
        let store = Store::at(&root);
        assert_eq!(store.read_epic("e").unwrap().body, "epic body from stdin");

        let cli = cli_with_root(
            &root,
            &["ticket", "create", "e", "--name", "t", "--summary", "s"],
        );
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        assert!(out.contains("created ticket e/1"), "got: {out}");
    }

    #[test]
    fn ticket_status_done_refused_with_open_child_exits_nonzero() {
        let (_d, root) = init_store();
        let store = Store::at(&root);
        ops::create_epic(&store, test_epic("e")).unwrap();
        let parent = ops::create_node(&store, test_node("e", None)).unwrap();
        ops::create_node(
            &store,
            test_node("e", Some(NodeRef::new("e", parent.frontmatter.number))),
        )
        .unwrap();

        let cli = cli_with_root(&root, &["ticket", "status", "e/1", "--done"]);
        let (_o, _e, r) = invoke(&cli, b"");
        let msg = r
            .expect_err("done with an open child must fail")
            .to_string();
        assert!(msg.contains("still open"), "got: {msg}");
    }

    #[test]
    fn comment_add_requires_stdin_or_file_and_records_actor() {
        let (_d, root) = init_store();
        let store = Store::at(&root);
        ops::create_epic(&store, test_epic("e")).unwrap();

        let cli = cli_with_root(&root, &["epic", "comment", "add", "e", "-a", "bot"]);
        let (out, _e, r) = invoke(&cli, b"a comment body");
        assert!(r.is_ok());
        assert!(out.contains("added comment #1"), "got: {out}");

        let comments = store.read_epic("e").unwrap().frontmatter.comments;
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, Actor::Agent("bot".into()));
        assert_eq!(comments[0].text, "a comment body");
    }

    #[test]
    fn asset_name_defaults_to_file_basename() {
        let (_d, root) = init_store();
        let store = Store::at(&root);
        ops::create_epic(&store, test_epic("e")).unwrap();

        // Write a payload file whose basename becomes the asset name.
        let payload = root.join("evidence.log");
        std::fs::write(&payload, b"log data").unwrap();
        let cli = cli_with_root(
            &root,
            &[
                "epic",
                "asset",
                "add",
                "e",
                "--file",
                payload.to_str().unwrap(),
            ],
        );
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok(), "asset add should succeed");
        assert!(out.contains("added asset evidence.log"), "got: {out}");
        assert!(store.epic_asset_dir("e").join("evidence.log").is_file());
    }

    #[test]
    fn label_add_and_list() {
        let (_d, root) = init_store();
        let store = Store::at(&root);
        ops::create_epic(&store, test_epic("e")).unwrap();

        let cli = cli_with_root(&root, &["epic", "label", "add", "e", "alpha", "beta"]);
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        assert!(out.contains("alpha, beta"), "got: {out}");

        let cli = cli_with_root(&root, &["epic", "label", "list", "e"]);
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        assert!(out.contains("alpha"));
        assert!(out.contains("beta"));
    }

    #[test]
    fn asset_name_missing_without_file_is_an_error() {
        assert!(resolve_asset_name(None, None).is_err());
        assert_eq!(
            resolve_asset_name(Some("x"), None).unwrap(),
            "x".to_string()
        );
        assert_eq!(
            resolve_asset_name(None, Some(Path::new("/a/b/c.png"))).unwrap(),
            "c.png".to_string()
        );
    }

    /// A minimal epic input for seeding a store directly through the core.
    fn test_epic(id: &str) -> NewEpic {
        NewEpic {
            epic_id: id.to_string(),
            name: "n".into(),
            summary: "s".into(),
            labels: vec![],
            body: String::new(),
        }
    }

    /// A minimal node input for seeding a store directly through the core.
    fn test_node(epic: &str, parent: Option<NodeRef>) -> NewNode {
        NewNode {
            epic_id: epic.to_string(),
            parent,
            name: "n".into(),
            summary: "s".into(),
            labels: vec![],
            body: String::new(),
        }
    }
}
