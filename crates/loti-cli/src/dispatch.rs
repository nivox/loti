//! The command adapter: parsed args → core operation → rendered result.
//!
//! This is the only place the CLI grammar meets `loti-core`. Every arm follows
//! the same shape: resolve the data root, read any stdin/`--file` payload,
//! translate flags into the typed inputs a core op expects, call it, and print
//! a short success line. Rules and persistence live entirely in the core, so
//! this module is a thin, testable shell — it takes its stdin and its output
//! sinks by reference, so a test can drive a whole command without a real TTY.
//!
//! Read-side rendering (`show`/`list`) resolves the data through `loti_core`'s
//! read layer and renders it with `loti_core::render`; the structured filter
//! families for `list` are a later layer.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{anyhow, Context, Result};

use loti_core::domain::NodeRef;
use loti_core::filter::{self, FilterInput, StructuredFilters};
use loti_core::matcher::{self, MatcherRegistry};
use loti_core::ops::{
    self, CommentView, EpicEdits, NewEpic, NewNode, NodeEdits, NodeStatusChange, Target,
};
use loti_core::read::{self, ListScope, MatchRequest};
use loti_core::render::{self, Color, Projection};
use loti_core::store::{self, Store};
use loti_core::Actor;

use crate::cli::{
    ActorArg, AssetCommand, Cli, Command, CommentCommand, EpicCommand, FieldSel, InitArgs,
    LabelCommand, ListFilterArgs, ListFormat, MigrateStoreArgs, ShowArgs, ShowFormat,
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
    stdout_is_tty: bool,
    out: &mut O,
    err: &mut E,
) -> Result<()> {
    match &cli.command {
        Command::Init(args) => run_init(args, out, err),
        Command::Skill => {
            // Printed verbatim: the skill is static, hand-authored prose, not a
            // rendering of the command model.
            write!(out, "{}", crate::skill::SKILL)?;
            Ok(())
        }
        Command::MigrateStore(a) => run_migrate_store(cli, a, out, err),
        Command::Epic(epic) => run_epic(
            cli,
            &epic.command,
            stdin,
            stdin_is_tty,
            stdout_is_tty,
            out,
            err,
        ),
        Command::Ticket(ticket) => run_ticket(
            cli,
            &ticket.command,
            stdin,
            stdin_is_tty,
            stdout_is_tty,
            out,
            err,
        ),
    }
}

/// The colour policy for the plain-text `list`: ANSI only on an interactive
/// terminal, never when piped or redirected.
fn color_for(stdout_is_tty: bool) -> Color {
    if stdout_is_tty {
        Color::Ansi
    } else {
        Color::None
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

/// A progress sink that writes each migration step as a `loti:` line to the
/// diagnostic stream, so a human sees the sentinel, drain, transform and commit
/// as they happen.
struct WriterProgress<'a, E: Write> {
    err: &'a mut E,
}
impl<E: Write> loti_core::migrate::Progress for WriterProgress<'_, E> {
    fn step(&mut self, message: &str) {
        // A progress line failing to write must not abort a migration.
        let _ = writeln!(self.err, "loti: {message}");
    }
}

/// Bring an older on-disk store up to the version this binary writes, or resume
/// an interrupted migration. Runs without upward version gating on the store
/// itself: a mid-migration store is exactly what this command is here to finish.
fn run_migrate_store<O: Write, E: Write>(
    cli: &Cli,
    args: &MigrateStoreArgs,
    out: &mut O,
    err: &mut E,
) -> Result<()> {
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
    let root = discovered.root;

    // No concrete cross-major transforms exist yet, so the registry is empty;
    // the machinery (sentinel/drain/snapshot/swap/commit) and the minor-bump
    // and no-op paths are all live. A future breaking change registers its
    // per-major transform here.
    let registry = loti_core::migrate::TransformRegistry::new();
    let config = loti_core::migrate::MigrateConfig {
        force: if args.force {
            loti_core::migrate::Force::Force
        } else {
            loti_core::migrate::Force::Deny
        },
        ..Default::default()
    };

    let outcome = {
        let mut progress = WriterProgress { err };
        loti_core::migrate::migrate_store(&root, &registry, &config, &mut progress)?
    };

    match outcome {
        loti_core::migrate::Outcome::AlreadyCurrent => {
            writeln!(out, "loti: the store is already at this loti's format")?;
        }
        loti_core::migrate::Outcome::MinorBumped { from, to } => {
            writeln!(
                out,
                "loti: updated the store's format version from {}.{} to {}.{} (no rewrite needed)",
                from.0, from.1, to.0, to.1
            )?;
        }
        loti_core::migrate::Outcome::Migrated { from, to, steps } => {
            writeln!(
                out,
                "loti: migrated the store from format {}.{} to {}.{} ({} step(s)); \
                 the pre-migration copy was kept alongside it",
                from.0, from.1, to.0, to.1, steps
            )?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// root resolution
// ---------------------------------------------------------------------------

/// Resolve the data root for a command: an explicit `--root` wins, otherwise
/// discovery walks upward from the current directory. A marker/config
/// disagreement is surfaced as a warning but does not fail the command.
///
/// Every command refuses up front a store whose major format is newer than this
/// binary understands — such a store is never read-guessed nor written. An older
/// major and a mid-migration store still open (they are readable); only their
/// mutations are refused later, by the store's mutation gate.
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
    let store = Store::at(discovered.root);
    store.verify_readable().map_err(|e| anyhow!("{e}"))?;
    Ok(store)
}

// ---------------------------------------------------------------------------
// epic verbs
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn run_epic<R: Read, O: Write, E: Write>(
    cli: &Cli,
    cmd: &EpicCommand,
    stdin: &mut R,
    stdin_is_tty: bool,
    stdout_is_tty: bool,
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
            let store = open_store(cli, err)?;
            show_epic(&store, a, out)?;
        }
        EpicCommand::List(a) => {
            let store = open_store(cli, err)?;
            list_epics(&store, &a.fields, &a.format, color_for(stdout_is_tty), out)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ticket verbs
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn run_ticket<R: Read, O: Write, E: Write>(
    cli: &Cli,
    cmd: &TicketCommand,
    stdin: &mut R,
    stdin_is_tty: bool,
    stdout_is_tty: bool,
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
            let store = open_store(cli, err)?;
            show_node(&store, a, out)?;
        }
        TicketCommand::List(a) => {
            let store = open_store(cli, err)?;
            list_tickets(&store, a, color_for(stdout_is_tty), out, err)?;
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
// show (the sole projectable reader)
// ---------------------------------------------------------------------------

/// Render `epic show` in the requested mode, applying any `--field`/`--fields`
/// projection. Markdown is the default; JSON is the canonical form; raw is
/// strict-unambiguous leaves.
fn show_epic<O: Write>(store: &Store, a: &ShowArgs, out: &mut O) -> Result<()> {
    let value = read::epic_json(store, &a.reference)?;
    let children = read::epic_children(store, &a.reference)?;
    let comments = read::comment_lines(store, &Target::Epic(a.reference.clone()), false)?;
    let text = render_show(&value, &a.format, &a.fields, &children, &comments)?;
    writeln!(out, "{text}")?;
    Ok(())
}

/// Render `ticket show` in the requested mode. The direct-children table lists
/// the node's direct child nodes.
fn show_node<O: Write>(store: &Store, a: &ShowArgs, out: &mut O) -> Result<()> {
    let node_ref = NodeRef::parse(&a.reference)?;
    let value = read::node_json(store, &node_ref)?;
    let children = read::node_children(store, &node_ref)?;
    let comments = read::comment_lines(store, &Target::Node(node_ref.clone()), false)?;
    let text = render_show(&value, &a.format, &a.fields, &children, &comments)?;
    writeln!(out, "{text}")?;
    Ok(())
}

/// Dispatch a resolved value to the chosen `show` mode. Markdown is the default
/// when no mode flag is given.
fn render_show(
    value: &serde_json::Value,
    format: &ShowFormat,
    fields: &FieldSel,
    children: &[render::ChildRow],
    comments: &[render::CommentLine],
) -> Result<String> {
    let projection = projection_of(fields);
    if format.json {
        Ok(render::show_json(value, &projection)?)
    } else if format.raw {
        Ok(render::show_raw(value, &projection)?)
    } else {
        // Markdown is the default. A projection narrows markdown to JSON of the
        // selected leaves — markdown is the whole-entity view, so a field
        // selection is served as its canonical value rather than a partial
        // document.
        match projection {
            Projection::Whole => Ok(render::show_markdown(value, children, comments)),
            _ => Ok(render::show_json(value, &projection)?),
        }
    }
}

/// Translate the `--field`/`--fields` group into a [`Projection`]. The clap
/// group guarantees at most one is set; `--fields` splits on commas.
fn projection_of(fields: &FieldSel) -> Projection {
    if let Some(one) = &fields.field {
        Projection::One(one.clone())
    } else if let Some(many) = &fields.fields {
        let paths: Vec<String> = many
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        Projection::Many(paths)
    } else {
        Projection::Whole
    }
}

// ---------------------------------------------------------------------------
// list (the roster reader; never presents)
// ---------------------------------------------------------------------------

/// Render `epic list` — the flat roster of every epic — in the chosen mode.
/// The roster has no scope depth (`--shallow` lives on `ticket list`); heavy
/// `--fields` are rejected because the roster serves summary fields only.
fn list_epics<O: Write>(
    store: &Store,
    fields: &FieldSel,
    format: &ListFormat,
    color: Color,
    out: &mut O,
) -> Result<()> {
    // A `--fields` selection on list is restricted to summary fields; a
    // heavy/structured field is a hard error (those are show-only).
    let selected = list_field_paths(fields);
    if !selected.is_empty() {
        render::validate_list_fields(&selected, render::LISTABLE_EPIC_FIELDS)?;
    }
    let epics = read::list_epics(store)?;
    let text = if format.json {
        render::list_epics_json(&epics)
    } else if format.ndjson {
        render::list_epics_ndjson(&epics)
    } else if format.raw {
        render::list_epics_raw(&epics)
    } else {
        render::list_epics_plain(&epics, color)
    };
    write!(out, "{text}")?;
    Ok(())
}

/// Render `ticket list <scope>` in the chosen mode. Scope is a whole epic
/// (`<epic-id>`) or under a node (`<epic-id>/<n>`); the default is the full tree
/// rooted at the scope, and `--shallow` keeps only its immediate level. The
/// plain form is a depth-first indented tree; the flat forms carry parent
/// pointers.
fn list_tickets<O: Write, E: Write>(
    store: &Store,
    a: &crate::cli::TicketListArgs,
    color: Color,
    out: &mut O,
    err: &mut E,
) -> Result<()> {
    let selected = list_field_paths(&a.fields);
    if !selected.is_empty() {
        render::validate_list_fields(&selected, render::LISTABLE_NODE_FIELDS)?;
    }
    let scope = parse_list_scope(&a.scope, a.shallow)?;

    // Validate the label/state families up front (conflicts, unknown states),
    // then apply scope → structured → match with AND across families.
    let filters = build_filters(&a.filters)?;
    let matching = build_match_request(&a.filters);
    let matchers = build_matcher_registry(cli_root_hint(store));
    let result = read::list_nodes_filtered(store, &scope, &filters, matching.as_ref(), &matchers)?;
    // Non-fatal match warnings (e.g. a matcher returning a path outside the
    // candidate set) are surfaced without failing the list.
    for warning in &result.warnings {
        writeln!(err, "loti: warning: {warning}")?;
    }
    let nodes = result.nodes;

    // With a field selection the flat/raw and plain forms narrow to those
    // columns; json/ndjson keep the full listable row.
    let text = if !selected.is_empty() && (a.format.raw || is_plain(&a.format)) {
        render::list_nodes_fields_raw(&nodes, &selected)
    } else if a.format.json {
        render::list_nodes_json(&nodes)
    } else if a.format.ndjson {
        render::list_nodes_ndjson(&nodes)
    } else if a.format.raw {
        render::list_nodes_raw(&nodes)
    } else {
        // The default plain tree carries a per-status progress footer; the
        // `filtered` flag tags it when a label/status/match family narrowed the
        // set, so a partial count is never read as the whole scope.
        let mut tree = render::list_nodes_plain(&nodes, color);
        tree.push_str(&render::list_summary(
            &nodes,
            filters_narrowed(&a.filters),
            color,
        ));
        tree
    };
    write!(out, "{text}")?;
    Ok(())
}

/// Whether any structured or match filter narrowed the listing. Scope and
/// `--shallow` select what to list and are not filters, so they never set this.
fn filters_narrowed(f: &ListFilterArgs) -> bool {
    !f.label.is_empty()
        || !f.not_label.is_empty()
        || !f.status.is_empty()
        || !f.not_status.is_empty()
        || f.open
        || f.resolved
        || f.match_query.is_some()
}

/// Whether a list format is the default plain text (no machine-mode flag set).
fn is_plain(format: &ListFormat) -> bool {
    !format.json && !format.ndjson && !format.raw
}

/// Validate and normalise the label/state filter flags into the core's
/// structured-filter value, surfacing usage errors (conflicts, unknown states)
/// before any store access.
fn build_filters(args: &ListFilterArgs) -> Result<StructuredFilters> {
    let input = FilterInput {
        labels: args.label.clone(),
        not_labels: args.not_label.clone(),
        states: args.status.clone(),
        not_states: args.not_status.clone(),
        open: args.open,
        resolved: args.resolved,
    };
    Ok(filter::parse_filters(&input)?)
}

/// The match request, if `--match` was given. The implementation defaults to the
/// reserved built-in `regex` when `--match-impl` is absent.
fn build_match_request(args: &ListFilterArgs) -> Option<MatchRequest> {
    args.match_query.as_ref().map(|query| MatchRequest {
        query: query.clone(),
        impl_name: args
            .match_impl
            .clone()
            .unwrap_or_else(|| filter::BUILTIN_MATCHER_NAME.to_string()),
    })
}

/// Build the external-matcher registry by layering the user-global XDG config
/// under the project config, project winning on a name collision. The project
/// config is the nearest `.loti.conf` at or above the store root.
fn build_matcher_registry(store_root: &Path) -> MatcherRegistry {
    let user_global = matcher::user_global_config_path();
    let project = loti_core::discovery::find_project_config(store_root);
    MatcherRegistry::layered(user_global.as_deref(), project.as_deref())
}

/// The store root, as the starting point for finding the project config file.
fn cli_root_hint(store: &Store) -> &Path {
    store.root()
}

/// The dotted field paths from a `--field`/`--fields` selection on list, empty
/// when neither is given.
fn list_field_paths(fields: &FieldSel) -> Vec<String> {
    match projection_of(fields) {
        Projection::Whole => Vec::new(),
        Projection::One(p) => vec![p],
        Projection::Many(ps) => ps,
    }
}

/// Parse a `ticket list` scope: `<epic-id>` selects a whole epic, `<epic-id>/<n>`
/// selects the nodes under that node. Each defaults to the full tree rooted
/// there; `shallow` keeps only the immediate level. A scope with more than one
/// `/` is rejected — a reference never nests.
fn parse_list_scope(scope: &str, shallow: bool) -> Result<ListScope> {
    if scope.contains('/') {
        let node = NodeRef::parse(scope)?;
        Ok(ListScope::Under { node, shallow })
    } else {
        Ok(ListScope::Epic {
            id: scope.to_string(),
            shallow,
        })
    }
}

// ---------------------------------------------------------------------------
// tiny renderers (used by the collection list verbs)
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
        // stdin is piped (not a TTY) and stdout is not a TTY, so no colour is
        // emitted — which is what a test wants to assert against plain text.
        let mut input = Cursor::new(stdin.to_vec());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = run(cli, &mut input, false, false, &mut out, &mut err);
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

    // -- read side: show / list -------------------------------------------

    /// Seed a small tree: epic `e` with node 1 (in-progress) and its child 2.
    fn seed_tree(root: &Path) {
        let store = Store::at(root);
        ops::create_epic(&store, test_epic("e")).unwrap();
        let a = ops::create_node(&store, test_node("e", None)).unwrap();
        let ar = NodeRef::new("e", a.frontmatter.number);
        ops::set_node_status(&store, &ar, NodeStatusChange::InProgress).unwrap();
        ops::create_node(&store, test_node("e", Some(ar))).unwrap();
    }

    #[test]
    fn show_json_is_canonical_with_all_fields() {
        let (_d, root) = init_store();
        seed_tree(&root);
        let cli = cli_with_root(&root, &["ticket", "show", "e/1", "--json"]);
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok(), "show --json should succeed");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ref"], "e/1");
        assert_eq!(v["status"], "in-progress");
        // Every canonical field is present, including body and timestamps.
        for key in [
            "number", "name", "summary", "labels", "body", "created", "updated",
        ] {
            assert!(v.get(key).is_some(), "missing {key} in {out}");
        }
    }

    #[test]
    fn show_epic_json_carries_computed_state() {
        let (_d, root) = init_store();
        seed_tree(&root);
        let cli = cli_with_root(&root, &["epic", "show", "e", "--json"]);
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["status"], "open");
        assert_eq!(v["nodes"], 2);
    }

    #[test]
    fn show_markdown_emits_sections_in_order() {
        let (_d, root) = init_store();
        seed_tree(&root);
        let cli = cli_with_root(&root, &["ticket", "show", "e/1"]);
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        // Metadata table, then the H1 name, summary blockquote, children,
        // assets, body, comments — in that order.
        let meta = out.find("| field | value |").unwrap();
        let h1 = out.find("\n# ").unwrap();
        let subs = out.find("## Subtickets").unwrap();
        let assets = out.find("## Assets").unwrap();
        let body = out.find("## Body").unwrap();
        let comments = out.find("## Comments").unwrap();
        assert!(
            meta < h1 && h1 < subs && subs < assets && assets < body && body < comments,
            "sections out of order:\n{out}"
        );
        // The direct child is tabulated.
        assert!(out.contains("e/2"), "child row missing:\n{out}");
    }

    #[test]
    fn show_raw_single_leaf_ok_and_structured_is_a_hard_error() {
        let (_d, root) = init_store();
        seed_tree(&root);
        // A single leaf renders one value, unquoted.
        let cli = cli_with_root(
            &root,
            &["ticket", "show", "e/1", "--raw", "--field", "status"],
        );
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        assert_eq!(out.trim(), "in-progress");
        // Selecting a whole structured field is ambiguous — a hard error.
        let cli = cli_with_root(
            &root,
            &["ticket", "show", "e/1", "--raw", "--field", "assets"],
        );
        let (_o, _e, r) = invoke(&cli, b"");
        let msg = r.expect_err("structured raw must error").to_string();
        assert!(
            msg.contains("--json"),
            "error should point at --json: {msg}"
        );
    }

    #[test]
    fn show_fields_dotted_projection() {
        let (_d, root) = init_store();
        let store = Store::at(&root);
        ops::create_epic(&store, test_epic("e")).unwrap();
        let n = ops::create_node(&store, test_node("e", None)).unwrap();
        let nr = NodeRef::new("e", n.frontmatter.number);
        ops::add_comment(
            &store,
            &Target::Node(nr.clone()),
            Actor::Agent("bot".into()),
            "hi".into(),
        )
        .unwrap();
        // A dotted leaf path over a repeated field distributes to one value.
        let cli = cli_with_root(
            &root,
            &[
                "ticket",
                "show",
                "e/1",
                "--raw",
                "--field",
                "comments.author",
            ],
        );
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok(), "projection should succeed");
        assert_eq!(out.trim(), "agent:bot");
    }

    #[test]
    fn list_plain_is_depth_first_indented_with_blocked_tag() {
        let (_d, root) = init_store();
        let store = Store::at(&root);
        ops::create_epic(&store, test_epic("e")).unwrap();
        let a = ops::create_node(&store, test_node("e", None)).unwrap();
        let ar = NodeRef::new("e", a.frontmatter.number);
        let b = ops::create_node(&store, test_node("e", Some(ar.clone()))).unwrap();
        let br = NodeRef::new("e", b.frontmatter.number);
        ops::set_node_status(
            &store,
            &br,
            NodeStatusChange::Blocked {
                refs: vec![],
                reason: Some("waiting".into()),
            },
        )
        .unwrap();
        let cli = cli_with_root(&root, &["ticket", "list", "e"]);
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        let lines: Vec<&str> = out.lines().collect();
        // Parent first at depth 0, child indented under it (depth-first).
        assert!(lines[0].starts_with("e/1 "), "got: {:?}", lines);
        assert!(
            lines[1].starts_with("  e/2 "),
            "child should be indented: {:?}",
            lines
        );
        assert!(
            lines[1].contains("[blocked: waiting]"),
            "blocked tag missing: {:?}",
            lines
        );
    }

    #[test]
    fn list_json_is_flat_with_parent_pointers() {
        let (_d, root) = init_store();
        seed_tree(&root);
        let cli = cli_with_root(&root, &["ticket", "list", "e", "--json"]);
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().expect("flat array");
        assert_eq!(arr.len(), 2);
        // Flat, never nested: each row carries a parent pointer, no children key.
        let child = arr.iter().find(|r| r["ref"] == "e/2").unwrap();
        assert_eq!(child["parent"], "e/1");
        assert!(child.get("children").is_none());
    }

    #[test]
    fn list_ndjson_is_one_object_per_line() {
        let (_d, root) = init_store();
        seed_tree(&root);
        let cli = cli_with_root(&root, &["ticket", "list", "e", "--ndjson"]);
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        for l in lines {
            let _: serde_json::Value = serde_json::from_str(l).expect("each line is JSON");
        }
    }

    #[test]
    fn list_raw_is_tab_separated() {
        let (_d, root) = init_store();
        seed_tree(&root);
        let cli = cli_with_root(&root, &["ticket", "list", "e", "--raw"]);
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        let first = out.lines().next().unwrap();
        assert!(
            first.contains('\t'),
            "raw rows are tab-separated: {first:?}"
        );
        assert!(first.starts_with("e/1\t"), "got: {first:?}");
    }

    #[test]
    fn list_heavy_field_is_a_hard_error() {
        let (_d, root) = init_store();
        seed_tree(&root);
        let cli = cli_with_root(&root, &["ticket", "list", "e", "--field", "body"]);
        let (_o, _e, r) = invoke(&cli, b"");
        let msg = r.expect_err("heavy field on list must error").to_string();
        assert!(msg.contains("body"), "got: {msg}");
    }

    #[test]
    fn epic_list_roster_and_json() {
        let (_d, root) = init_store();
        seed_tree(&root);
        let cli = cli_with_root(&root, &["epic", "list", "--json"]);
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "e");
        assert_eq!(arr[0]["nodes"], 2);
    }

    /// Seed an epic with three labelled nodes of varied state for filter tests:
    ///   e/1 in-progress [urgent]     e/2 to-do [later]     e/3 done [urgent]
    fn seed_filterable(root: &Path) {
        let store = Store::at(root);
        ops::create_epic(&store, test_epic("e")).unwrap();
        let mut a = test_node("e", None);
        a.name = "alpha needle".into();
        a.labels = vec!["urgent".into()];
        let an = ops::create_node(&store, a).unwrap();
        ops::set_node_status(
            &store,
            &NodeRef::new("e", an.frontmatter.number),
            NodeStatusChange::InProgress,
        )
        .unwrap();
        let mut b = test_node("e", None);
        b.name = "beta".into();
        b.labels = vec!["later".into()];
        ops::create_node(&store, b).unwrap();
        let mut c = test_node("e", None);
        c.name = "gamma needle".into();
        c.labels = vec!["urgent".into()];
        let cn = ops::create_node(&store, c).unwrap();
        ops::set_node_status(
            &store,
            &NodeRef::new("e", cn.frontmatter.number),
            NodeStatusChange::Done,
        )
        .unwrap();
    }

    fn refs_in(out: &str) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(out).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .map(|n| n["ref"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn list_label_filter_narrows_the_set() {
        let (_d, root) = init_store();
        seed_filterable(&root);
        let cli = cli_with_root(
            &root,
            &["ticket", "list", "e", "--label", "urgent", "--json"],
        );
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        assert_eq!(refs_in(&out), vec!["e/1", "e/3"]);
    }

    #[test]
    fn list_state_filter_narrows_the_set() {
        let (_d, root) = init_store();
        seed_filterable(&root);
        let cli = cli_with_root(
            &root,
            &["ticket", "list", "e", "--status", "to-do,done", "--json"],
        );
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        assert_eq!(refs_in(&out), vec!["e/2", "e/3"]);
    }

    #[test]
    fn list_open_aggregator_and_label_combine_with_and() {
        let (_d, root) = init_store();
        seed_filterable(&root);
        // open = to-do|in-progress|blocked, AND label urgent => only e/1.
        let cli = cli_with_root(
            &root,
            &[
                "ticket", "list", "e", "--open", "--label", "urgent", "--json",
            ],
        );
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        assert_eq!(refs_in(&out), vec!["e/1"]);
    }

    #[test]
    fn list_builtin_regex_match_over_name() {
        let (_d, root) = init_store();
        seed_filterable(&root);
        let cli = cli_with_root(
            &root,
            &["ticket", "list", "e", "--match", "needle", "--json"],
        );
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        // Only the two nodes whose name carries "needle".
        assert_eq!(refs_in(&out), vec!["e/1", "e/3"]);
    }

    #[test]
    fn list_match_runs_over_structured_survivors() {
        let (_d, root) = init_store();
        seed_filterable(&root);
        // Structured (state=done) first leaves only e/3; match "needle" keeps it.
        let cli = cli_with_root(
            &root,
            &[
                "ticket", "list", "e", "--status", "done", "--match", "needle", "--json",
            ],
        );
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok());
        assert_eq!(refs_in(&out), vec!["e/3"]);
    }

    #[test]
    fn list_unknown_match_impl_lists_available() {
        let (_d, root) = init_store();
        seed_filterable(&root);
        let cli = cli_with_root(
            &root,
            &[
                "ticket",
                "list",
                "e",
                "--match",
                "q",
                "--match-impl",
                "ghost",
            ],
        );
        let (_o, _e, r) = invoke(&cli, b"");
        let msg = r.expect_err("unknown match-impl must error").to_string();
        assert!(msg.contains("ghost"), "got: {msg}");
        assert!(
            msg.contains("regex"),
            "available list should name the built-in: {msg}"
        );
    }

    #[test]
    fn list_external_matcher_via_project_config() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, root) = init_store();
        seed_filterable(&root);
        // A fake external matcher: echo back the candidate whose file is 1.md,
        // regardless of the order candidates arrive in (deterministic).
        let script = root.join("fake-matcher.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nshift\nfor c in \"$@\"; do\n  case \"$c\" in\n    */1.md) printf '%s\\n' \"$c\" ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        // Configure it as a project matcher named `first` in .loti.conf.
        std::fs::write(
            root.join(".loti.conf"),
            format!(
                "loti-root = \".\"\n[match-impl.first]\ncommand = [\"{}\", \"<QUERY>\", \"<CANDIDATES>\"]\n",
                script.to_string_lossy()
            ),
        )
        .unwrap();
        let cli = cli_with_root(
            &root,
            &[
                "ticket",
                "list",
                "e",
                "--match",
                "anything",
                "--match-impl",
                "first",
                "--json",
            ],
        );
        let (out, _e, r) = invoke(&cli, b"");
        assert!(r.is_ok(), "external matcher list should succeed");
        // The fake matcher returned the first candidate (e/1).
        assert_eq!(refs_in(&out), vec!["e/1"]);
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
