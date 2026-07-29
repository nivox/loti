//! The single declarative command-tree.
//!
//! Invariant: argument parsing, scoped `--help`, and the global full-tree help
//! all render from this one `clap` derive definition — there is never an
//! independently hand-maintained CLI surface to drift out of sync. The grammar
//! is noun → verb → collection.
//!
//! Two annotations are carried in the help text of every relevant node so an
//! agent can drive the CLI unaided:
//!   * **input rule** — inline flag vs stdin/`--file` — on each payload arg;
//!   * **actor requirement** — `-u`/`-a` — on comment operations.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// `loti` — a local, markdown-backed ticket tracker driven entirely by this CLI.
///
/// Grammar is noun-verb: `loti <epic|ticket> <verb> ...`, with collections
/// (`label`/`comment`/`asset`) nesting a third level. Free-form/binary payloads
/// (body, comment text, asset data) are read from stdin or `--file` — never
/// inline. Run `loti skill` for concepts and workflow.
#[derive(Debug, Parser)]
#[command(
    name = "loti",
    version,
    propagate_version = true,
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Use PATH as the data root, overriding upward `.loti/` discovery. For
    /// `init`, PATH is where the new store's files are created (a `.loti.conf`
    /// pointer is left in the current directory). Flag only — there is no
    /// environment-variable override.
    #[arg(long, global = true, value_name = "PATH")]
    pub root: Option<PathBuf>,

    /// Print the complete command tree in one pass — every noun, verb, and
    /// collection, with each flag's input rule and the actor requirement on
    /// comment operations. Handled before normal dispatch; exits after printing.
    #[arg(long, global = true)]
    pub help_full: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialise a store: `.loti/` here by default, or a data root elsewhere
    /// (via --root or the positional <DIR>) plus a `.loti.conf` pointer here.
    /// Refuses if this scope is already inside a store. Warns if inside a git
    /// repo but not at its root.
    Init(InitArgs),

    /// Epics — the top-level units of work.
    Epic(EpicArgs),

    /// Tickets and subtickets — nodes at any depth. A subticket is
    /// `ticket create <epic-id> --parent <ref>`.
    Ticket(TicketArgs),

    /// Print the static, hand-authored SKILL.md verbatim.
    Skill,

    /// Browse epics and tickets in a full-screen terminal interface. Needs an
    /// interactive terminal; press ? inside it for the keys.
    Tui,

    /// Align an older on-disk store format to this binary.
    MigrateStore(MigrateStoreArgs),
}

#[derive(Debug, Args)]
pub struct MigrateStoreArgs {
    /// Proceed even if staging files from an interrupted operation never clear
    /// during the drain. Use only when you are sure no other operation is live.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Data directory. Omitted: create `.loti/` in the current directory.
    /// Given: create the data root there and a `.loti.conf` pointer here.
    /// Equivalent to (and mutually exclusive with) the global `--root`.
    #[arg(value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// epic
// ---------------------------------------------------------------------------

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct EpicArgs {
    #[command(subcommand)]
    pub command: EpicCommand,
}

#[derive(Debug, Subcommand)]
pub enum EpicCommand {
    /// Create an epic. Body ← stdin or --file (never inline).
    Create(EpicCreateArgs),
    /// Show an epic (the sole projectable reader).
    Show(ShowArgs),
    /// Edit plain scalar fields (name/summary/body).
    Edit(EpicEditArgs),
    /// Set epic status: only closed/open is settable (completed is computed
    /// from the tickets). Set-only; read via `show`.
    Status(EpicStatusArgs),
    /// Manage labels (add/remove/list).
    Label(LabelArgs),
    /// Manage comments (add/edit/delete/list). Actor required on mutations.
    Comment(CommentArgs),
    /// Manage assets (add/update/show/delete/list). Delete is hard.
    Asset(AssetArgs),
    /// List epics — a flat roster.
    List(EpicListArgs),
}

#[derive(Debug, Args)]
pub struct EpicCreateArgs {
    /// Epic id (human-chosen).
    #[arg(value_name = "EPIC-ID")]
    pub epic_id: String,
    /// One-line name (inline).
    #[arg(long, value_name = "S")]
    pub name: String,
    /// One-line summary (inline).
    #[arg(long, value_name = "S")]
    pub summary: String,
    /// Label; repeatable (inline).
    #[arg(long, value_name = "L")]
    pub label: Vec<String>,
    /// Body source: stdin or --file (never inline; optional → empty).
    #[command(flatten)]
    pub content: ContentInput,
}

#[derive(Debug, Args)]
pub struct EpicEditArgs {
    #[arg(value_name = "ID")]
    pub id: String,
    /// New name (inline).
    #[arg(long, value_name = "S")]
    pub name: Option<String>,
    /// New summary (inline).
    #[arg(long, value_name = "S")]
    pub summary: Option<String>,
    /// New body source: stdin or --file (never inline).
    #[command(flatten)]
    pub content: ContentInput,
}

#[derive(Debug, Args)]
pub struct EpicStatusArgs {
    #[arg(value_name = "ID")]
    pub id: String,
    #[command(flatten)]
    pub state: EpicStatusSel,
    /// Close reason (inline; only with --closed).
    #[arg(long, value_name = "S", requires = "closed")]
    pub reason: Option<String>,
}

/// Epic status selector — exactly one of these is required.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct EpicStatusSel {
    /// Mark the epic closed (stored flag; takes precedence).
    #[arg(long)]
    pub closed: bool,
    /// Reopen the epic (clear the closed flag).
    #[arg(long)]
    pub open: bool,
}

#[derive(Debug, Args)]
pub struct EpicListArgs {
    /// Restrict output to these summary fields (dotted leaf paths). Heavy
    /// fields (body/comments/assets) are show-only and rejected here.
    #[command(flatten)]
    pub fields: FieldSel,
    /// Machine-readable JSON (flat array).
    #[command(flatten)]
    pub format: ListFormat,
}

// ---------------------------------------------------------------------------
// ticket
// ---------------------------------------------------------------------------

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct TicketArgs {
    #[command(subcommand)]
    pub command: TicketCommand,
}

#[derive(Debug, Subcommand)]
pub enum TicketCommand {
    /// Create a ticket/subticket. Body ← stdin or --file (never inline).
    Create(TicketCreateArgs),
    /// Show a node (the sole projectable reader).
    Show(ShowArgs),
    /// Edit plain scalar fields (name/summary/parent/body).
    Edit(TicketEditArgs),
    /// Set node status (set-only; read via `show`).
    Status(TicketStatusArgs),
    /// Manage the blocked-by dependency list (add/remove/set/clear/list).
    BlockedBy(BlockedByArgs),
    /// Manage the single-holder claim (take/release).
    Claim(ClaimArgs),
    /// Manage labels (add/remove/list).
    Label(LabelArgs),
    /// Manage comments (add/edit/delete/list). Actor required on mutations.
    Comment(CommentArgs),
    /// Manage assets (add/update/show/delete/list). Delete is hard.
    Asset(AssetArgs),
    /// List nodes under a required scope.
    List(TicketListArgs),
}

/// The `blocked-by` dependency list is node-only: it records which tickets
/// block this one. It is an advisory annotation independent of the node's
/// status — setting or clearing it never changes the state, and a state change
/// never touches it. A blocker must exist; its own state is irrelevant.
#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct BlockedByArgs {
    #[command(subcommand)]
    pub command: BlockedByCommand,
}

#[derive(Debug, Subcommand)]
pub enum BlockedByCommand {
    /// Add blockers (deduplicated). Each blocker is `<n>` (same epic) or
    /// `<epic-id>/<n>`.
    Add(BlockedByMutateArgs),
    /// Remove blockers. Removing an absent blocker is a no-op.
    Remove(BlockedByMutateArgs),
    /// Replace the whole list with these blockers.
    Set(BlockedByMutateArgs),
    /// Clear the whole list.
    Clear(RefArg),
    /// List the current blockers (canonical refs).
    List(RefArg),
}

#[derive(Debug, Args)]
pub struct BlockedByMutateArgs {
    /// Node reference `<epic-id>/<n>`.
    #[arg(value_name = "REF")]
    pub reference: String,
    /// Blockers: each `<n>` (same epic as REF) or `<epic-id>/<n>` (inline).
    #[arg(value_name = "BLOCKER", required = true)]
    pub blockers: Vec<String>,
}

/// A node's claim is a single freeform holder identifier plus a
/// `loti`-maintained timestamp. It is node-only and actor-agnostic — the
/// identifier is not the `-u`/`-a` attribution actor — and independent of
/// status; a node has at most one holder, so re-taking reassigns. Read it back
/// via `ticket show`.
#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct ClaimArgs {
    #[command(subcommand)]
    pub command: ClaimCommand,
}

#[derive(Debug, Subcommand)]
pub enum ClaimCommand {
    /// Take or reassign the claim; overwrites any current holder and refreshes
    /// the timestamp.
    Take(ClaimTakeArgs),
    /// Release the claim, dropping the holder and timestamp together.
    Release(RefArg),
}

#[derive(Debug, Args)]
pub struct ClaimTakeArgs {
    /// Node reference `<epic-id>/<n>`.
    #[arg(value_name = "REF")]
    pub reference: String,
    /// Claimer identifier — a freeform email or name (inline; never empty).
    #[arg(long = "as", value_name = "IDENTIFIER")]
    pub claimer: String,
}

#[derive(Debug, Args)]
pub struct TicketCreateArgs {
    /// Owning epic id.
    #[arg(value_name = "EPIC-ID")]
    pub epic_id: String,
    /// Parent node reference `<epic-id>/<n>` — omit for a top-level ticket.
    #[arg(long, value_name = "REF")]
    pub parent: Option<String>,
    /// One-line name (inline).
    #[arg(long, value_name = "S")]
    pub name: String,
    /// One-line summary (inline).
    #[arg(long, value_name = "S")]
    pub summary: String,
    /// Label; repeatable (inline).
    #[arg(long, value_name = "L")]
    pub label: Vec<String>,
    /// Body source: stdin or --file (never inline; optional → empty).
    #[command(flatten)]
    pub content: ContentInput,
}

#[derive(Debug, Args)]
pub struct TicketEditArgs {
    /// Node reference `<epic-id>/<n>`.
    #[arg(value_name = "REF")]
    pub reference: String,
    /// New name (inline).
    #[arg(long, value_name = "S")]
    pub name: Option<String>,
    /// New summary (inline).
    #[arg(long, value_name = "S")]
    pub summary: Option<String>,
    /// Reparent under REF (a one-field edit; identity is unchanged).
    #[arg(long, value_name = "REF")]
    pub parent: Option<String>,
    /// New body source: stdin or --file (never inline).
    #[command(flatten)]
    pub content: ContentInput,
}

#[derive(Debug, Args)]
pub struct TicketStatusArgs {
    /// Node reference `<epic-id>/<n>`.
    #[arg(value_name = "REF")]
    pub reference: String,
    #[command(flatten)]
    pub state: TicketStateSel,
    /// Free-form reason (inline). Required with --blocked and with --closed.
    #[arg(long, value_name = "S")]
    pub reason: Option<String>,
    /// Cascade a close to non-terminal descendants (only with --closed).
    #[arg(long, requires = "closed")]
    pub cascade: bool,
}

/// Node status selector — exactly one of these is required.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct TicketStateSel {
    #[arg(long)]
    pub to_do: bool,
    #[arg(long)]
    pub in_progress: bool,
    /// Requires `--reason`. The blocked-by dependency list is managed
    /// separately via `ticket blocked-by` and is not set here.
    #[arg(long)]
    pub blocked: bool,
    /// Allowed only when all descendants are terminal.
    #[arg(long)]
    pub done: bool,
    /// Requires `--reason`; closes only this node unless `--cascade` is given.
    #[arg(long)]
    pub closed: bool,
}

#[derive(Debug, Args)]
pub struct TicketListArgs {
    /// Required scope: `<epic-id>` (whole epic) or `<epic-id>/<n>` (under a
    /// node). There is no bare cross-epic list.
    #[arg(value_name = "EPIC-ID[/N]")]
    pub scope: String,
    /// List only the immediate level under the scope — an epic's top-level
    /// nodes, or a node's direct children — instead of the full tree.
    #[arg(long)]
    pub shallow: bool,
    #[command(flatten)]
    pub filters: ListFilterArgs,
    /// Restrict output to these summary fields (dotted leaf paths). Heavy
    /// fields (body/comments/assets/subtickets) are show-only and rejected here.
    #[command(flatten)]
    pub fields: FieldSel,
    #[command(flatten)]
    pub format: ListFormat,
}

/// The label/status/match filter families for `ticket list`. Families combine
/// with AND. Structured families (label, status) are evaluated first; `--match`
/// runs over the survivors.
#[derive(Debug, Args)]
pub struct ListFilterArgs {
    /// Keep nodes carrying this label. Repeat for AND; comma within one flag is
    /// an OR-group (`--label a --label b,c` keeps `a AND (b OR c)`).
    #[arg(long, value_name = "L[,L]")]
    pub label: Vec<String>,
    /// Drop nodes carrying any of these labels ("has none of"); comma and repeat
    /// both union.
    #[arg(long, value_name = "L[,L]")]
    pub not_label: Vec<String>,
    /// Keep nodes in one of these statuses; comma is OR. Give once — statuses
    /// are mutually exclusive, so several go in one comma-separated flag.
    #[arg(
        long,
        value_name = "STATUS[,STATUS]",
        conflicts_with_all = ["open", "resolved"]
    )]
    pub status: Vec<String>,
    /// Drop nodes in any of these statuses; comma and repeat union.
    #[arg(long, value_name = "STATUS[,STATUS]")]
    pub not_status: Vec<String>,
    /// Shorthand for the non-terminal statuses (to-do, in-progress, blocked).
    #[arg(long, conflicts_with_all = ["resolved", "status"])]
    pub open: bool,
    /// Shorthand for the terminal statuses (done, closed).
    #[arg(long, conflicts_with_all = ["open", "status"])]
    pub resolved: bool,
    /// Keep nodes the matcher selects. The built-in `regex` matcher (default)
    /// tests name, summary and body; an external matcher is chosen with
    /// `--match-impl`.
    #[arg(long = "match", value_name = "QUERY")]
    pub match_query: Option<String>,
    /// The match implementation to use. Defaults to the built-in `regex`;
    /// other names must be configured. Only meaningful with `--match`.
    #[arg(long, value_name = "IMPL", requires = "match_query")]
    pub match_impl: Option<String>,
}

// ---------------------------------------------------------------------------
// shared collections: label / comment / asset  (identical under epic & ticket)
// ---------------------------------------------------------------------------

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct LabelArgs {
    #[command(subcommand)]
    pub command: LabelCommand,
}

#[derive(Debug, Subcommand)]
pub enum LabelCommand {
    /// Add labels (inline positionals).
    Add(LabelMutateArgs),
    /// Remove labels (inline positionals).
    Remove(LabelMutateArgs),
    /// List labels.
    List(RefArg),
}

#[derive(Debug, Args)]
pub struct LabelMutateArgs {
    /// Target reference (`<id>` or `<epic-id>/<n>`).
    #[arg(value_name = "REF")]
    pub reference: String,
    /// Labels (inline).
    #[arg(value_name = "LABEL", required = true)]
    pub labels: Vec<String>,
}

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct CommentArgs {
    #[command(subcommand)]
    pub command: CommentCommand,
}

#[derive(Debug, Subcommand)]
pub enum CommentCommand {
    /// Add a comment. ACTOR REQUIRED (-u/--user xor -a/--agent). Text ← stdin
    /// or --file (required; never inline).
    Add(CommentAddArgs),
    /// Edit a comment — own author only. ACTOR REQUIRED. Text ← stdin/--file.
    Edit(CommentEditArgs),
    /// Soft-delete a comment — own author only. ACTOR REQUIRED.
    Delete(CommentDeleteArgs),
    /// List comments. Hidden deleted comments shown as tombstones with
    /// --include-deleted.
    List(CommentListArgs),
}

/// Actor identity — `-u/--user` xor `-a/--agent <NAME>`, required on comment
/// mutations (comments are the sole attribution channel).
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct ActorArg {
    /// Attribute to the human actor.
    #[arg(short = 'u', long = "user")]
    pub user: bool,
    /// Attribute to a named agent.
    #[arg(short = 'a', long = "agent", value_name = "NAME")]
    pub agent: Option<String>,
}

#[derive(Debug, Args)]
pub struct CommentAddArgs {
    #[arg(value_name = "REF")]
    pub reference: String,
    #[command(flatten)]
    pub actor: ActorArg,
    /// Comment text: stdin or --file (required; never inline).
    #[command(flatten)]
    pub content: ContentInput,
}

#[derive(Debug, Args)]
pub struct CommentEditArgs {
    #[arg(value_name = "REF")]
    pub reference: String,
    #[arg(value_name = "COMMENT-ID")]
    pub comment_id: u64,
    #[command(flatten)]
    pub actor: ActorArg,
    /// Replacement text: stdin or --file (required; never inline).
    #[command(flatten)]
    pub content: ContentInput,
}

#[derive(Debug, Args)]
pub struct CommentDeleteArgs {
    #[arg(value_name = "REF")]
    pub reference: String,
    #[arg(value_name = "COMMENT-ID")]
    pub comment_id: u64,
    #[command(flatten)]
    pub actor: ActorArg,
}

#[derive(Debug, Args)]
pub struct CommentListArgs {
    #[arg(value_name = "REF")]
    pub reference: String,
    /// Include soft-deleted comments as author+timestamp tombstones.
    #[arg(long)]
    pub include_deleted: bool,
}

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
pub struct AssetArgs {
    #[command(subcommand)]
    pub command: AssetCommand,
}

#[derive(Debug, Subcommand)]
pub enum AssetCommand {
    /// Add a new asset (create-only; a name already present is refused — use
    /// `update` to replace one). Data ← stdin or --file (never inline). --name
    /// defaults to the --file basename.
    Add(AssetAddArgs),
    /// Update an asset in place: replace its data and/or description. Data ←
    /// stdin or --file (never inline). Use `add` to create a new asset.
    Update(AssetUpdateArgs),
    /// Read an asset's data back to stdout, verbatim.
    Show(AssetShowArgs),
    /// Delete an asset by name — HARD delete.
    Delete(AssetDeleteArgs),
    /// List assets.
    List(RefArg),
}

#[derive(Debug, Args)]
pub struct AssetAddArgs {
    #[arg(value_name = "REF")]
    pub reference: String,
    /// Asset name (inline); defaults to the --file basename when omitted.
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Optional description (inline).
    #[arg(long, value_name = "S")]
    pub description: Option<String>,
    /// Asset data: stdin or --file (never inline).
    #[command(flatten)]
    pub content: ContentInput,
}

#[derive(Debug, Args)]
pub struct AssetUpdateArgs {
    #[arg(value_name = "REF")]
    pub reference: String,
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Replace the description (inline). Pass an empty string to clear it.
    #[arg(long, value_name = "S")]
    pub description: Option<String>,
    /// Replacement data: stdin or --file (never inline). Omit to keep the
    /// current data and update only the description.
    #[command(flatten)]
    pub content: ContentInput,
}

#[derive(Debug, Args)]
pub struct AssetShowArgs {
    #[arg(value_name = "REF")]
    pub reference: String,
    #[arg(value_name = "NAME")]
    pub name: String,
}

#[derive(Debug, Args)]
pub struct AssetDeleteArgs {
    #[arg(value_name = "REF")]
    pub reference: String,
    #[arg(value_name = "NAME")]
    pub name: String,
}

// ---------------------------------------------------------------------------
// shared building blocks
// ---------------------------------------------------------------------------

/// A bare target reference — `<id>` or `<epic-id>/<n>` — for readers/listers.
#[derive(Debug, Args)]
pub struct RefArg {
    #[arg(value_name = "REF")]
    pub reference: String,
}

/// The shared content-input source. The payload is **never** inline:
/// it comes from `--file <PATH>` or, absent that, piped stdin; `--file -` names
/// stdin explicitly. An interactive TTY is treated as "no source" (never
/// blocks). See [`crate::content_input`].
#[derive(Debug, Args)]
pub struct ContentInput {
    /// Read the payload from PATH instead of stdin (never passed inline). Use
    /// PATH `-` to name stdin explicitly (same as piping with no --file).
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,
}

/// `show` projection: at most one of `--field` / `--fields`.
#[derive(Debug, Args)]
#[group(required = false, multiple = false)]
pub struct FieldSel {
    /// Single dotted leaf path (e.g. `comments.author`).
    #[arg(long, value_name = "F")]
    pub field: Option<String>,
    /// Comma-separated dotted leaf paths.
    #[arg(long, value_name = "F,...")]
    pub fields: Option<String>,
}

/// `show` output mode: at most one of markdown/json/raw.
#[derive(Debug, Args)]
#[group(required = false, multiple = false)]
pub struct ShowFormat {
    /// Viewer-friendly markdown (default).
    #[arg(long)]
    pub markdown: bool,
    /// Canonical JSON (the source of truth).
    #[arg(long)]
    pub json: bool,
    /// Strict-unambiguous leaf values, one per line.
    #[arg(long)]
    pub raw: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Target reference (`<id>` or `<epic-id>/<n>`).
    #[arg(value_name = "REF")]
    pub reference: String,
    #[command(flatten)]
    pub fields: FieldSel,
    #[command(flatten)]
    pub format: ShowFormat,
}

/// `list` output mode: at most one of json/ndjson/raw; default is plain text
/// (`list` never presents, so there is no `--markdown`).
#[derive(Debug, Args)]
#[group(required = false, multiple = false)]
pub struct ListFormat {
    /// Flat JSON array with `parent` pointers.
    #[arg(long)]
    pub json: bool,
    /// Stream one JSON object per line.
    #[arg(long)]
    pub ndjson: bool,
    /// Flat, tab-separated rows.
    #[arg(long)]
    pub raw: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_tree_is_valid() {
        // Panics if the single declarative tree is internally inconsistent.
        Cli::command().debug_assert();
    }

    #[test]
    fn no_auto_help_subcommand() {
        // The top-level nouns are exactly init|epic|ticket|skill|migrate-store;
        // clap's auto `help` subcommand must not appear as extra surface.
        // `--help` flags remain, only the extra subcommand is gone.
        assert!(Cli::try_parse_from(["loti", "help"]).is_err());
        assert!(Cli::try_parse_from(["loti", "epic", "help"]).is_err());
        assert!(Cli::try_parse_from(["loti", "ticket", "comment", "help"]).is_err());
    }

    #[test]
    fn parses_root_override() {
        let cli = Cli::try_parse_from(["loti", "--root", "/tmp/store", "skill"]).unwrap();
        assert_eq!(
            cli.root.as_deref(),
            Some(std::path::Path::new("/tmp/store"))
        );
    }

    #[test]
    fn comment_add_requires_actor() {
        // Missing -u/-a is a parse error: comment mutations require an actor.
        let err = Cli::try_parse_from(["loti", "ticket", "comment", "add", "e/1"]);
        assert!(err.is_err());
        // With an actor it parses.
        assert!(Cli::try_parse_from(["loti", "ticket", "comment", "add", "e/1", "-u"]).is_ok());
    }

    #[test]
    fn actor_is_exclusive() {
        let err = Cli::try_parse_from(["loti", "epic", "comment", "add", "e", "-u", "-a", "bot"]);
        assert!(err.is_err());
    }

    #[test]
    fn ticket_status_requires_one_state() {
        assert!(Cli::try_parse_from(["loti", "ticket", "status", "e/1"]).is_err());
        assert!(Cli::try_parse_from(["loti", "ticket", "status", "e/1", "--done"]).is_ok());
        // Two states are mutually exclusive.
        assert!(
            Cli::try_parse_from(["loti", "ticket", "status", "e/1", "--done", "--to-do"]).is_err()
        );
    }

    #[test]
    fn ticket_list_scope_is_required() {
        assert!(Cli::try_parse_from(["loti", "ticket", "list"]).is_err());
        assert!(Cli::try_parse_from(["loti", "ticket", "list", "my-epic"]).is_ok());
    }

    #[test]
    fn body_has_no_inline_flag() {
        // `--body` does not exist: body content comes from stdin/--file only,
        // never an inline flag.
        assert!(Cli::try_parse_from([
            "loti",
            "epic",
            "create",
            "e",
            "--name",
            "n",
            "--summary",
            "s",
            "--body",
            "x",
        ])
        .is_err());
    }

    #[test]
    fn list_accepts_the_filter_families() {
        // Repeated --label (AND) with a comma OR-group, exclusions, a state set
        // and a match all parse together.
        assert!(Cli::try_parse_from([
            "loti",
            "ticket",
            "list",
            "e",
            "--label",
            "a",
            "--label",
            "b,c",
            "--not-label",
            "x",
            "--status",
            "to-do,blocked",
            "--match",
            "needle",
        ])
        .is_ok());
    }

    #[test]
    fn list_state_aggregators_conflict_at_parse_time() {
        // --open and --resolved are mutually exclusive, and each conflicts with
        // an explicit --status, so the grammar rejects the combinations.
        assert!(
            Cli::try_parse_from(["loti", "ticket", "list", "e", "--open", "--resolved"]).is_err()
        );
        assert!(
            Cli::try_parse_from(["loti", "ticket", "list", "e", "--open", "--status", "done"])
                .is_err()
        );
        assert!(Cli::try_parse_from([
            "loti",
            "ticket",
            "list",
            "e",
            "--resolved",
            "--status",
            "to-do"
        ])
        .is_err());
    }

    #[test]
    fn match_impl_requires_a_match_query() {
        // --match-impl is meaningless without --match, so it is rejected alone.
        assert!(
            Cli::try_parse_from(["loti", "ticket", "list", "e", "--match-impl", "rg"]).is_err()
        );
        assert!(Cli::try_parse_from([
            "loti",
            "ticket",
            "list",
            "e",
            "--match",
            "q",
            "--match-impl",
            "rg"
        ])
        .is_ok());
    }
}
