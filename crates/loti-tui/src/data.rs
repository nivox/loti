//! The browser's store seam: everything it learns from or writes to a store
//! passes through this module.
//!
//! Two core seams serve different concerns. This module is the only browser
//! module that calls the store surface; [`crate::theme`] is the only one that
//! calls the status-colour palette. Keeping presentation vocabulary out of the
//! store seam means each concern has one predictable place for a core change.
//!
//! [`crate::app::App`] carries a [`Store`] only as an opaque transit handle and
//! passes it straight here. Naming that handle does not reach the store: this
//! module alone calls it. The rest of the browser deals in [`Row`]s and rendered
//! markdown, so which core call backs a screen — and whether an operation reads
//! or writes — never leaks into the navigation model or drawing code.

use anyhow::{Context, Result};
use jiff::Timestamp;
use loti_core::domain::{EpicStatus, NodeRef};
use loti_core::launch;
use loti_core::lock::VersionRefusal;
use loti_core::ops::{self, CommentView, NodeStatusChange, Target};
use loti_core::read;
use loti_core::render;
use loti_core::resource::{self, Origin, ResourceId, Roots};
use loti_core::store::Store;
use loti_core::{Actor, NodeState};

/// What a collection of meta hangs off. Labels, comments and assets are
/// identical on an epic and on a node; a dependency list exists only on a node,
/// because an epic is not a unit of work that can be blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Container {
    /// An epic, addressed by its id.
    Epic(String),
    /// A node, addressed by `<epic-id>/<number>`.
    Node(NodeRef),
}

impl Container {
    /// The container as a selection, which is how a row addresses it and how a
    /// preview asks for its document.
    pub fn selection(&self) -> Selection {
        match self {
            Container::Epic(id) => Selection::Epic(id.clone()),
            Container::Node(r) => Selection::Node(r.clone()),
        }
    }

    /// The collections this container carries, in the order they are listed.
    /// Fixed rather than derived from what is populated: a collection is a row
    /// whether or not it has members, so the rows of a level do not move around
    /// as meta is added and removed.
    pub fn collections(&self) -> &'static [Collection] {
        match self {
            Container::Epic(_) => &[Collection::Labels, Collection::Comments, Collection::Assets],
            Container::Node(_) => &[
                Collection::Labels,
                Collection::Comments,
                Collection::BlockedBy,
                Collection::Assets,
            ],
        }
    }

    /// The noun the command line addresses this container by, for a message that
    /// tells a reader which command does a job the browser does not.
    ///
    /// Invariant: a container's assets and a node's assets are different commands,
    /// so a message naming one has to name the container's own — and every node is
    /// a `ticket` there, subtickets included, because the command line has no
    /// second noun for a node with a parent.
    pub fn cli_noun(&self) -> &'static str {
        match self {
            Container::Epic(_) => "epic",
            Container::Node(_) => "ticket",
        }
    }

    /// The core-side target, so every collection call names the container once.
    fn target(&self) -> Target {
        match self {
            Container::Epic(id) => Target::Epic(id.clone()),
            Container::Node(r) => Target::Node(r.clone()),
        }
    }
}

/// One of a container's meta collections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collection {
    /// The label set.
    Labels,
    /// The comment thread, tombstones included.
    Comments,
    /// The dependency list; nodes only.
    BlockedBy,
    /// The attached assets.
    Assets,
}

impl Collection {
    /// The collection's name as the rest of loti spells it, so a row, a
    /// breadcrumb and the document in the preview pane one pane away never give
    /// the same collection two names.
    pub fn name(self) -> &'static str {
        match self {
            Collection::Labels => "labels",
            Collection::Comments => "comments",
            Collection::BlockedBy => "blocked-by",
            Collection::Assets => "assets",
        }
    }
}

/// What a navigation row points at. This is the target every operation on the
/// highlighted row needs, so it is carried as one value rather than re-derived
/// per operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// An epic, addressed by its id.
    Epic(String),
    /// A node, addressed by `<epic-id>/<number>`.
    Node(NodeRef),
    /// One of a container's collections, as a row on the container's level.
    Collection(Container, Collection),
    /// One label of a container.
    Label(Container, String),
    /// One comment of a container, by the id that is never reused.
    Comment(Container, u64),
    /// One asset of a container, by the name that is unique within it.
    Asset(Container, String),
    /// One entry of a node's dependency list: the node that is blocked, and the
    /// node blocking it. Both travel, because the entry belongs to the first and
    /// the document belongs to the second.
    Blocker(Container, NodeRef),
    /// A dependency entry the store holds as text but which cannot address a
    /// node. It remains visible, but cannot become a removal target because
    /// removal requires a parsed reference.
    UnremovableBlocker(Container, String),
}

/// One valid resource an agent picker can select. Its id is the value a launch
/// request carries; its origin is only the provenance shown to the reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentChoice {
    /// The effective resource id.
    pub id: ResourceId,
    /// Whether that effective definition came from this project or the user's
    /// global configuration.
    pub origin: Origin,
}

impl AgentChoice {
    /// The one-line value a picker shows. The origin remains beside the id so two
    /// otherwise identical-looking catalogs explain which definition won.
    pub fn shown(&self) -> String {
        let origin = match self.origin {
            Origin::Local => "local",
            Origin::Global => "global",
        };
        format!("{} ({origin})", self.id)
    }
}

/// The valid effective resources and frozen target gathered when an agent picker
/// is requested. Invalid effective entries are deliberately absent: a picker can
/// only produce a request that later launch preparation can resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPicker {
    /// The epic or ticket the request remains scoped to.
    pub target: launch::Target,
    /// Workflows, in the core roster's stable effective-id order.
    pub workflows: Vec<AgentChoice>,
    /// Agent profiles, in the core roster's stable effective-id order.
    pub profiles: Vec<AgentChoice>,
}

impl AgentPicker {
    /// Whether both values a launch request requires are selectable.
    pub fn is_selectable(&self) -> bool {
        !self.workflows.is_empty() && !self.profiles.is_empty()
    }

    /// Why an empty picker cannot open, in terms of the missing selectable
    /// resources rather than invalid candidates the picker never offered.
    pub fn unavailable_reason(&self) -> String {
        match (self.workflows.is_empty(), self.profiles.is_empty()) {
            (true, true) => "no valid workflows or agent profiles are available".to_string(),
            (true, false) => "no valid workflows are available".to_string(),
            (false, true) => "no valid agent profiles are available".to_string(),
            (false, false) => String::new(),
        }
    }
}

impl Selection {
    /// How the selection is named to a reader: a reference as it is typed for an
    /// epic or a node, and otherwise the container plus the member's own id or
    /// name, which is how the CLI addresses a member too.
    ///
    /// A collection, label and unremovable blocker name their container, because
    /// the container's document is what the preview shows for them; a blocker
    /// names the node it points at, for the same reason.
    pub fn reference(&self) -> String {
        match self {
            Selection::Epic(id) => id.clone(),
            Selection::Node(r) | Selection::Blocker(_, r) => r.to_string(),
            Selection::Collection(c, _) | Selection::Label(c, _) => c.selection().reference(),
            Selection::Comment(c, id) => format!("{} comment {id}", c.selection().reference()),
            Selection::Asset(c, name) => format!("{} asset {name}", c.selection().reference()),
            Selection::UnremovableBlocker(c, entry) => {
                format!("{} blocker {entry}", c.selection().reference())
            }
        }
    }

    /// The level entering this selection opens, or `None` when there is nothing
    /// below it: a collection member is a leaf, a blocker included — it is read
    /// where it stands, not followed.
    pub fn level(&self) -> Option<Level> {
        match self {
            Selection::Epic(id) => Some(Level::Epic(id.clone())),
            Selection::Node(r) => Some(Level::Node(r.clone())),
            Selection::Collection(c, kind) => Some(Level::Collection(c.clone(), *kind)),
            Selection::Label(..)
            | Selection::Comment(..)
            | Selection::Asset(..)
            | Selection::Blocker(..)
            | Selection::UnremovableBlocker(..) => None,
        }
    }

    /// The document identity this selection previews: the one value that decides
    /// whether two rows show the same content.
    ///
    /// A collection, label or unremovable blocker row carries no document of its
    /// own — the preview shows its container's — so each normalises to the
    /// container's own selection here; every other selection already names the
    /// document it shows, so it is returned unchanged. [`preview`] and the
    /// reader's scroll position both decide "same document" through this one
    /// function, so they cannot come to disagree about it.
    pub fn document(&self) -> Selection {
        match self {
            Selection::Collection(c, _)
            | Selection::Label(c, _)
            | Selection::UnremovableBlocker(c, _) => c.selection(),
            other => other.clone(),
        }
    }
}

/// What a row stands for, which is what decides how it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// An epic or a node, carrying its state's wire name and who holds its
    /// claim. Only a work row has a state, so a filled glyph column is itself the
    /// signal that a row is work — and only a work row can be claimed, which is
    /// why the holder is here rather than on every row.
    Work {
        /// The state's wire name.
        status: String,
        /// Who holds the single-holder claim on what the row points at; absent
        /// exactly when it is unclaimed. Only the holder travels, never when the
        /// claim was taken: that belongs to the document in the preview.
        ///
        /// Invariant: a claim is node-only, so an epic's row never carries one.
        claimed_by: Option<String>,
    },
    /// A collection of the level's container. It is structure rather than work,
    /// and has no state to invent a glyph for.
    Collection(Collection),
    /// A member of a collection, which has no state either.
    Member,
    /// A live comment of a collection.
    ///
    /// A comment is the one member whose author decides what may be done to it:
    /// only its author may rewrite or withdraw it, and the browser writes as the
    /// human and only the human. So the row carries the answer to that one
    /// question rather than a name for somewhere else to compare again — the
    /// author is on the row for the reader too, in the row's own words.
    Comment {
        /// Whether the human wrote it, which is exactly when the browser may
        /// rewrite or withdraw it.
        by_the_human: bool,
    },
    /// A withdrawn comment. Its text is withheld rather than destroyed — the
    /// store retains it, and its number is never reused — so the row says so in a
    /// word as well as in colour, because colour alone carries nothing in this
    /// crate. It offers nothing whoever wrote it: there is no text to rewrite,
    /// and withdrawing twice means nothing.
    Withdrawn,
    /// A member the store cannot fully use: an asset whose bytes are gone, a
    /// blocker naming a ticket that is not there, or blocker text that is not a
    /// reference.
    ///
    /// The row stands where the member does, because a store the reader cannot
    /// fully read is exactly when a browser is most useful: the corruption is
    /// reported on the row rather than taking the whole level down with it. Its
    /// name carries the reason in a word as well as in colour, for the same reason
    /// a withdrawn comment does.
    Unreadable,
}

/// One row of the navigation pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What the row points at, and the key the cursor is tracked by.
    pub selection: Selection,
    /// What the row stands for.
    pub kind: RowKind,
    /// The left-hand identifier column: an epic id, a bare node number, or the
    /// token a member is addressed by. A node's epic is already named in the
    /// breadcrumb, so the number alone addresses it unambiguously within a
    /// level. A collection row leaves it empty, so that a word like `blocked-by`
    /// cannot buy identifier width from every work row on the level.
    pub label: String,
    /// The one-line name, or a collection's own name.
    pub name: String,
    /// How many direct children the row has: subtickets for a work row, members
    /// for a collection row, and never anything for a member. Printed when
    /// non-empty and blank when empty, so it says how much is below without
    /// promising that anything is.
    pub children: usize,
}

impl Row {
    /// Whether entering the row opens a level.
    ///
    /// Every epic and node is enterable, because its collections are rows there
    /// whether or not they have members — so an absent child count means "no
    /// subtickets", never "nothing below". A collection is enterable exactly
    /// when it has members, which is also exactly when it prints a count. Only
    /// collection members are true leaves.
    pub fn enterable(&self) -> bool {
        match &self.selection {
            Selection::Epic(_) | Selection::Node(_) => true,
            Selection::Collection(..) => self.children > 0,
            Selection::Label(..)
            | Selection::Comment(..)
            | Selection::Asset(..)
            | Selection::Blocker(..)
            | Selection::UnremovableBlocker(..) => false,
        }
    }
}

/// Which level of the browser a listing came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    /// The epic roster — the root level, which always exists.
    Epics,
    /// An epic's collections and top-level tickets.
    Epic(String),
    /// A node's collections and direct subtickets.
    Node(NodeRef),
    /// The members of one collection.
    Collection(Container, Collection),
}

impl Level {
    /// The level's own entity, or `None` for the roster, which has no entity.
    pub fn selection(&self) -> Option<Selection> {
        match self {
            Level::Epics => None,
            Level::Epic(id) => Some(Selection::Epic(id.clone())),
            Level::Node(r) => Some(Selection::Node(r.clone())),
            Level::Collection(c, kind) => Some(Selection::Collection(c.clone(), *kind)),
        }
    }
}

/// Why this binary may read a store but not write it.
///
/// Read-only is a state of the store, never an option of the browser's: the
/// store's own format gate decides it, so there is no flag and no mode to
/// choose. A store enters and leaves the state under a running browser — an
/// agent may migrate it at any time — so this is a question to ask again rather
/// than a verdict to record once.
///
/// One variant per reason the gate refuses a mutation for, because the remedy
/// differs by reason: an unmigrated store is the reader's to fix, a migration in
/// flight is somebody else's and clears on its own, and a format newer than the
/// binary needs a newer loti.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnly {
    /// The store's format is older than this binary's: readable, and writable
    /// only once it has been migrated.
    NeedsMigration,
    /// A migration is in flight, or died holding the marker that says so: the
    /// store is read-only for everyone but the migrator until it commits.
    MigrationInProgress,
    /// The store's format is newer than this binary understands, so nothing may
    /// be written to it by this binary at all.
    NeedsNewerLoti,
    /// The recorded format version cannot be parsed, so no rule about the store
    /// can be applied and nothing may be written.
    VersionUnreadable,
}

impl ReadOnly {
    /// Every reason a store may be read and not written, so a surface that has
    /// to cover them all — one marker each — cannot then miss one.
    pub const ALL: &'static [ReadOnly] = &[
        ReadOnly::NeedsMigration,
        ReadOnly::MigrationInProgress,
        ReadOnly::NeedsNewerLoti,
        ReadOnly::VersionUnreadable,
    ];

    /// The store's own words for this refusal, so a reader is told why in the
    /// sentence the command line tells them — the remedy included, which only
    /// the store knows.
    pub fn refusal(self) -> String {
        self.gate_refusal().to_string()
    }

    /// The gate's refusal this state stands for. One mapping, read in both
    /// directions, so a state and the words that explain it cannot come to mean
    /// different things.
    fn gate_refusal(self) -> VersionRefusal {
        match self {
            ReadOnly::NeedsMigration => VersionRefusal::NeedsMigration,
            ReadOnly::MigrationInProgress => VersionRefusal::MigrationInProgress,
            ReadOnly::NeedsNewerLoti => VersionRefusal::StoreTooNew,
            ReadOnly::VersionUnreadable => VersionRefusal::Unreadable,
        }
    }
}

/// Ask the store whether it may be written, and why not where it may not.
///
/// The store is the only thing that can answer, and it is asked before any write
/// is offered rather than after one has failed: only actions the browser
/// believes it can perform are ever offered, and a store awaiting migration
/// would otherwise offer every action on every row and refuse at the last
/// keystroke.
///
/// The answer is a snapshot and never a licence: the store's recorded version
/// can change between this question and a write, so a write still verifies the
/// gate for itself. Exhaustive over the gate's refusals, so a reason this does
/// not know is a compile error rather than a store the browser treats as
/// writable.
pub fn read_only(store: &Store) -> Option<ReadOnly> {
    match store.verify_mutable() {
        Ok(()) => None,
        Err(VersionRefusal::NeedsMigration) => Some(ReadOnly::NeedsMigration),
        Err(VersionRefusal::MigrationInProgress) => Some(ReadOnly::MigrationInProgress),
        Err(VersionRefusal::StoreTooNew) => Some(ReadOnly::NeedsNewerLoti),
        Err(VersionRefusal::Unreadable) => Some(ReadOnly::VersionUnreadable),
    }
}

/// Open the store for the current directory, honouring an explicit root the
/// same way every other surface does, and refuse a format this binary cannot
/// read before any screen is drawn.
pub fn open(root: Option<&std::path::Path>) -> Result<Store> {
    let start = std::env::current_dir()?;
    let discovered = loti_core::discovery::resolve(&start, root)?;
    let store = Store::at(discovered.root);
    store
        .verify_readable()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(store)
}

/// The two effective-resource roots used by agent selection and preparation.
///
/// Both operations must resolve the same local-over-global catalog at their own
/// boundary: a resource can change after it was displayed but before it is run.
struct AgentRoots {
    workflows: Roots,
    profiles: Roots,
}

/// Resolve the resource roots from the browser's starting directory.
fn agent_roots(start: &std::path::Path) -> Result<AgentRoots> {
    // A project config is optional. When it is present, its resource roots are
    // validated by core rather than treated as an empty local catalog.
    let local = loti_core::discovery::find_project_config(start)
        .map(|path| resource::local_roots(&path))
        .transpose()?
        .unwrap_or_default();
    Ok(AgentRoots {
        workflows: Roots {
            local: local.workflows,
            global: resource::global_workflow_root(),
        },
        profiles: Roots {
            local: local.agents,
            global: resource::global_agent_root(),
        },
    })
}

/// Discover the values an agent-picker surface needs from `start`, which is the
/// directory the browser was opened from. Configuration and resource directories
/// are read here — at the requested action — never while editing hints are drawn.
///
/// The target is read with the same operation that later launch preparation uses,
/// so the request carries the frozen reference and display name rather than a
/// string reconstructed by the UI. Only epics and tickets can be launch targets;
/// every structural or metadata selection is refused by name.
pub fn agent_picker(
    store: &Store,
    selection: &Selection,
    start: &std::path::Path,
) -> Result<AgentPicker> {
    let target = match selection {
        Selection::Epic(id) => {
            let epic = ops::read_epic(store, id)?;
            launch::Target::Epic {
                id: id.clone(),
                name: epic.frontmatter.name,
            }
        }
        Selection::Node(reference) => {
            let node = ops::read_node(store, reference)?;
            launch::Target::Ticket {
                reference: reference.clone(),
                name: node.frontmatter.name,
            }
        }
        Selection::Collection(..)
        | Selection::Label(..)
        | Selection::Comment(..)
        | Selection::Asset(..)
        | Selection::Blocker(..)
        | Selection::UnremovableBlocker(..) => {
            anyhow::bail!("{} is not an epic or ticket", selection.reference())
        }
    };

    let roots = agent_roots(start)?;
    let workflows = resource::list_workflows(&roots.workflows)?
        .into_iter()
        .filter(|effective| effective.is_valid())
        .map(|effective| AgentChoice {
            id: ResourceId::parse(&effective.id)
                .expect("a valid effective resource always has a valid resource id"),
            origin: effective.origin,
        })
        .collect();
    let profiles = resource::list_profiles(&roots.profiles)?
        .into_iter()
        .filter(|effective| effective.is_valid())
        .map(|effective| AgentChoice {
            id: ResourceId::parse(&effective.id)
                .expect("a valid effective resource always has a valid resource id"),
            origin: effective.origin,
        })
        .collect();

    Ok(AgentPicker {
        target,
        workflows,
        profiles,
    })
}

/// Resolve the currently effective selections and prepare their direct launch
/// without changing the tracker. The picker only carried ids, so resolving here
/// detects a resource that disappeared or became invalid while it was open.
///
/// The caller context is assembled at the store seam because its project root
/// belongs to the store. The state machine supplies the opaque handle and its
/// working directory without inspecting either store state or store paths.
pub fn prepare_agent_launch(
    store: &Store,
    start: &std::path::Path,
    target: &launch::Target,
    workflow: &ResourceId,
    profile: &ResourceId,
    environment: std::collections::BTreeMap<String, String>,
) -> Result<launch::LaunchPlan> {
    let caller = launch::CallerContext {
        project_root: store.root().to_path_buf(),
        current_directory: start.to_path_buf(),
        env: environment,
    };
    let roots = agent_roots(start)?;
    let profile = resource::resolve_profile(&roots.profiles, profile.as_str())?
        .ok_or_else(|| anyhow::anyhow!("agent profile '{profile}' does not exist"))?
        .value
        .ok_or_else(|| anyhow::anyhow!("agent profile '{profile}' is invalid"))?;
    let resolved_workflow = resource::resolve_workflow(&roots.workflows, workflow.as_str())?
        .ok_or_else(|| anyhow::anyhow!("workflow '{workflow}' does not exist"))?;
    if resolved_workflow.value.is_none() {
        anyhow::bail!("workflow '{workflow}' is invalid");
    }

    // The workflow body is intentionally opaque; successful resolution says its
    // selected id remains usable, which is all shared preparation needs.
    launch::prepare(target, &profile, workflow, &caller).map_err(Into::into)
}

/// The rows of one level, in the order every other loti surface lists them:
/// epics by id, siblings by ascending number, and collection members in stored
/// order. Ordering is not re-decided here — a level browsed in the UI and the
/// same level printed by `list` must agree.
///
/// An epic's and a node's level lists its collections first and its work rows
/// after, which is why the roster has no collection rows: it is the one level
/// whose container is nothing.
pub fn rows(store: &Store, level: &Level) -> Result<Vec<Row>> {
    match level {
        Level::Epics => {
            let mut out = Vec::new();
            for entry in read::list_epics(store)? {
                match entry {
                    read::RosterEntry::Readable(epic) => {
                        // An epic's children are its top-level tickets, not every node
                        // in the epic: the count must describe what descending reveals.
                        let children = read::epic_children(store, &epic.id)?.len();
                        out.push(Row {
                            selection: Selection::Epic(epic.id.clone()),
                            kind: RowKind::Work {
                                status: epic.status,
                                // An epic carries no claim at all — a claim is taken on a
                                // unit of work — so the roster never marks a row.
                                claimed_by: None,
                            },
                            label: epic.id,
                            name: epic.name,
                            children,
                        });
                    }
                    // The core roster has already distinguished this one epic's
                    // failure from failure to list the roster itself. Keep its id
                    // as the selection so the reader can see where corruption is.
                    read::RosterEntry::Unreadable { id, failure } => {
                        let selection = Selection::Epic(id.clone());
                        out.push(unreadable(selection, id, &failure));
                    }
                }
            }
            Ok(out)
        }
        Level::Epic(id) => {
            let container = Container::Epic(id.clone());
            let mut out = collection_rows(store, &container)?;
            out.extend(child_rows(read::epic_children(store, id)?)?);
            Ok(out)
        }
        Level::Node(r) => {
            let container = Container::Node(r.clone());
            let mut out = collection_rows(store, &container)?;
            out.extend(child_rows(read::node_children(store, r)?)?);
            Ok(out)
        }
        Level::Collection(container, kind) => member_rows(store, container, *kind),
    }
}

/// Turn a core children listing into rows using the direct child count already
/// carried by each listing row. The function takes no store, so it cannot grow a
/// read per child while deciding what is below a row.
fn child_rows(children: Vec<render::ChildRow>) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    for child in children {
        let node_ref = NodeRef::parse(&child.reference)?;
        out.push(Row {
            label: node_ref.number.to_string(),
            selection: Selection::Node(node_ref),
            kind: RowKind::Work {
                status: child.status,
                claimed_by: child.claimed_by,
            },
            name: child.name,
            children: child.children,
        });
    }
    Ok(out)
}

/// Counts for every collection a container may offer. A normal level needs all
/// of them, so they come from one parsed container rather than one read apiece.
struct CollectionCounts {
    labels: usize,
    comments: usize,
    blocked_by: usize,
    assets: usize,
}

impl CollectionCounts {
    /// The count for this collection. Tombstones count as comments because their
    /// rows remain visible in the collection level.
    fn for_collection(&self, kind: Collection) -> usize {
        match kind {
            Collection::Labels => self.labels,
            Collection::Comments => self.comments,
            Collection::BlockedBy => self.blocked_by,
            Collection::Assets => self.assets,
        }
    }
}

/// A container's collection rows: one per collection it carries, present
/// whether or not it has members, so there is always a row to stand on.
fn collection_rows(store: &Store, container: &Container) -> Result<Vec<Row>> {
    let counts = match container {
        Container::Epic(id) => {
            let epic = ops::read_epic(store, id)?;
            CollectionCounts {
                labels: epic.frontmatter.labels.len(),
                comments: epic.frontmatter.comments.len(),
                blocked_by: 0,
                assets: epic.frontmatter.assets.len(),
            }
        }
        Container::Node(reference) => {
            let node = ops::read_node(store, reference)?;
            CollectionCounts {
                labels: node.frontmatter.labels.len(),
                comments: node.frontmatter.comments.len(),
                blocked_by: node.frontmatter.blocked_by.len(),
                assets: node.frontmatter.assets.len(),
            }
        }
    };

    Ok(container
        .collections()
        .iter()
        .map(|kind| Row {
            selection: Selection::Collection(container.clone(), *kind),
            kind: RowKind::Collection(*kind),
            label: String::new(),
            name: kind.name().to_string(),
            children: counts.for_collection(*kind),
        })
        .collect())
}

/// The members of one collection, in stored order.
///
/// Members are leaves: every row here carries no children, so entering one does
/// nothing whatever the collection.
fn member_rows(store: &Store, container: &Container, kind: Collection) -> Result<Vec<Row>> {
    let target = container.target();
    let mut out = Vec::new();
    match kind {
        Collection::Labels => {
            for label in ops::list_labels(store, &target)? {
                out.push(Row {
                    selection: Selection::Label(container.clone(), label.clone()),
                    kind: RowKind::Member,
                    label,
                    name: String::new(),
                    children: 0,
                });
            }
        }
        Collection::Comments => {
            // One clock for the whole level, so two rows of the same age never
            // disagree about how old they are.
            let now = Timestamp::now();
            for view in ops::list_comments(store, &target, true)? {
                let (id, author, created, withdrawn) = match view {
                    CommentView::Live(c) => (c.id, c.author, c.created, false),
                    CommentView::Tombstone {
                        id,
                        author,
                        created,
                    } => (id, author, created, true),
                };
                // The marker leads, because the name column is what a narrow
                // pane truncates: a withdrawn comment has to still read as
                // withdrawn with colour disabled and the pane at its narrowest.
                let name = match withdrawn {
                    true => format!("deleted · {author} · {}", age(created, now)),
                    false => format!("{author} · {}", age(created, now)),
                };
                out.push(Row {
                    selection: Selection::Comment(container.clone(), id),
                    // Who wrote it travels only on a live comment, because that is
                    // the one case it decides anything: a tombstone offers nothing
                    // whoever wrote it.
                    kind: match withdrawn {
                        true => RowKind::Withdrawn,
                        false => RowKind::Comment {
                            by_the_human: author == Actor::Human,
                        },
                    },
                    label: id.to_string(),
                    name,
                    children: 0,
                });
            }
        }
        Collection::BlockedBy => {
            // An epic is never offered a dependency list, so it never reaches
            // this level; the arm exists because the match has to be total.
            let Container::Node(node) = container else {
                return Ok(out);
            };
            for reference in ops::list_blocked_by(store, node)? {
                let blocker = match NodeRef::parse(&reference) {
                    Ok(blocker) => blocker,
                    // No parsed reference means there is no safe operation target.
                    // Keep the text visible rather than losing the whole level, but
                    // make the row incapable of reaching a raw removal.
                    Err(_) => {
                        let selection =
                            Selection::UnremovableBlocker(container.clone(), reference.clone());
                        out.push(unremovable_blocker(selection, reference));
                        continue;
                    }
                };
                // A blocker may live in another epic, so the row carries the whole
                // reference rather than the bare number a sibling would.
                let selection = Selection::Blocker(container.clone(), blocker.clone());
                out.push(match ops::read_node(store, &blocker) {
                    Ok(blocking) => Row {
                        label: reference,
                        selection,
                        // A blocker reads as a work row, glyph and all, because
                        // what it points at is work. The status, the name and the
                        // holder are three fields of this one read: a dependency
                        // list is a short curated list the browser reads entry by
                        // entry to draw at all, so the holder costs nothing beyond
                        // the read the row already pays for.
                        kind: RowKind::Work {
                            status: blocking.frontmatter.status.wire_name().to_string(),
                            claimed_by: blocking.frontmatter.claim.map(|claim| claim.by),
                        },
                        name: blocking.frontmatter.name,
                        children: 0,
                    },
                    // An entry naming a ticket the store has not got is a
                    // dependency the reader has to see to remove — so the row
                    // stays, and says so.
                    Err(e) => unreadable(selection, reference, &e),
                });
            }
        }
        Collection::Assets => {
            for asset in ops::list_assets(store, &target)? {
                let selection = Selection::Asset(container.clone(), asset.name.clone());
                out.push(match asset_size(store, container, &asset.name) {
                    Ok(size) => Row {
                        selection,
                        kind: RowKind::Member,
                        label: asset.name,
                        // A description can be long, so it is left to the preview;
                        // the size is what makes a row worth scanning.
                        name: human_size(size),
                        children: 0,
                    },
                    // The index promises bytes the store has not got. The row
                    // keeps the asset it names, so the reader can still take the
                    // dangling entry off.
                    Err(e) => unreadable(selection, asset.name, &e),
                });
            }
        }
    }
    Ok(out)
}

/// An asset's size, from the file's own metadata rather than by reading it: a
/// level of assets must not pull every payload into memory to draw its rows.
///
/// The refusal names the corruption and not the asset, because the row that shows
/// it names the asset in its own identifier column.
fn asset_size(store: &Store, container: &Container, name: &str) -> Result<u64> {
    let path = match container {
        Container::Epic(id) => store.epic_asset_dir(id).join(name),
        Container::Node(r) => store.node_asset_dir(&r.epic_id, r.number).join(name),
    };
    Ok(std::fs::metadata(path)
        .context("indexed, but its bytes are missing")?
        .len())
}

/// The word an unreadable row with a usable member target leads with, so one
/// word covers every recoverable read failure rather than a reader learning one
/// per kind.
const UNREADABLE: &str = "unreadable";

/// A member the store lists but whose own data could not be read.
///
/// The level still opens and the row still points at the member, so the reader
/// sees what the store claims to hold and can still act on an entry its selection
/// can address — a dangling index entry is a thing to delete, which takes a row
/// to stand on.
///
/// The reason is the failure's own outermost words, not a browser paraphrase, and
/// the fixed word leads because a row is read with colour off as often as with it
/// on. A row is one line, so the pane the cursor faces carries the failure in
/// full: it reports the same corruption for the same member.
fn unreadable(selection: Selection, label: String, failure: &impl std::fmt::Display) -> Row {
    Row {
        selection,
        kind: RowKind::Unreadable,
        label,
        name: format!("{UNREADABLE} · {failure}"),
        children: 0,
    }
}

/// A malformed blocker entry cannot be safely removed: the core operation names
/// an entry with a parsed reference, and raw text is not one. The short reason
/// leads because the raw entry is already in the identifier column and a narrow
/// row must still say what the browser cannot do.
fn unremovable_blocker(selection: Selection, label: String) -> Row {
    Row {
        selection,
        kind: RowKind::Unreadable,
        label,
        name: "cannot be removed".to_string(),
        children: 0,
    }
}

/// An asset's bytes as text a pane can render: valid UTF-8 carrying no control
/// characters beyond tab, newline and carriage return.
///
/// Valid UTF-8 on its own is not the test. A short binary payload can decode
/// cleanly and still be control bytes, and printing those is exactly the "never
/// raw bytes" the preview has to honour.
fn as_text(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8(bytes.to_vec()).ok()?;
    text.chars()
        .all(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .then_some(text)
}

/// A byte count as a reader scans it. Binary units, because an asset is a file
/// on disk, and one decimal above bytes, because the size is a hint rather than
/// a figure to compute with.
fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * KIB;
    let size = bytes as f64;
    if size < KIB {
        format!("{bytes} B")
    } else if size < MIB {
        format!("{:.1} KiB", size / KIB)
    } else {
        format!("{:.1} MiB", size / MIB)
    }
}

/// How long ago a stamp was, coarsely and always rounded down, so "3h" is never
/// read as four. A row has room for a glance; the full timestamp is in the
/// preview.
fn age(created: Timestamp, now: Timestamp) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    // A stamp ahead of this clock is a skewed writer, not a comment from the
    // future: it reads as brand new rather than as a negative age.
    let seconds = (now.as_second() - created.as_second()).max(0);
    match seconds {
        s if s < MINUTE => "just now".to_string(),
        s if s < HOUR => format!("{}m ago", s / MINUTE),
        s if s < DAY => format!("{}h ago", s / HOUR),
        s if s < WEEK => format!("{}d ago", s / DAY),
        s => format!("{}w ago", s / WEEK),
    }
}

/// The `updated` stamp an entity carried when it was read. It travels from the
/// read that produced it to the write that names it as its precondition, and is
/// never interpreted in between: no other module compares, formats or invents
/// one, which is what keeps the freshness rule inside this seam.
///
/// The timestamp remains private even though a caller may carry this wrapper
/// between a read and its matching write. The compiler therefore rejects both
/// inventing a precondition and inspecting the value that makes one current:
///
/// ```compile_fail
/// use loti_tui::data::Stamp;
///
/// let _ = Stamp(todo!());
/// ```
///
/// ```compile_fail
/// use loti_tui::data::Stamp;
///
/// let stamp: Stamp = todo!();
/// let _ = stamp.0;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp(Timestamp);

/// One of an epic's or a node's own fields that a reader rewrites outright
/// rather than adding an entry to.
///
/// Invariant: exactly the fields an entity's edit set can carry, so each is read
/// off its own value and written into its own slot. These are the fields a letter
/// of the keyboard names; every whole field the browser replaces, letter or not,
/// is a [`Replaceable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeForm {
    /// The one-line name.
    Name,
    /// The one-line summary of scope.
    Summary,
    /// The markdown body, as long as the reader writes it. The letter that names
    /// it names the long-form text of whatever row it is pressed on, so on a
    /// comment row it reaches [`Replaceable::CommentText`] instead: which field
    /// that letter means is the row's answer and not the key's.
    Body,
}

/// One whole field the browser replaces outright.
///
/// Invariant: these are exactly the writes that can silently discard text
/// somebody else wrote, so they are exactly the writes that carry [`Stamp`] as a
/// precondition — and they are carried out by one stamped write, so there is one
/// conflict to answer however many fields a reader may replace. A further
/// replaceable field is a variant here rather than a second write of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replaceable {
    /// One of an epic's or a node's own fields.
    Field(FreeForm),
    /// One comment's text.
    ///
    /// A comment lives in its container's frontmatter, so the stamp that guards
    /// it is the container's and any concurrent change to that container refuses
    /// the edit — which is the same per-entity granularity every other
    /// replacement is guarded at.
    CommentText,
}

impl Replaceable {
    /// Every whole field the browser replaces, so a surface that has to cover them
    /// all — the shape each takes, the stamp each carries — cannot then miss one.
    pub const ALL: &'static [Replaceable] = &[
        Replaceable::Field(FreeForm::Name),
        Replaceable::Field(FreeForm::Summary),
        Replaceable::Field(FreeForm::Body),
        Replaceable::CommentText,
    ];

    /// What the field is called wherever the reader is shown it; see
    /// [`FreeForm::noun`]. A comment's is `text`, because the comment itself is
    /// what the surface's title already names.
    pub fn noun(self) -> &'static str {
        match self {
            Replaceable::Field(field) => field.noun(),
            Replaceable::CommentText => "text",
        }
    }
}

impl FreeForm {
    /// Every field of an entity a letter reaches, so a surface that has to cover
    /// them all — the keys each answers, and the hints each lists — cannot then
    /// miss one.
    pub const ALL: &'static [FreeForm] = &[FreeForm::Name, FreeForm::Summary, FreeForm::Body];

    /// What the field is called wherever the reader is shown it: the surface's
    /// title, the field's own column, the warning about discarding it. The store's
    /// own word for it, so the browser and the command line name one field one
    /// thing.
    pub fn noun(self) -> &'static str {
        match self {
            FreeForm::Name => "name",
            FreeForm::Summary => "summary",
            FreeForm::Body => "body",
        }
    }

    /// What this field holds in an entity that has been read, so a surface opens
    /// on the field it names rather than on whichever one a caller reached for.
    pub fn of(self, target: &EditTarget) -> &str {
        match self {
            FreeForm::Name => &target.name,
            FreeForm::Summary => &target.summary,
            FreeForm::Body => &target.body,
        }
    }

    /// This field as an edit set names it: exactly one of `(name, summary, body)`
    /// is present, so a replacement can never carry a field it was not aimed at.
    ///
    /// One decision for both kinds of entity, because an epic's edit set and a
    /// node's take the same three fields and a second mapping is a second thing to
    /// keep in step.
    fn edits(self, value: &str) -> (Option<String>, Option<String>, Option<String>) {
        let value = Some(value.to_string());
        match self {
            FreeForm::Name => (value, None, None),
            FreeForm::Summary => (None, value, None),
            FreeForm::Body => (None, None, value),
        }
    }
}

/// A state a row's own picker offers.
///
/// Invariant: the words are the store's own, taken from the store's types rather
/// than spelled again here, so the browser can neither invent a state nor call one
/// something the command line does not. A unit of work's states and an epic's are
/// never the same value even where they share a word: an epic's `closed` is a
/// stored flag that never touches its nodes, while a unit of work's `closed` is a
/// resolution that may take its open descendants with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// One of the five states a unit of work moves between.
    Work(NodeState),
    /// An epic carrying no closed flag. `completed` is computed from the epic's
    /// nodes rather than stored, so it is a state no picker offers and no write
    /// sets.
    EpicOpen,
    /// An epic whose stored closed flag is set.
    EpicClosed,
}

/// The states a unit of work's picker offers, in the order the state machine
/// reads: not started, under way, stuck, finished, abandoned.
const WORK_STATES: &[State] = &[
    State::Work(NodeState::ToDo),
    State::Work(NodeState::InProgress),
    State::Work(NodeState::Blocked),
    State::Work(NodeState::Done),
    State::Work(NodeState::Closed),
];

/// The states an epic's picker offers: the stored flag, off and on.
const EPIC_STATES: &[State] = &[State::EpicOpen, State::EpicClosed];

impl State {
    /// The states the row a selection points at may be put into, in the order a
    /// picker moves through them, and `None` for a row that has no state of its
    /// own.
    ///
    /// An epic and a unit of work differ here and nowhere else in this module: an
    /// epic has one stored flag, a unit of work has the state machine.
    pub fn offered(selection: &Selection) -> Option<&'static [State]> {
        match selection {
            Selection::Epic(_) => Some(EPIC_STATES),
            Selection::Node(_) => Some(WORK_STATES),
            Selection::Collection(..)
            | Selection::Label(..)
            | Selection::Comment(..)
            | Selection::Asset(..)
            | Selection::Blocker(..)
            | Selection::UnremovableBlocker(..) => None,
        }
    }

    /// What the state is called wherever the reader is shown it, which is the
    /// store's own word for it.
    pub fn wire_name(self) -> &'static str {
        match self {
            State::Work(state) => state.wire_name(),
            State::EpicOpen => EpicStatus::Open.wire_name(),
            State::EpicClosed => EpicStatus::Closed.wire_name(),
        }
    }

    /// Whether this state says why, so a surface picking it asks for a reason.
    ///
    /// Blocking and resolving-without-completing both record why; the states that
    /// carry no reason clear whatever the row was holding.
    pub fn needs_reason(self) -> bool {
        match self {
            State::Work(NodeState::Blocked | NodeState::Closed) | State::EpicClosed => true,
            State::Work(NodeState::ToDo | NodeState::InProgress | NodeState::Done)
            | State::EpicOpen => false,
        }
    }

    /// Whether picking this state can take the row's open descendants with it.
    ///
    /// Only a unit of work's close: an epic's closed flag never touches its nodes,
    /// so an epic's picker has nothing to offer here.
    pub fn cascades(self) -> bool {
        matches!(self, State::Work(NodeState::Closed))
    }
}

/// One row's state as a picker starts from it: the states it offers, the one it
/// is in, and how much a cascade would have to close.
///
/// Read when the letter is pressed rather than when the cursor last moved, for the
/// same reason a replaced field's text is: the picker must start on the state the
/// store holds now. No stamp travels with it — a state pick carries no
/// precondition, because its conflict is the later of two deliberate choices
/// rather than text silently lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateTarget {
    /// What was read, so a surface can name its target and write back to it.
    pub selection: Selection,
    /// The states this row's picker offers, in the order it moves through them.
    pub offered: &'static [State],
    /// The state the row is in now, which is where the picker's highlight starts.
    pub current: State,
    /// How many of the row's descendants are still open, which is what a cascade
    /// would close.
    ///
    /// Advisory: a state pick carries no precondition, so the store recomputes the
    /// plan under the lock and may close more or fewer than this promised. Always
    /// none for an epic — an epic's closed flag never touches its nodes, so nothing
    /// of an epic's cascades.
    pub open_descendants: usize,
}

/// Re-read one row's state for a picker; see [`StateTarget`].
///
/// An epic's state is its stored flag and not the state its row shows: `completed`
/// is computed from its nodes, so the flag is what a reader picks and the computed
/// word is not offered at all.
pub fn state_target(store: &Store, selection: &Selection) -> Result<StateTarget> {
    let Some(offered) = State::offered(selection) else {
        anyhow::bail!("{} has no state of its own", selection.reference())
    };
    let (current, open_descendants) = match selection {
        Selection::Epic(id) => {
            let epic = ops::read_epic(store, id)?;
            let current = match epic.frontmatter.closed {
                true => State::EpicClosed,
                false => State::EpicOpen,
            };
            (current, 0)
        }
        Selection::Node(r) => {
            let node = ops::read_node(store, r)?;
            let open = ops::descendants_of(store, r)?
                .iter()
                .filter(|d| !d.state.is_terminal())
                .count();
            (State::Work(node.frontmatter.status), open)
        }
        // Every selection [`State::offered`] answers for is covered above, so a row
        // with no state never reaches here.
        Selection::Collection(..)
        | Selection::Label(..)
        | Selection::Comment(..)
        | Selection::Asset(..)
        | Selection::Blocker(..)
        | Selection::UnremovableBlocker(..) => {
            anyhow::bail!("{} has no state of its own", selection.reference())
        }
    };
    Ok(StateTarget {
        selection: selection.clone(),
        offered,
        current,
        open_descendants,
    })
}

/// One entity as an editing surface starts from it: the fields a surface can
/// replace, plus the stamp they were read at.
///
/// The fields are the free-form replacements — see [`FreeForm`] — which are
/// exactly the writes that can silently discard someone else's text, so they are
/// the writes that carry [`Stamp`] as a precondition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditTarget {
    /// What was read, so a surface can name its target and write back to it.
    pub selection: Selection,
    /// The one-line name.
    pub name: String,
    /// The one-line summary of scope.
    pub summary: String,
    /// The markdown body, verbatim as stored.
    pub body: String,
    /// The stamp the three fields above were read at.
    pub stamp: Stamp,
}

/// Re-read one entity for editing.
///
/// Called when an editing action is initiated — not when editing mode is entered
/// and not when the cursor last moved — so the buffer starts from the current
/// text rather than from a preview that may be minutes old, and the conflict
/// window the stamp opens is only as long as the edit itself.
///
/// Only an epic and a node have these fields: a collection and its members are
/// edited by their own operations, not by replacing a name, a summary or a body.
pub fn edit_target(store: &Store, selection: &Selection) -> Result<EditTarget> {
    let (name, summary, body, updated) = match selection {
        Selection::Collection(..)
        | Selection::Label(..)
        | Selection::Comment(..)
        | Selection::Asset(..)
        | Selection::Blocker(..)
        | Selection::UnremovableBlocker(..) => {
            anyhow::bail!(
                "{} has no name, summary or body of its own",
                selection.reference()
            )
        }
        Selection::Epic(id) => {
            let epic = loti_core::ops::read_epic(store, id)?;
            (
                epic.frontmatter.name,
                epic.frontmatter.summary,
                epic.body,
                epic.frontmatter.updated,
            )
        }
        Selection::Node(r) => {
            let node = loti_core::ops::read_node(store, r)?;
            (
                node.frontmatter.name,
                node.frontmatter.summary,
                node.body,
                node.frontmatter.updated,
            )
        }
    };
    Ok(EditTarget {
        selection: selection.clone(),
        name,
        summary,
        body,
        stamp: Stamp(updated),
    })
}

/// One comment as an editing surface starts from it: the text it holds, and the
/// stamp that text was read at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentTarget {
    /// What was read, so a surface can name its target and write back to it.
    pub selection: Selection,
    /// The comment's text, verbatim as stored.
    pub text: String,
    /// The stamp the text was read at, which is the container's: a comment lives
    /// in its container's frontmatter and has no stamp of its own, so what guards
    /// the rewrite is the container not having moved on.
    pub stamp: Stamp,
}

/// Re-read one comment for editing, at the instant the letter is pressed; see
/// [`edit_target`] for why the read is not any earlier.
///
/// A comment that is not there, or that has been withdrawn since the letter was
/// offered, is refused by name: its text is withheld once it is withdrawn, so
/// there is nothing to open a buffer on. Nothing about the author is judged here
/// — the row carries that, and the store enforces it.
pub fn comment_target(store: &Store, selection: &Selection) -> Result<CommentTarget> {
    let Selection::Comment(container, id) = selection else {
        anyhow::bail!("{} is not a comment", selection.reference())
    };
    let (comments, updated) = match container {
        Container::Epic(epic_id) => {
            let epic = ops::read_epic(store, epic_id)?;
            (epic.frontmatter.comments, epic.frontmatter.updated)
        }
        Container::Node(r) => {
            let node = ops::read_node(store, r)?;
            (node.frontmatter.comments, node.frontmatter.updated)
        }
    };
    let held = comments
        .into_iter()
        .find(|comment| comment.id == *id && !comment.deleted)
        .ok_or_else(|| anyhow::anyhow!("{} has no text to edit", selection.reference()))?;
    Ok(CommentTarget {
        selection: selection.clone(),
        text: held.text,
        stamp: Stamp(updated),
    })
}

/// A change to the store, naming what is written and what it is written to.
///
/// Invariant: a dialog carries the write its answer performs, so the state
/// machine that answers a question names no operation of its own — a new
/// confirmation is a value here rather than another branch there.
///
/// A write names its target by the row's own [`Selection`], not by a pre-checked
/// pair of parts: the seam that carries it out is the one place that judges
/// whether the target is the kind of thing the write applies to, and refuses by
/// name when it is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    /// Create an epic, with the id the reader typed and the name and summary they
    /// filled in.
    ///
    /// The id travels as the selection that will address the epic, because an id is
    /// exactly how an epic is addressed: what this write is aimed at is the thing it
    /// creates. Whether that id is already taken is the store's to say, so nothing
    /// here asks first.
    ///
    /// No stamp: a stamp is the precondition of a free-form replacement, and an
    /// entity that does not exist yet holds nobody's text to discard.
    CreateEpic {
        /// The epic to bring into being, addressed by the id it will have.
        epic: Selection,
        /// What it is called.
        name: String,
        /// Its one-line summary, which may be empty.
        summary: String,
    },
    /// Create a unit of work in the container the row names: a top-level ticket of
    /// an epic, or a subticket of a ticket.
    ///
    /// Which of the two is the row's answer rather than this write's — creation
    /// acts on the container the cursor stands on — so one write covers both and
    /// no caller chooses between them.
    ///
    /// No stamp, for the same reason an epic's creation carries none.
    CreateNode {
        /// The container it is created in: an epic, or the ticket it hangs under.
        parent: Selection,
        /// What it is called.
        name: String,
        /// Its one-line summary, which may be empty.
        summary: String,
    },
    /// Put one label on the container whose label set the row names, with the
    /// text the reader typed.
    AddLabel(Selection, String),
    /// Take one label off the container it sits on.
    RemoveLabel(Selection),
    /// Put one blocker on the dependency list the row names, from the reference
    /// the reader typed. Whether that reference names a node at all, and whether
    /// that node may block this one, are the store's to judge.
    AddBlocker(Selection, String),
    /// Take one entry off the dependency list it sits on.
    RemoveBlocker(Selection),
    /// Take one asset off the container it hangs on, index entry and bytes alike.
    DeleteAsset(Selection),
    /// Put one comment on the container whose comment list the row names, with the
    /// text the reader wrote.
    ///
    /// Authored by the human, always: the browser writes as the human and only the
    /// human, so who is writing is never something a surface asks or a caller
    /// supplies.
    ///
    /// No stamp: an append takes a slot of its own rather than replacing one, so it
    /// cannot discard text somebody else wrote and both survive.
    AddComment(Selection, String),
    /// Withdraw one comment of the container it sits on.
    ///
    /// The text goes and the comment does not: the store keeps the slot, so the
    /// number stays taken and is never reused. No stamp, for the same reason an
    /// append carries none — flagging one slot replaces nobody's text.
    DeleteComment(Selection),
    /// Take the claim on the node the row names, for the holder the reader typed.
    ///
    /// A claim has one holder, so taking one that is already held reassigns it.
    /// The holder is freeform text and is not attribution — it records who is on
    /// the work, not who wrote the change — and when the claim was taken is the
    /// store's to maintain, so no instant travels with it.
    TakeClaim(Selection, String),
    /// Give up the claim on the node the row names, holder and timestamp
    /// together.
    ReleaseClaim(Selection),
    /// Put the epic or node the row names into a state.
    ///
    /// No stamp: a state pick is not a free-form replacement, and its conflict
    /// story is that the later of two deliberate choices wins.
    SetState {
        /// The epic or node whose state is set.
        target: Selection,
        /// The state to put it in, which is the one the picker holds.
        state: State,
        /// Why, for a state that says why. Written as the reader left it: what
        /// makes a reason acceptable is the store's rule, and a state that carries
        /// none discards this rather than storing an unread word.
        reason: String,
        /// Whether closing takes the row's open descendants with it. The store
        /// recomputes the plan under the lock, so what this asks for is a cascade
        /// and never a particular set of nodes.
        cascade: bool,
    },
    /// Replace one whole field the row names.
    ///
    /// The one write here that can silently discard text somebody else wrote —
    /// which is what the stamp is for — and the only one, whichever field it
    /// names: one write shape means one conflict for a reader to answer.
    Replace {
        /// The epic, node or comment whose field is replaced.
        target: Selection,
        /// Which whole field of it.
        field: Replaceable,
        /// The replacement, exactly as the reader left it: what makes a value
        /// acceptable is the store's rule, and the browser normalises none of it.
        value: String,
        /// The stamp the replaced text was read at, applied only while the entity
        /// still carries it. `None` names no precondition — last write wins — which
        /// is what a reader chooses by answering a conflict with overwrite, and
        /// nothing else in the browser chooses for them.
        expect: Option<Stamp>,
    },
}

impl Write {
    /// What the write is aimed at, which is how the reader addresses what it
    /// changes: a question raised about a write names this rather than re-deriving
    /// a reference of its own.
    pub fn target(&self) -> &Selection {
        match self {
            Write::CreateEpic { epic: target, .. }
            | Write::CreateNode { parent: target, .. }
            | Write::AddLabel(target, _)
            | Write::RemoveLabel(target)
            | Write::AddBlocker(target, _)
            | Write::RemoveBlocker(target)
            | Write::DeleteAsset(target)
            | Write::AddComment(target, _)
            | Write::DeleteComment(target)
            | Write::TakeClaim(target, _)
            | Write::ReleaseClaim(target)
            | Write::SetState { target, .. }
            | Write::Replace { target, .. } => target,
        }
    }

    /// The same write with its precondition dropped, which is what overwriting a
    /// conflict performs: the reader has seen that the entity moved on and said to
    /// write anyway, so naming a stamp again would refuse for the same reason.
    ///
    /// A write that names no stamp cannot conflict, so for every other write this
    /// is the write itself.
    pub fn overwriting(&self) -> Write {
        match self {
            Write::Replace {
                target,
                field,
                value,
                ..
            } => Write::Replace {
                target: target.clone(),
                field: *field,
                value: value.clone(),
                expect: None,
            },
            other => other.clone(),
        }
    }
}

/// What a write did that the browser could not have said in advance.
///
/// Invariant: a notice about a write is finished *after* the write rather than
/// worded with the question that raised it, because the size of a cascade is
/// something only the write knows — the count a surface showed was the plan as it
/// stood when the surface opened, and the store recomputes it under the lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Exactly what was asked for and nothing besides. Writes whose outcome adds
    /// information the browser could not know use a more specific variant.
    AsAsked,
    /// The reference the store recorded a newly created unit of work under.
    ///
    /// The browser cannot know it in advance: a number is allocated from its epic's
    /// pool under the lock, and the reference that number makes is the only name
    /// the new ticket has. An epic's own id is not one of these — the reader typed
    /// it, so it was knowable before the write.
    Created(String),
    /// A dependency list already held the blocker the reader asked to add. The
    /// core operation is deliberately idempotent, so the browser reports the
    /// non-event rather than claiming a duplicate was added.
    AlreadyListed(String),
    /// A close that resolved this many of the row's open descendants with it.
    ///
    /// Never none: a cascade that finds nothing left to close — somebody else got
    /// there first — is an ordinary single close and is reported as one rather than
    /// as a cascade of nothing.
    AlsoClosed(usize),
    /// The number the store gave a comment as it was added.
    ///
    /// The browser cannot know it in advance: a comment's number is assigned under
    /// the lock, from what the list already holds, and it is the only name the new
    /// comment has.
    Commented(u64),
}

impl Effect {
    /// What a close did, given how many descendants went with it.
    fn of_cascade(closed: usize) -> Self {
        match closed {
            0 => Effect::AsAsked,
            closed => Effect::AlsoClosed(closed),
        }
    }
}

/// Why the store would not take a write.
///
/// Invariant: the one refusal the browser reacts to differently is told apart
/// here and nowhere else — the surface that reacts to it matches on this rather
/// than reading a message — and every other refusal travels in the store's own
/// words, verbatim, because those words are what the reader is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The entity moved on since the stamp the write named, so nothing was
    /// written. The reader is asked about this one rather than told: only they can
    /// decide whether their text should replace the change that landed under it.
    Conflict,
    /// Every other refusal that leaves the store as it was, in the store's own
    /// words, so the browser and the command line teach the same rule in the same
    /// words.
    Rule(String),
    /// A cascade closed one or more descendants before it stopped. Its message is
    /// still the store's own, but the browser must reload before showing it so its
    /// rows do not contradict the partial progress.
    Partial(String),
}

impl Refusal {
    /// Whether the store changed before refusing the write.
    ///
    /// This is a typed outcome rather than an inference from a refusal message, so
    /// the browser has one decision point for reloading after every changed write.
    pub fn changed(&self) -> bool {
        matches!(self, Refusal::Partial(..))
    }
}

/// Carry out a write, returning the store's own refusal when it refuses.
///
/// The browser never judges a write itself: only the store can, so the action is
/// offered, attempted, and whatever comes back is shown — which is why nothing
/// here pre-checks a store rule.
pub fn perform(store: &Store, write: &Write) -> Result<Effect, Refusal> {
    match write {
        Write::CreateEpic {
            epic,
            name,
            summary,
        } => as_asked(create_epic(store, epic, name, summary)),
        // The third write whose notice only the store can finish: which number the
        // new ticket took is decided under the lock.
        Write::CreateNode {
            parent,
            name,
            summary,
        } => create_node(store, parent, name, summary),
        // Most writes report exactly what was asked. The operations whose outcome
        // adds information the browser could not know return a specific effect so
        // the notice is finished from the store's result rather than guessed.
        Write::AddLabel(selection, label) => as_asked(add_label(store, selection, label)),
        Write::RemoveLabel(selection) => as_asked(remove_label(store, selection)),
        Write::AddBlocker(selection, reference) => add_blocker(store, selection, reference),
        Write::RemoveBlocker(selection) => as_asked(remove_blocker(store, selection)),
        Write::DeleteAsset(selection) => as_asked(delete_asset(store, selection)),
        // The second write whose notice only the store can finish: which number the
        // new comment took is decided under the lock.
        Write::AddComment(selection, text) => add_comment(store, selection, text),
        Write::DeleteComment(selection) => as_asked(delete_comment(store, selection)),
        Write::TakeClaim(selection, holder) => as_asked(take_claim(store, selection, holder)),
        Write::ReleaseClaim(selection) => as_asked(release_claim(store, selection)),
        Write::SetState {
            target,
            state,
            reason,
            cascade,
        } => set_state(store, target, *state, reason, *cascade),
        Write::Replace {
            target,
            field,
            value,
            expect,
        } => as_asked(replace(store, target, *field, value, *expect)),
    }
}

/// A write that can only ever do what it was asked for.
fn as_asked(result: Result<(), Refusal>) -> Result<Effect, Refusal> {
    result.map(|()| Effect::AsAsked)
}

/// The refusal an operation's failure is reported as: a stale precondition, or
/// the store's own words for everything else.
///
/// The classification is made on the store's own error type rather than on its
/// message, so a reworded refusal cannot silently stop being a conflict.
///
/// The store refuses a target that is not there under the lock the same way it
/// refuses a stale stamp, but every operation the browser calls checks existence
/// before taking the lock, so what reaches a reader as a question is a stamp that
/// moved and a target that is gone is refused by name.
fn refusal(error: ops::OpError) -> Refusal {
    match error {
        ops::OpError::Store(loti_core::store::StoreError::Conflict { .. }) => Refusal::Conflict,
        // The cascade outcome says whether an earlier independent descendant
        // write committed. Keep that fact with the store's message, rather than
        // asking callers to recognise it from wording that may change.
        ops::OpError::CascadePartial {
            committed: true, ..
        } => Refusal::Partial(error.to_string()),
        ops::OpError::CascadePartial {
            committed: false, ..
        } => Refusal::Rule(error.to_string()),
        other => Refusal::Rule(other.to_string()),
    }
}

/// A refusal the browser itself makes: a write aimed at a row that cannot take
/// it, which is a caller that has lost track of what its row points at rather
/// than anything the store was asked.
fn misdirected(message: String) -> Refusal {
    Refusal::Rule(message)
}

/// Replace one whole field with the text the reader left, applying only while the
/// entity that holds it still carries the stamp that text was read at.
///
/// These are the writes the browser makes that replace a whole field, so they are
/// the ones that can discard text somebody else wrote: the stamp is the store's
/// precondition, checked under the lock, and a mismatch writes nothing. Whether
/// the value is acceptable at all is the store's rule; the text is written exactly
/// as the reader left it, trailing newline and all, as the command line writes
/// what it is given.
///
/// One path for every replaceable field, so a field cannot be given a conflict
/// story of its own — nor be added without a precondition.
fn replace(
    store: &Store,
    selection: &Selection,
    field: Replaceable,
    value: &str,
    expect: Option<Stamp>,
) -> Result<(), Refusal> {
    let expect_updated = expect.map(|stamp| stamp.0);
    let field = match field {
        Replaceable::Field(field) => field,
        // A comment's text is replaced by its own operation, because a comment is
        // not a field of the entity's frontmatter but one entry of a list in it —
        // and because the store checks there what an entity's fields have no rule
        // for: a comment is its author's alone to rewrite.
        Replaceable::CommentText => {
            let Selection::Comment(container, id) = selection else {
                return Err(misdirected(format!(
                    "{} is not a comment",
                    selection.reference()
                )));
            };
            return ops::edit_comment(
                store,
                &container.target(),
                *id,
                // The browser writes as the human and only the human, so the actor
                // is fixed here rather than asked for: attribution is never
                // something a surface offers to fill in.
                Actor::Human,
                value.to_string(),
                expect_updated,
            )
            .map(|_| ())
            .map_err(refusal);
        }
    };
    let (name, summary, body) = field.edits(value);
    match selection {
        Selection::Epic(id) => ops::edit_epic(
            store,
            id,
            ops::EpicEdits {
                name,
                summary,
                body,
                expect_updated,
            },
        )
        .map(|_| ())
        .map_err(refusal),
        Selection::Node(r) => ops::edit_node(
            store,
            r,
            ops::NodeEdits {
                name,
                summary,
                body,
                expect_updated,
                ..Default::default()
            },
        )
        .map(|_| ())
        .map_err(refusal),
        // Only an epic and a node have these fields of their own, and only their
        // rows offer the action, so any other selection is a caller that has lost
        // track of what its row points at.
        Selection::Collection(..)
        | Selection::Label(..)
        | Selection::Comment(..)
        | Selection::Asset(..)
        | Selection::Blocker(..)
        | Selection::UnremovableBlocker(..) => Err(misdirected(format!(
            "{} has no {} of its own",
            selection.reference(),
            field.noun()
        ))),
    }
}

/// Put an epic or a node into a state, with the reason that state carries and
/// whether closing takes the row's open descendants with it.
///
/// Every rule about a state is the store's: that `done` waits on open descendants,
/// that blocking and closing say why, and how much a cascade closes. So nothing is
/// checked here — the pick is attempted and whatever comes back is what the reader
/// is shown, which is why the browser cannot go stale when one of those rules gains
/// a nuance.
///
/// A cascade is not atomic. It closes each descendant on its own and stops at the
/// first failure, leaving a half-closed subtree; the store's refusal names where it
/// stopped and says to re-run, and every step is idempotent, so re-running is the
/// whole of the recovery.
///
/// No stamp guards this write: a stamp is the precondition of a free-form
/// replacement, and a state pick's conflict is the later of two deliberate choices.
fn set_state(
    store: &Store,
    selection: &Selection,
    state: State,
    reason: &str,
    cascade: bool,
) -> Result<Effect, Refusal> {
    // A state that carries no reason takes none: the reason field is not on screen
    // for those states, so anything left in it is text the reader is not looking at.
    let reason = state.needs_reason().then(|| reason.to_string());
    match selection {
        Selection::Node(node) => match state {
            State::Work(work) => {
                let change = match work {
                    NodeState::ToDo => NodeStatusChange::ToDo,
                    NodeState::InProgress => NodeStatusChange::InProgress,
                    NodeState::Blocked => NodeStatusChange::Blocked { reason },
                    NodeState::Done => NodeStatusChange::Done,
                    NodeState::Closed => NodeStatusChange::Closed { reason, cascade },
                };
                let outcome = ops::set_node_status(store, node, change).map_err(refusal)?;
                // How many descendants went with it is the store's answer and not
                // the plan's: it recomputed that plan under the lock.
                Ok(Effect::of_cascade(outcome.cascaded_closed.len()))
            }
            // An epic's flag is not a state of the work: a caller offering it here
            // has lost track of what its row points at.
            State::EpicOpen | State::EpicClosed => Err(wrong_state(selection, state)),
        },
        Selection::Epic(id) => match state {
            State::EpicOpen | State::EpicClosed => {
                let closed = matches!(state, State::EpicClosed);
                ops::set_epic_closed(store, id, closed, reason).map_err(refusal)?;
                // An epic's flag never touches its nodes, so an epic's close is
                // always exactly what was asked for.
                Ok(Effect::AsAsked)
            }
            State::Work(_) => Err(wrong_state(selection, state)),
        },
        // Only an epic and a node have a state of their own, and only their rows
        // offer the letter, so any other selection is a caller that has lost track
        // of what its row points at.
        Selection::Collection(..)
        | Selection::Label(..)
        | Selection::Comment(..)
        | Selection::Asset(..)
        | Selection::Blocker(..)
        | Selection::UnremovableBlocker(..) => Err(wrong_state(selection, state)),
    }
}

/// A state aimed at a row that has no such state; see [`misdirected`].
fn wrong_state(selection: &Selection, state: State) -> Refusal {
    misdirected(format!(
        "{} cannot be {}",
        selection.reference(),
        state.wire_name()
    ))
}

/// Create an epic under the id the reader typed.
///
/// Whether the id is free is the store's rule and is not asked here: a duplicate
/// comes back as the store's own refusal, in the store's own words, so that rule
/// lives in exactly one place. What the *browser* insists on about an id is the
/// shape of it, and that is settled before a write is built at all.
///
/// The new epic starts with no labels and an empty body: a creation form asks for
/// what a row cannot be read without, and everything else is edited afterwards by
/// the letter that owns it.
fn create_epic(store: &Store, epic: &Selection, name: &str, summary: &str) -> Result<(), Refusal> {
    let Selection::Epic(id) = epic else {
        return Err(misdirected(format!(
            "{} is not an epic id",
            epic.reference()
        )));
    };
    ops::create_epic(
        store,
        ops::NewEpic {
            epic_id: id.clone(),
            name: name.to_string(),
            summary: summary.to_string(),
            labels: Vec::new(),
            body: String::new(),
        },
    )
    .map(|_| ())
    .map_err(refusal)
}

/// Create a unit of work in the container the row names: a top-level ticket of an
/// epic, or a subticket of the ticket the row stands on.
///
/// Which of the two comes off the container alone, because creation acts on the
/// container the cursor stands on — so there is nothing here to choose and no way
/// to ask for a subticket of an epic. Any other kind of row holds no units of
/// work, and is a caller that has lost track of what its row points at.
///
/// The number is the store's to allocate, out of its epic's pool and under the
/// lock, so the reference is answered back rather than predicted: it is the only
/// name the new ticket has, and the reader is told which ticket theirs became.
fn create_node(
    store: &Store,
    parent: &Selection,
    name: &str,
    summary: &str,
) -> Result<Effect, Refusal> {
    let (epic_id, under) = match parent {
        Selection::Epic(id) => (id.clone(), None),
        Selection::Node(r) => (r.epic_id.clone(), Some(r.clone())),
        other => {
            return Err(misdirected(format!(
                "{} holds no tickets",
                other.reference()
            )))
        }
    };
    let created = ops::create_node(
        store,
        ops::NewNode {
            epic_id: epic_id.clone(),
            parent: under,
            name: name.to_string(),
            summary: summary.to_string(),
            labels: Vec::new(),
            body: String::new(),
        },
    )
    .map_err(refusal)?;
    Ok(Effect::Created(
        NodeRef::new(epic_id, created.frontmatter.number).to_string(),
    ))
}

/// Put one label on a container's label set.
///
/// The text is written exactly as it was typed: whether a given string is a label
/// the store will take is the store's rule, and the browser reimplements none of
/// them — an already-present label is the store's own no-op, not a refusal the
/// browser invents.
///
/// No stamp guards this write, for the same reason a removal carries none: a
/// stamp is the precondition of a free-form replacement, and adding one member to
/// a set cannot silently discard text someone else wrote.
fn add_label(store: &Store, selection: &Selection, label: &str) -> Result<(), Refusal> {
    // Only the label set's own row offers an addition, so any other selection is a
    // caller that has lost track of what its row points at.
    let Selection::Collection(container, Collection::Labels) = selection else {
        return Err(misdirected(format!(
            "{} is not a label set",
            selection.reference()
        )));
    };
    ops::add_labels(store, &container.target(), &[label.to_string()]).map_err(refusal)?;
    Ok(())
}

/// Take one label off the container it sits on.
///
/// A label set has no rename, so a label is only ever removed: renaming one is
/// remove-then-add, which is two edits and so two editing sessions.
///
/// A refusal is the store's own message and nothing else — no wrapping context,
/// no reworded rule — because the browser shows it verbatim, so the browser and
/// the CLI teach the same rule in the same words and neither can go stale when a
/// store rule gains a nuance.
///
/// No stamp guards this write: a stamp is the precondition of a free-form
/// replacement, and removing one member of a set cannot silently discard text
/// someone else wrote.
fn remove_label(store: &Store, selection: &Selection) -> Result<(), Refusal> {
    // Only a label row offers removal, so any other selection is a caller that
    // has lost track of what its row points at.
    let Selection::Label(container, label) = selection else {
        return Err(misdirected(format!(
            "{} is not a label",
            selection.reference()
        )));
    };
    ops::remove_labels(store, &container.target(), std::slice::from_ref(label)).map_err(refusal)?;
    Ok(())
}

/// Put one blocker on a node's dependency list, from the reference as the reader
/// typed it.
///
/// Nothing about the reference is judged here beyond which node it names: a
/// blocker that does not exist, and a node blocking itself, are refused by the
/// store in the store's own words, so each of those rules lives in exactly one
/// place and the browser cannot go stale when one of them gains a nuance.
///
/// No stamp guards this write, for the same reason a label addition carries none:
/// a stamp is the precondition of a free-form replacement, and adding one entry
/// to a list cannot silently discard text someone else wrote.
fn add_blocker(store: &Store, selection: &Selection, reference: &str) -> Result<Effect, Refusal> {
    let (node, blocker) = blocked_and_blocking(selection, reference).map_err(misdirected)?;
    let canonical = blocker.to_string();
    // The core keeps a dependency list unique. Read the list through the same
    // seam before asking it to add, so a successful idempotent operation is not
    // reported as a change the reader cannot see.
    let already_listed = ops::list_blocked_by(store, &node)
        .map_err(refusal)?
        .contains(&canonical);
    ops::add_blocked_by(store, &node, std::slice::from_ref(&blocker)).map_err(refusal)?;
    Ok(match already_listed {
        true => Effect::AlreadyListed(canonical),
        false => Effect::AsAsked,
    })
}

/// Take one entry off the dependency list it sits on.
///
/// A dependency list has no rename either, so an entry is only ever removed. No
/// reference is typed to remove one: the row carries both the node that is
/// blocked and the node blocking it, so there is nothing here to re-derive and no
/// second chance to name the wrong entry.
fn remove_blocker(store: &Store, selection: &Selection) -> Result<(), Refusal> {
    // Only a blocker row offers removal, so any other selection is a caller that
    // has lost track of what its row points at.
    let Selection::Blocker(Container::Node(node), blocker) = selection else {
        return Err(misdirected(format!(
            "{} is not a node's blocker",
            selection.reference()
        )));
    };
    ops::remove_blocked_by(store, node, std::slice::from_ref(blocker)).map_err(refusal)?;
    Ok(())
}

/// Take one asset off the container it hangs on.
///
/// An asset is only ever deleted from the browser: attaching one is file-picking
/// and binary round-tripping, which the command line does and the browser does
/// not, so there is no addition here for a deletion to be the other half of.
///
/// The deletion is hard — the index entry and the bytes both go, and the store
/// keeps no tombstone for an asset the way it does for a comment — which is what
/// the confirmation in front of it is for.
///
/// No stamp guards this write, for the same reason a label removal carries none:
/// a stamp is the precondition of a free-form replacement, and taking one member
/// out of a collection cannot silently discard text someone else wrote.
fn delete_asset(store: &Store, selection: &Selection) -> Result<(), Refusal> {
    // Only an asset row offers a deletion, so any other selection is a caller
    // that has lost track of what its row points at.
    let Selection::Asset(container, name) = selection else {
        return Err(misdirected(format!(
            "{} is not an asset",
            selection.reference()
        )));
    };
    ops::delete_asset(store, &container.target(), name).map_err(refusal)?;
    Ok(())
}

/// Put one comment on a container's comment list, authored by the human.
///
/// The browser writes as the human and only the human, so no attribution reaches
/// this: an agent's comment is something the command line writes, and there is no
/// channel here to write one by.
///
/// The text is written exactly as the reader left it, trailing newline and all:
/// whether a given string is a comment the store will take is the store's rule.
///
/// The number is the store's to assign, from what the list already holds and under
/// the lock, so it is answered back rather than predicted — the reader is told
/// which comment theirs became.
fn add_comment(store: &Store, selection: &Selection, text: &str) -> Result<Effect, Refusal> {
    // Creation acts on the container row the cursor stands on, so any other
    // selection is a caller that has lost track of what its row points at.
    let Selection::Collection(container, Collection::Comments) = selection else {
        return Err(misdirected(format!(
            "{} is not a comment list",
            selection.reference()
        )));
    };
    let added = ops::add_comment(store, &container.target(), Actor::Human, text.to_string())
        .map_err(refusal)?;
    Ok(Effect::Commented(added.id))
}

/// Withdraw one comment of the container it sits on.
///
/// The comment is retained and flagged rather than removed, so its number stays
/// taken and is never reused, and the row keeps standing where it stood with its
/// text withheld. That is what makes this the one deletion the browser makes that
/// destroys nothing — the confirmation in front of it is the uniform rule that
/// every deletion asks, not a measure of what is at stake.
///
/// Who may withdraw a comment is the store's rule and is not re-checked here: only
/// its author may, and the browser writes as the human, so a comment the human did
/// not write is refused by the store in the store's own words.
fn delete_comment(store: &Store, selection: &Selection) -> Result<(), Refusal> {
    // Only a comment row offers a withdrawal, so any other selection is a caller
    // that has lost track of what its row points at.
    let Selection::Comment(container, id) = selection else {
        return Err(misdirected(format!(
            "{} is not a comment",
            selection.reference()
        )));
    };
    ops::delete_comment(store, &container.target(), *id, Actor::Human).map_err(refusal)?;
    Ok(())
}

/// Take the claim on a node for the holder the reader typed.
///
/// A claim has one holder, so taking one that is already held reassigns it: the
/// prior holder is replaced rather than joined, and that is the whole of what
/// taking an already-held claim means.
///
/// The holder is freeform text and has nothing to do with attribution: it says
/// who is on the work, not who wrote the change, so nothing about the writer's
/// identity reaches this. The store trims the holder and refuses one that trims
/// to nothing; beyond that, whether a given string is a holder the store will
/// take is the store's rule.
///
/// When the claim was taken is the store's to maintain and is never passed, so
/// no clock outside the store can disagree with the instant it recorded.
///
/// No stamp guards this write: a stamp is the precondition of a free-form
/// replacement, and a claim's conflict is the later of two deliberate choices
/// rather than text silently lost.
fn take_claim(store: &Store, selection: &Selection, holder: &str) -> Result<(), Refusal> {
    // A claim is taken on a unit of work, so any other selection is a caller that
    // has lost track of what its row points at.
    let Selection::Node(node) = selection else {
        return Err(misdirected(format!(
            "{} cannot be claimed",
            selection.reference()
        )));
    };
    ops::take_claim(store, node, holder).map_err(refusal)?;
    Ok(())
}

/// Give up a node's claim, holder and timestamp together.
///
/// The row carries whether a claim is held, so nothing is typed to release one
/// and there is no holder here to name the wrong one with. No stamp guards it,
/// for the same reason taking one carries none.
fn release_claim(store: &Store, selection: &Selection) -> Result<(), Refusal> {
    // See [`take_claim`]: only a unit of work has a claim of its own.
    let Selection::Node(node) = selection else {
        return Err(misdirected(format!(
            "{} has no claim of its own",
            selection.reference()
        )));
    };
    ops::release_claim(store, node).map_err(refusal)?;
    Ok(())
}

/// The node whose dependency list is being written, and the node a typed
/// reference names.
///
/// An epic is not a unit of work that can be blocked, so it carries no dependency
/// list and is never offered one: any selection that is not a node's dependency
/// list is a caller that has lost track of what its row points at.
fn blocked_and_blocking(
    selection: &Selection,
    reference: &str,
) -> Result<(NodeRef, NodeRef), String> {
    let Selection::Collection(Container::Node(node), Collection::BlockedBy) = selection else {
        return Err(format!(
            "{} is not a dependency list",
            selection.reference()
        ));
    };
    let blocker = resolve_blocker(&node.epic_id, reference)?;
    Ok((node.clone(), blocker))
}

/// The node a typed blocker reference names: a bare number is a node of the
/// blocked node's own epic, and anything else is a whole `<epic-id>/<number>`,
/// which reaches any epic.
///
/// Both forms are accepted because both are how a reader writes a reference, and
/// surrounding blanks are dropped because a reference is a token rather than
/// text. Whether the node it names exists is not asked here — a refusal comes
/// back from the store, in the store's words, and so does the refusal of a
/// reference that is no reference at all.
fn resolve_blocker(epic_id: &str, reference: &str) -> Result<NodeRef, String> {
    let reference = reference.trim();
    if let Ok(number) = reference.parse::<u64>() {
        return Ok(NodeRef::new(epic_id, number));
    }
    NodeRef::parse(reference).map_err(|e| e.to_string())
}

/// How a blocker written from a typed reference is named to the reader: the
/// canonical reference the store records it under, so a notice about a bare
/// number does not read as a ticket belonging to no epic.
///
/// A reference that names no node is named back as it was typed: that write is
/// refused, so nothing is written and no notice about it is ever read.
pub fn blocker_name(selection: &Selection, reference: &str) -> String {
    match blocked_and_blocking(selection, reference) {
        Ok((_, blocker)) => blocker.to_string(),
        Err(_) => reference.trim().to_string(),
    }
}

/// The preview body for a selection.
///
/// For an epic or a node it is byte-for-byte the markdown `loti epic show` /
/// `loti ticket show` print, so there is no second document shape to keep in
/// sync with theirs. A collection and a label have no document of their own, so
/// the pane keeps the container's and stays useful — the label itself is visible
/// in that document's own metadata table; [`Selection::document`] is what makes
/// that normalisation, so this reads it back rather than repeating it. A comment
/// and an asset get a document composed here, because no other surface needs
/// one.
pub fn preview(store: &Store, selection: &Selection) -> Result<String> {
    let (value, children, comments) = match selection.document() {
        Selection::Epic(id) => (
            read::epic_json(store, &id)?,
            read::epic_children(store, &id)?,
            read::comment_lines(store, &Target::Epic(id), false)?,
        ),
        Selection::Node(r) => (
            read::node_json(store, &r)?,
            read::node_children(store, &r)?,
            read::comment_lines(store, &Target::Node(r), false)?,
        ),
        // `document()` never produces these selections: all normalise away to
        // their container's selection above, so this arm exists only for exhaustiveness.
        Selection::Collection(..) | Selection::Label(..) | Selection::UnremovableBlocker(..) => {
            unreachable!("document() normalises container-owned rows to their container")
        }
        Selection::Comment(container, id) => return comment_document(store, &container, id),
        Selection::Asset(container, name) => return asset_document(store, &container, &name),
        // A blocker's own document, so what blocks you is readable without
        // leaving the level.
        Selection::Blocker(_, r) => (
            read::node_json(store, &r)?,
            read::node_children(store, &r)?,
            read::comment_lines(store, &Target::Node(r), false)?,
        ),
    };
    Ok(render::show_markdown(&value, &children, &comments))
}

/// One comment as a document: who wrote it, when in full, and the text as
/// markdown. A withdrawn comment's text is withheld by the read, so its document
/// says that instead — and keeps its id, which is never reused.
fn comment_document(store: &Store, container: &Container, id: u64) -> Result<String> {
    let view = ops::list_comments(store, &container.target(), true)?
        .into_iter()
        .find(|v| match v {
            CommentView::Live(c) => c.id == id,
            CommentView::Tombstone { id: other, .. } => *other == id,
        })
        .with_context(|| format!("comment {id} does not exist"))?;

    let mut doc = String::from("| field | value |\n|---|---|\n");
    let (author, created, text) = match view {
        CommentView::Live(c) => (c.author.to_string(), c.created, Some(c.text)),
        CommentView::Tombstone {
            author, created, ..
        } => (author.to_string(), created, None),
    };
    doc.push_str(&format!("| comment | {id} |\n"));
    doc.push_str(&format!("| author | {author} |\n"));
    doc.push_str(&format!("| written | {created} |\n"));
    match text {
        Some(text) => {
            doc.push('\n');
            doc.push_str(&text);
            if !text.ends_with('\n') {
                doc.push('\n');
            }
        }
        None => {
            doc.push_str("| state | withdrawn |\n");
            doc.push_str(
                "\n_Withdrawn: the text is not shown. The entry stays because a comment id is never reused._\n",
            );
        }
    }
    Ok(doc)
}

/// One asset as a document: its metadata, then its content. Binary content gets
/// a stub naming the command that writes the bytes out, because raw bytes in a
/// terminal scramble the screen.
fn asset_document(store: &Store, container: &Container, name: &str) -> Result<String> {
    let bytes = ops::read_asset(store, &container.target(), name)?;
    let description = ops::list_assets(store, &container.target())?
        .into_iter()
        .find(|a| a.name == name)
        .and_then(|a| a.description);
    let text = as_text(&bytes);

    let mut doc = String::from("| field | value |\n|---|---|\n");
    doc.push_str(&format!("| asset | {name} |\n"));
    doc.push_str(&format!("| size | {} |\n", human_size(bytes.len() as u64)));
    doc.push_str(&format!(
        "| type | {} |\n",
        if text.is_some() { "text" } else { "binary" }
    ));
    if let Some(description) = description {
        doc.push_str(&format!("| description | {description} |\n"));
    }

    doc.push_str("\n## Content\n\n");
    match text {
        Some(text) => {
            doc.push_str(&text);
            if !text.ends_with('\n') {
                doc.push('\n');
            }
        }
        None => {
            doc.push_str(&format!(
                "_Not text, so it is not shown. Write the bytes out with_\n\n```\nloti {} asset show {} {name}\n```\n",
                container.cli_noun(),
                container.selection().reference()
            ));
        }
    }
    Ok(doc)
}

/// Test support for the whole crate: the one throwaway store its tests read,
/// and the row builders for tests that need rows without a store.
///
/// It lives inside the core seam because building a store means calling
/// `loti_core`, and the rule that this module is the only one naming `loti_core`
/// holds for test code too — a fixture module of its own would break it.
#[cfg(test)]
pub(crate) mod fixture {
    use loti_core::meta::{self, Meta};
    use loti_core::ops::{self, NewEpic, NewNode, Target};
    use loti_core::read;
    use loti_core::Actor;

    use super::*;

    /// Record the format version that leaves `store` in the read-only state
    /// `state` names, returning whether this binary's own version can express it
    /// — there is no major below the lowest one, so an unmigrated store is out
    /// of reach for a binary at the first major.
    ///
    /// Written to the store's metadata rather than reached by running a
    /// migration, because the states a surface has to show are the ones a store
    /// is *left* in: a migration that completes leaves nothing to see.
    pub(crate) fn turn_read_only(store: &Store, state: ReadOnly) -> bool {
        let (major, minor) = loti_core::FORMAT_VERSION;
        let recorded = match state {
            ReadOnly::NeedsMigration => match major.checked_sub(1) {
                Some(older) => Meta::clean(older, minor),
                None => return false,
            },
            ReadOnly::MigrationInProgress => Meta::migrating(major, minor),
            ReadOnly::NeedsNewerLoti => Meta::clean(major + 1, minor),
            ReadOnly::VersionUnreadable => Meta {
                format_version: "not-a-version".to_string(),
            },
        };
        meta::write(store.root(), &recorded).unwrap();
        true
    }

    /// Record this binary's own format version, as a migration that commits
    /// leaves it: read-only is a state a store leaves as well as enters.
    pub(crate) fn turn_writable(store: &Store) {
        meta::write(store.root(), &Meta::current()).unwrap();
    }

    /// A store built for one test, with the references into it that a test needs
    /// to name a target.
    ///
    /// The layout is the smallest one that reaches every level the browser has —
    /// an epic, a ticket under it, a subticket under that — plus a sibling
    /// ticket, which exists because a blocker has to be an entity of its own: no
    /// node may block itself, and a subticket blocking its own parent is not a
    /// shape any workflow produces.
    ///
    /// Meta sits on the epic and on `node`: labels, one comment, one asset, and
    /// `blocked-by` on the node. `subnode` deliberately carries none, so a test
    /// that needs an entity with every collection empty has one.
    pub(crate) struct Fixture {
        /// The store lives under this directory; dropping it removes the store,
        /// so a fixture has to outlive every read of it.
        _dir: tempfile::TempDir,
        /// The handle on the store, cloned when a surface has to own one.
        pub(crate) store: Store,
        /// The epic's id.
        pub(crate) epic: String,
        /// The top-level ticket, which has one subticket and carries meta.
        pub(crate) node: NodeRef,
        /// The subticket under `node`, a leaf with no meta.
        pub(crate) subnode: NodeRef,
        /// The sibling ticket `node` is blocked by; a leaf with no meta.
        pub(crate) blocker: NodeRef,
    }

    impl Fixture {
        /// Build the store. Every entity is created through the operation layer
        /// rather than written as files, so a fixture can never hold a shape the
        /// real write path would refuse.
        pub(crate) fn build() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join(".loti");
            loti_core::store::init(dir.path(), &root).unwrap();
            let store = Store::at(&root);
            let epic = "feature".to_string();

            ops::create_epic(
                &store,
                NewEpic {
                    epic_id: epic.clone(),
                    name: "A feature".into(),
                    summary: "Epic scope".into(),
                    // More than one line, and one line longer than a pane is
                    // wide: a buffer that collapsed a body's breaks, or that
                    // showed only what fits across, is told from a correct one
                    // by a body that has both.
                    body: "epic body\n\nA second paragraph, long enough that no \
                           pane of any test terminal here holds it across.\n"
                        .into(),
                    labels: vec![],
                },
            )
            .unwrap();

            let node = new_node(&store, &epic, None, "Parent", "node body\n");
            let subnode = new_node(&store, &epic, Some(&node), "Child", "");
            let blocker = new_node(&store, &epic, None, "Prerequisite", "");

            add_meta(&store, Target::Epic(epic.clone()));
            add_meta(&store, Target::Node(node.clone()));
            ops::add_blocked_by(&store, &node, std::slice::from_ref(&blocker)).unwrap();

            Self {
                _dir: dir,
                store,
                epic,
                node,
                subnode,
                blocker,
            }
        }

        /// A byte-level view of every document and indexed asset in the store.
        ///
        /// The roster is rediscovered on each call, so the snapshot distinguishes a
        /// write to an existing entity from one that creates a new entity. Asset
        /// bytes travel beside their index because replacing bytes is a tracker
        /// change even when an asset keeps its name and description.
        pub(crate) fn tracker_state(&self) -> Vec<(String, Vec<u8>)> {
            let ids: Vec<String> = read::list_epics(&self.store)
                .expect("the fixture roster can be read")
                .into_iter()
                .map(|entry| match entry {
                    read::RosterEntry::Readable(epic) => epic.id,
                    read::RosterEntry::Unreadable { id, .. } => {
                        panic!("fixture epic {id} became unreadable")
                    }
                })
                .collect();
            let mut state = Vec::new();

            for id in ids {
                let epic = ops::read_epic(&self.store, &id).expect("the fixture epic can be read");
                state.push((
                    format!("epic {id}"),
                    epic.to_text()
                        .expect("the fixture epic can be encoded")
                        .into_bytes(),
                ));
                for asset in epic.frontmatter.assets {
                    state.push((
                        format!("epic {id} asset {}", asset.name),
                        ops::read_asset(&self.store, &Target::Epic(id.clone()), &asset.name)
                            .expect("the fixture epic asset can be read"),
                    ));
                }

                for node in
                    ops::load_epic_nodes(&self.store, &id).expect("the fixture nodes can be read")
                {
                    let number = node.frontmatter.number;
                    let target = Target::Node(NodeRef::new(id.clone(), number));
                    state.push((
                        format!("node {id}/{number}"),
                        node.to_text()
                            .expect("the fixture node can be encoded")
                            .into_bytes(),
                    ));
                    for asset in node.frontmatter.assets {
                        state.push((
                            format!("node {id}/{number} asset {}", asset.name),
                            ops::read_asset(&self.store, &target, &asset.name)
                                .expect("the fixture node asset can be read"),
                        ));
                    }
                }
            }

            state.sort_by(|left, right| left.0.cmp(&right.0));
            state
        }

        /// The epic as a selection, which is how a surface addresses it.
        pub(crate) fn epic_selection(&self) -> Selection {
            Selection::Epic(self.epic.clone())
        }

        /// The ticket as a selection, which is how a surface addresses it.
        pub(crate) fn node_selection(&self) -> Selection {
            Selection::Node(self.node.clone())
        }

        /// The subticket as a selection, which is how a surface addresses it.
        pub(crate) fn subnode_selection(&self) -> Selection {
            Selection::Node(self.subnode.clone())
        }

        /// The state a node is in and the reasons it holds, as the store holds them.
        ///
        /// A test asserts against these rather than against what it asked for, so a
        /// write aimed at the wrong node, or one that dropped the reason on the way,
        /// cannot look right.
        pub(crate) fn node_state(
            &self,
            node: &NodeRef,
        ) -> (String, Option<String>, Option<String>) {
            let node = ops::read_node(&self.store, node).expect("the node can be read");
            (
                node.frontmatter.status.wire_name().to_string(),
                node.frontmatter.block_reason,
                node.frontmatter.close_reason,
            )
        }

        /// Whether the epic carries its closed flag, and the reason it holds with it.
        pub(crate) fn epic_closed(&self) -> (bool, Option<String>) {
            let epic = ops::read_epic(&self.store, &self.epic).expect("the epic can be read");
            (epic.frontmatter.closed, epic.frontmatter.close_reason)
        }

        /// How many of a node's descendants are still open, as the store counts
        /// them: what a cascade has left to close.
        pub(crate) fn open_descendants(&self, node: &NodeRef) -> usize {
            ops::descendants_of(&self.store, node)
                .expect("the subtree can be read")
                .iter()
                .filter(|d| !d.state.is_terminal())
                .count()
        }

        /// One replaceable field of an entity as the store holds it, read through
        /// the seam an editing surface opens on — so a test about a surface asserts
        /// against the store rather than against a constant that a richer fixture
        /// would leave behind.
        pub(crate) fn field(&self, selection: &Selection, field: FreeForm) -> String {
            field
                .of(&edit_target(&self.store, selection).expect("the entity can be read"))
                .to_string()
        }

        /// One replaceable field of the epic; see [`Fixture::field`].
        pub(crate) fn epic_field(&self, field: FreeForm) -> String {
            self.field(&self.epic_selection(), field)
        }

        /// One replaceable field of the ticket, which is addressed differently from
        /// the epic: a write aimed at the wrong one of the two looks right as long
        /// as only epics are asserted on.
        pub(crate) fn node_field(&self, field: FreeForm) -> String {
            self.field(&self.node_selection(), field)
        }

        /// The epic's body as the store holds it; see [`Fixture::field`].
        pub(crate) fn epic_body(&self) -> String {
            self.epic_field(FreeForm::Body)
        }

        /// Replace the epic's body behind the browser's back, as a concurrent
        /// writer composing their own would.
        ///
        /// Through the operation layer, so it moves the `updated` stamp exactly as
        /// any other write does — which is what a precondition is compared against.
        pub(crate) fn rewrite_the_epics_body(&self, body: &str) {
            ops::edit_epic(
                &self.store,
                &self.epic,
                ops::EpicEdits {
                    body: Some(body.to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        }

        /// The labels the epic carries, as the store holds them. A test asserts
        /// against these rather than against a fixture constant, so a richer
        /// fixture cannot turn a removal test into a false promise.
        pub(crate) fn epic_labels(&self) -> Vec<String> {
            ops::list_labels(&self.store, &Target::Epic(self.epic.clone())).unwrap()
        }

        /// The names of the assets the epic carries, as the store holds them. A
        /// test asserts against these rather than against a fixture constant, so a
        /// richer fixture cannot turn a deletion test into a false promise.
        pub(crate) fn epic_assets(&self) -> Vec<String> {
            ops::list_assets(&self.store, &Target::Epic(self.epic.clone()))
                .unwrap()
                .into_iter()
                .map(|asset| asset.name)
                .collect()
        }

        /// A further asset on the epic, created on demand, by name.
        ///
        /// On demand rather than in the shared fixture: proving a deletion took the
        /// asset its row named takes two of them, so the count is that test's
        /// business rather than every other test's.
        pub(crate) fn another_asset(&self) -> String {
            let name = "diagram.png".to_string();
            ops::add_asset(
                &self.store,
                &Target::Epic(self.epic.clone()),
                &name,
                None,
                b"\x89PNG\r\n",
            )
            .unwrap();
            name
        }

        /// An asset on the fixture's *node*, created on demand, by name.
        ///
        /// A node and an epic are addressed differently, so a write aimed at the
        /// wrong one of the two looks correct as long as only epics are tested.
        pub(crate) fn a_node_asset(&self) -> String {
            let name = "trace.log".to_string();
            ops::add_asset(
                &self.store,
                &Target::Node(self.node.clone()),
                &name,
                None,
                b"a line\n",
            )
            .unwrap();
            name
        }

        /// The names of the node's assets, as the store lists them.
        pub(crate) fn node_assets(&self) -> Vec<String> {
            ops::list_assets(&self.store, &Target::Node(self.node.clone()))
                .unwrap()
                .into_iter()
                .map(|asset| asset.name)
                .collect()
        }

        /// The epic's comment list as a selection: the row an addition acts on,
        /// since creation acts on the container row the cursor stands on.
        pub(crate) fn comments_selection(&self) -> Selection {
            Selection::Collection(Container::Epic(self.epic.clone()), Collection::Comments)
        }

        /// The ticket's comment list as a selection. A node and an epic are
        /// addressed differently, so a write aimed at the wrong one of the two looks
        /// correct as long as only epics are asserted on.
        pub(crate) fn node_comments_selection(&self) -> Selection {
            Selection::Collection(Container::Node(self.node.clone()), Collection::Comments)
        }

        /// One comment of the epic as a selection, by the number the store gave it.
        pub(crate) fn epic_comment_selection(&self, id: u64) -> Selection {
            Selection::Comment(Container::Epic(self.epic.clone()), id)
        }

        /// The epic's comments as the store holds them, tombstones included, so a
        /// test asserts what was written rather than what it asked for — and can see
        /// that a withdrawn comment is still there under its own number.
        pub(crate) fn epic_comments(&self) -> Vec<loti_core::model::Comment> {
            self.comments(Target::Epic(self.epic.clone()))
        }

        /// The ticket's comments; a node and an epic are addressed differently, so a
        /// write aimed at the wrong one of the two looks correct as long as only
        /// epics are asserted on.
        pub(crate) fn node_comments(&self) -> Vec<loti_core::model::Comment> {
            self.comments(Target::Node(self.node.clone()))
        }

        /// Every comment one target holds, live and withdrawn alike, straight out of
        /// the frontmatter: a listing hides the text of a withdrawn one, and whether
        /// the text is retained is part of what a withdrawal has to be judged on.
        fn comments(&self, target: Target) -> Vec<loti_core::model::Comment> {
            match target {
                Target::Epic(id) => {
                    ops::read_epic(&self.store, &id)
                        .unwrap()
                        .frontmatter
                        .comments
                }
                Target::Node(r) => {
                    ops::read_node(&self.store, &r)
                        .unwrap()
                        .frontmatter
                        .comments
                }
            }
        }

        /// The number of the epic's one comment written by the human, which is the
        /// only comment the browser may rewrite or withdraw.
        ///
        /// Found by author rather than by position, so a fixture that grows another
        /// comment cannot quietly hand a test somebody else's.
        pub(crate) fn the_humans_comment(&self) -> u64 {
            self.epic_comments()
                .into_iter()
                .find(|c| !c.deleted && c.author == Actor::Human)
                .expect("the fixture writes one comment as the human")
                .id
        }

        /// A comment on the epic written by an agent, created on demand, by the
        /// number the store gave it.
        ///
        /// The one thing the shared fixture cannot hold and this slice cannot do
        /// without: what the browser offers on a comment turns on who wrote it, and
        /// with only the human's comment in the store a browser that offered
        /// everything on everyone's would look right.
        pub(crate) fn an_agents_comment(&self) -> u64 {
            ops::add_comment(
                &self.store,
                &Target::Epic(self.epic.clone()),
                Actor::Agent("builder".to_string()),
                "somebody else wrote this\n".to_string(),
            )
            .unwrap()
            .id
        }

        /// A withdrawn comment of the human's on the epic, created on demand, by the
        /// number it keeps.
        ///
        /// Withdrawn through the operation layer as its author, because a tombstone
        /// is a state a comment is left in rather than a shape to write by hand —
        /// and the browser must offer nothing on it even though the human wrote it.
        pub(crate) fn a_withdrawn_comment(&self) -> u64 {
            let target = Target::Epic(self.epic.clone());
            let added = ops::add_comment(
                &self.store,
                &target,
                Actor::Human,
                "withdrawn since\n".to_string(),
            )
            .unwrap();
            ops::delete_comment(&self.store, &target, added.id, Actor::Human).unwrap();
            added.id
        }

        /// The ticket's dependency list as a selection: the row an addition acts
        /// on, since creation acts on the container row the cursor stands on.
        pub(crate) fn blocked_by_selection(&self) -> Selection {
            Selection::Collection(Container::Node(self.node.clone()), Collection::BlockedBy)
        }

        /// The dependency list as the store holds it, in canonical references. A
        /// test asserts against these rather than against a fixture constant, so a
        /// richer fixture cannot turn a removal test into a false promise.
        pub(crate) fn node_blockers(&self) -> Vec<String> {
            ops::list_blocked_by(&self.store, &self.node).unwrap()
        }

        /// Replace the dependency list with text no node reference can parse.
        ///
        /// This bypasses the operation layer deliberately: it creates a damaged
        /// store the normal writer rejects, which is the only store that can prove
        /// the browser shows the entry without inventing a raw removal operation.
        pub(crate) fn replace_blockers_with_unparseable_entry(&self) -> String {
            let entry = "not-a-reference".to_string();
            let mut node = ops::read_node(&self.store, &self.node).unwrap();
            node.frontmatter.blocked_by = vec![entry.clone()];
            self.store
                .write_node(&self.node.epic_id, self.node.number, &node)
                .unwrap();
            entry
        }

        /// Add one malformed dependency beside the valid blocker the fixture
        /// already holds.
        ///
        /// A damaged store can reach the browser outside the normal writer, so
        /// this writes through the store boundary rather than creating a
        /// test-only navigation row.
        pub(crate) fn add_unparseable_blocker_entry(&self) -> String {
            let entry = "not-a-reference".to_string();
            let mut node = ops::read_node(&self.store, &self.node).unwrap();
            node.frontmatter.blocked_by.push(entry.clone());
            self.store
                .write_node(&self.node.epic_id, self.node.number, &node)
                .unwrap();
            entry
        }

        /// The ticket under test in the two forms a reader may type its reference
        /// in; see [`reference_forms`]. It is the one reference no dependency list
        /// of its own may hold, and only the store may say so.
        pub(crate) fn node_reference_forms(&self) -> (String, String) {
            reference_forms(&self.node)
        }

        /// A further ticket of the fixture's epic, created on demand, in the two
        /// forms a reader may type its reference in; see [`reference_forms`].
        ///
        /// On demand rather than in the shared fixture: a test that adds a blocker
        /// needs a node the list does not hold yet, and proving a removal took the
        /// entry it named takes two entries, so the count is the test's business
        /// rather than every other test's.
        pub(crate) fn another_node(&self) -> (String, String) {
            reference_forms(&new_node(&self.store, &self.epic, None, "Another", ""))
        }

        /// A ticket of a *second* epic, created on demand, in the two forms a reader
        /// may type its reference in; see [`reference_forms`].
        ///
        /// This is what makes a whole reference's guarantee testable at all: with
        /// every node in one epic, a resolver that quietly rewrote the epic to the
        /// blocked node's own would land on the right node anyway.
        pub(crate) fn a_node_of_another_epic(&self) -> (String, String) {
            let epic = "elsewhere".to_string();
            if ops::read_epic(&self.store, &epic).is_err() {
                ops::create_epic(
                    &self.store,
                    NewEpic {
                        epic_id: epic.clone(),
                        name: "Another effort".into(),
                        summary: "Somewhere else".into(),
                        body: String::new(),
                        labels: vec![],
                    },
                )
                .unwrap();
            }
            reference_forms(&new_node(&self.store, &epic, None, "Far", ""))
        }

        /// Take the epic's own file away, as a concurrent writer that closed the
        /// whole effort out would.
        ///
        /// The shortest way to make a write the browser has already offered fail
        /// for a reason only the store can judge: every label operation reads the
        /// entity first, so what comes back is the store's existence refusal.
        pub(crate) fn remove_the_epics_file(&self) {
            std::fs::remove_file(self.store.epic_path(&self.epic)).unwrap();
        }

        /// Take an epic asset's bytes away, leaving the index entry that promises
        /// them, by name.
        ///
        /// Reached behind the operation layer on purpose: no write path leaves a
        /// store in this shape, and it is the shape a browser has to survive — an
        /// asset that is indexed and whose payload is not there.
        pub(crate) fn strip_an_assets_bytes(&self, name: &str) {
            std::fs::remove_file(self.store.epic_asset_dir(&self.epic).join(name)).unwrap();
        }

        /// Take the blocking ticket's own file away, leaving the dependency entry
        /// that names it.
        ///
        /// Behind the operation layer for the same reason as a stripped payload:
        /// loti has no operation that removes a ticket, so a dependency on a ticket
        /// that is not in the store only exists in a store something else damaged.
        pub(crate) fn remove_the_blockers_file(&self) {
            std::fs::remove_file(
                self.store
                    .node_path(&self.blocker.epic_id, self.blocker.number),
            )
            .unwrap();
        }

        /// Take a claim on one node, as another writer does, and answer with the
        /// holder it was taken by.
        ///
        /// The holder is the fixture's to spell, not a test's: a test asserts the
        /// row carries the holder the store holds, so a literal in the test would
        /// pass a row that carried some other writer's identifier.
        pub(crate) fn claim(&self, node: &NodeRef) -> String {
            let holder = "agent:builder".to_string();
            ops::take_claim(&self.store, node, &holder).unwrap();
            holder
        }

        /// The claim the ticket carries, as the store holds it: who holds it and
        /// when it was taken.
        ///
        /// Read back through the store rather than remembered by the caller, so a
        /// test about a claim asserts what was recorded and not what was passed.
        pub(crate) fn node_claim(&self) -> Option<loti_core::model::Claim> {
            ops::read_node(&self.store, &self.node)
                .expect("the ticket can be read")
                .frontmatter
                .claim
        }

        /// When the ticket last changed, as the store holds it: the instant a claim
        /// taken on it has to have been stamped with, since it is the store that
        /// stamps both.
        pub(crate) fn node_updated(&self) -> Timestamp {
            ops::read_node(&self.store, &self.node)
                .expect("the ticket can be read")
                .frontmatter
                .updated
        }

        /// Release a node's claim, as the holder does when the work is handed on:
        /// a row must not keep a holder the store no longer records.
        pub(crate) fn release(&self, node: &NodeRef) {
            ops::release_claim(&self.store, node).unwrap();
        }

        /// Take every label off the epic, as a concurrent writer would.
        ///
        /// This is the shortest way to make a level the browser is standing on
        /// vanish under it: a collection with no members is no longer a level.
        /// Withdrawing a comment would not do it — a tombstone stays listed.
        pub(crate) fn strip_the_epics_labels(&self) {
            let target = Target::Epic(self.epic.clone());
            let labels = ops::list_labels(&self.store, &target).unwrap();
            ops::remove_labels(&self.store, &target, &labels).unwrap();
        }
    }

    /// An initialised store with nothing in it, for the browser's one screen with
    /// no selection at all: the roster of an empty store. The directory travels
    /// with the handle, because dropping it removes the store.
    pub(crate) fn empty_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".loti");
        loti_core::store::init(dir.path(), &root).unwrap();
        let store = Store::at(&root);
        (dir, store)
    }

    /// A reference as `(bare number, whole reference)` — the two forms a reader
    /// may write one in, which no test outside this module may spell for itself:
    /// a bare number names a node of the epic in hand and a whole reference names
    /// a node of any epic.
    fn reference_forms(r: &NodeRef) -> (String, String) {
        (r.number.to_string(), r.to_string())
    }

    fn new_node(
        store: &Store,
        epic: &str,
        parent: Option<&NodeRef>,
        name: &str,
        body: &str,
    ) -> NodeRef {
        let node = ops::create_node(
            store,
            NewNode {
                epic_id: epic.to_string(),
                parent: parent.cloned(),
                name: name.to_string(),
                summary: format!("{name} scope"),
                body: body.to_string(),
                labels: vec![],
            },
        )
        .unwrap();
        NodeRef::new(epic, node.frontmatter.number)
    }

    /// The meta an editing surface can land on, on one target.
    ///
    /// The comment is authored by the human because the browser writes as the
    /// human and nobody else: a comment attributed to an agent could not be
    /// edited or deleted through the surface under test.
    fn add_meta(store: &Store, target: Target) {
        ops::add_labels(store, &target, &["ui".to_string(), "perf".to_string()]).unwrap();
        ops::add_comment(store, &target, Actor::Human, "a remark\n".to_string()).unwrap();
        ops::add_asset(store, &target, "sketch.txt", None, b"sketch\n").unwrap();
    }

    /// An epic row, for a test that exercises the navigation model without a
    /// store behind it.
    pub(crate) fn epic_row(id: &str, children: usize) -> Row {
        Row {
            selection: Selection::Epic(id.to_string()),
            kind: RowKind::Work {
                status: "open".to_string(),
                claimed_by: None,
            },
            label: id.to_string(),
            name: format!("the {id} epic"),
            children,
        }
    }

    /// A node row; see [`epic_row`].
    pub(crate) fn node_row(epic: &str, number: u64, children: usize) -> Row {
        Row {
            selection: Selection::Node(NodeRef::new(epic, number)),
            kind: RowKind::Work {
                status: "to-do".to_string(),
                claimed_by: None,
            },
            label: number.to_string(),
            name: format!("ticket {number}"),
            children,
        }
    }

    /// A node as a container of meta, for a test that must not name a node
    /// reference itself — which is every test outside this module.
    pub(crate) fn node_container(epic: &str, number: u64) -> Container {
        Container::Node(NodeRef::new(epic, number))
    }

    /// A collection row with the given number of members; see [`epic_row`].
    pub(crate) fn collection_row(container: Container, kind: Collection, members: usize) -> Row {
        Row {
            selection: Selection::Collection(container, kind),
            kind: RowKind::Collection(kind),
            label: String::new(),
            name: kind.name().to_string(),
            children: members,
        }
    }

    /// A label row — a collection member, and so a leaf; see [`epic_row`].
    pub(crate) fn label_row(container: Container, label: &str) -> Row {
        Row {
            selection: Selection::Label(container, label.to_string()),
            kind: RowKind::Member,
            label: label.to_string(),
            name: String::new(),
            children: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::Fixture;
    use super::*;
    use loti_core::lock::{self, LockConfig};
    use loti_core::ops::{EpicEdits, NewEpic, NewNode, NodeEdits};
    use loti_core::store::StoreError;
    use std::time::Duration;

    /// The words a refusal is shown in.
    ///
    /// A conflict has none: it is the one refusal the reader is asked about rather
    /// than told, in the browser's own words, so a test reading a refusal's words
    /// must not silently accept one in its place.
    fn refusal_words(refused: Refusal) -> String {
        match refused {
            Refusal::Rule(words) | Refusal::Partial(words) => words,
            Refusal::Conflict => panic!("refused as a conflict rather than by a rule"),
        }
    }

    /// The work rows of a level, dropping the collection rows every epic and node
    /// level leads with.
    fn work_rows(rows: &[Row]) -> Vec<Row> {
        rows.iter()
            .filter(|r| matches!(r.kind, RowKind::Work { .. }))
            .cloned()
            .collect()
    }

    /// The collection rows of a level, as `(name, member count)`.
    fn collection_cells(rows: &[Row]) -> Vec<(String, usize)> {
        rows.iter()
            .filter(|r| matches!(r.kind, RowKind::Collection(_)))
            .map(|r| (r.name.clone(), r.children))
            .collect()
    }

    /// [`Selection::document`] is the seam's one source of truth for "same
    /// document": a collection row and every label row of one container
    /// normalise to the container's own selection, two different containers
    /// never collapse into one, and every other selection stays itself. Pure
    /// data, so no store is needed to prove it.
    #[test]
    fn document_normalises_collections_and_labels_to_their_container() {
        let node = NodeRef::parse("feature/1").unwrap();
        let other = NodeRef::parse("feature/2").unwrap();
        let container = Container::Node(node);
        let other_container = Container::Node(other.clone());

        // Every collection of one container normalises to that container's own
        // document.
        for kind in container.collections() {
            assert_eq!(
                Selection::Collection(container.clone(), *kind).document(),
                container.selection()
            );
        }
        // So does every label, whatever text it carries: a cursor crossing label
        // rows must never see the document change.
        for label in ["a", "b"] {
            assert_eq!(
                Selection::Label(container.clone(), label.to_string()).document(),
                container.selection()
            );
        }
        // A collection row and a label row of the same container agree with each
        // other, and with the container's own selection.
        assert_eq!(
            Selection::Collection(container.clone(), Collection::Labels).document(),
            Selection::Label(container.clone(), "a".to_string()).document()
        );
        assert_eq!(container.selection().document(), container.selection());

        // A different container is a different document — collection and label
        // rows included — so normalisation must never collapse distinct entities
        // together.
        assert_ne!(
            Selection::Collection(container.clone(), Collection::Labels).document(),
            Selection::Collection(other_container.clone(), Collection::Labels).document()
        );
        assert_ne!(
            Selection::Label(container.clone(), "a".to_string()).document(),
            Selection::Label(other_container, "a".to_string()).document()
        );

        // Every other selection already names the document it shows, so it comes
        // back unchanged rather than normalised into something else.
        let epic = Selection::Epic("feature".to_string());
        assert_eq!(epic.document(), epic);
        let comment = Selection::Comment(container.clone(), 1);
        assert_eq!(comment.document(), comment);
        let asset = Selection::Asset(container.clone(), "a.png".to_string());
        assert_eq!(asset.document(), asset);
        let blocker = Selection::Blocker(container, other);
        assert_eq!(blocker.document(), blocker);
    }

    /// The fixture is a contract: every test module in the crate reads the shape
    /// asserted here, so a drift in it has to fail once, here, rather than as an
    /// unrelated assertion elsewhere.
    #[test]
    fn the_fixture_holds_an_epic_a_ticket_a_subticket_and_meta() {
        let fx = Fixture::build();

        let epics = rows(&fx.store, &Level::Epics).unwrap();
        assert_eq!(
            epics
                .iter()
                .map(|r| r.selection.clone())
                .collect::<Vec<_>>(),
            vec![fx.epic_selection()]
        );

        // The epic's own level lists its top-level tickets: the one under test
        // and the one that blocks it.
        let tickets = work_rows(&rows(&fx.store, &Level::Epic(fx.epic.clone())).unwrap());
        assert_eq!(
            tickets
                .iter()
                .map(|r| r.selection.clone())
                .collect::<Vec<_>>(),
            vec![
                Selection::Node(fx.node.clone()),
                Selection::Node(fx.blocker.clone())
            ]
        );
        // Only one of the two has a subticket, which is what lets a test exercise
        // both a ticket with a level of work under it and one without.
        assert_eq!((tickets[0].children, tickets[1].children), (1, 0));

        let subtickets = work_rows(&rows(&fx.store, &Level::Node(fx.node.clone())).unwrap());
        assert_eq!(
            subtickets
                .iter()
                .map(|r| r.selection.clone())
                .collect::<Vec<_>>(),
            vec![Selection::Node(fx.subnode.clone())]
        );

        let node = fx
            .store
            .read_node(&fx.node.epic_id, fx.node.number)
            .unwrap();
        let epic = fx.store.read_epic(&fx.epic).unwrap();
        for meta in [&epic.frontmatter.labels, &node.frontmatter.labels] {
            assert!(meta.len() > 1, "a removal test needs more than one label");
        }
        assert_eq!(epic.frontmatter.comments.len(), 1);
        assert_eq!(epic.frontmatter.assets.len(), 1);
        assert_eq!(node.frontmatter.comments.len(), 1);
        assert_eq!(node.frontmatter.assets.len(), 1);
        assert_eq!(node.frontmatter.blocked_by, vec![fx.blocker.to_string()]);

        // The subticket carries no meta at all, so a test that needs empty
        // collections has an entity with them.
        let subnode = fx
            .store
            .read_node(&fx.subnode.epic_id, fx.subnode.number)
            .unwrap();
        assert!(subnode.frontmatter.labels.is_empty());
        assert!(subnode.frontmatter.comments.is_empty());
        assert!(subnode.frontmatter.assets.is_empty());
        assert!(subnode.frontmatter.blocked_by.is_empty());
    }

    /// Launch preparation belongs at the store seam because a rendered working
    /// directory and the `project_root` template derive from the store, not from
    /// the browser state machine's resource directory.
    #[test]
    fn launch_preparation_derives_project_root_from_the_store() {
        let fixture = Fixture::build();
        let resources = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(resources.path().join("workflows")).unwrap();
        std::fs::create_dir_all(resources.path().join("agents")).unwrap();
        std::fs::write(
            resources.path().join(".loti.conf"),
            "workflow-root = \"workflows\"\nagent-root = \"agents\"\n",
        )
        .unwrap();
        std::fs::write(
            resources.path().join("workflows").join("review.md"),
            "# Review\n",
        )
        .unwrap();
        std::fs::write(
            resources.path().join("agents").join("agent.toml"),
            "command = \"agent\"\nargs = [\"{{ loti_prompt }}\"]\ncwd = \"{{ project_root }}\"\n",
        )
        .unwrap();

        let picker =
            agent_picker(&fixture.store, &fixture.epic_selection(), resources.path()).unwrap();
        let plan = prepare_agent_launch(
            &fixture.store,
            resources.path(),
            &picker.target,
            &picker.workflows[0].id,
            &picker.profiles[0].id,
            Default::default(),
        )
        .unwrap();

        assert_eq!(plan.cwd, fixture.store.root());
    }

    #[test]
    fn children_listing_counts_become_work_row_counts() {
        let rows = child_rows(vec![
            render::ChildRow {
                reference: "feature/7".to_string(),
                name: "branch".to_string(),
                status: "to-do".to_string(),
                claimed_by: None,
                children: 3,
            },
            render::ChildRow {
                reference: "feature/8".to_string(),
                name: "leaf".to_string(),
                status: "to-do".to_string(),
                claimed_by: None,
                children: 0,
            },
        ])
        .unwrap();

        assert_eq!(
            rows.iter()
                .map(|row| (row.label.as_str(), row.name.as_str(), row.children))
                .collect::<Vec<_>>(),
            [("7", "branch", 3), ("8", "leaf", 0)]
        );
    }

    #[test]
    fn every_epic_and_node_level_leads_with_its_collections() {
        let fx = Fixture::build();
        let store = &fx.store;

        // An epic has no dependency list: it is not a unit of work that can be
        // blocked. Counts are the store's, so this cannot drift from the fixture.
        let epic = store.read_epic(&fx.epic).unwrap();
        let level = rows(store, &Level::Epic(fx.epic.clone())).unwrap();
        assert_eq!(
            collection_cells(&level),
            vec![
                ("labels".to_string(), epic.frontmatter.labels.len()),
                ("comments".to_string(), epic.frontmatter.comments.len()),
                ("assets".to_string(), epic.frontmatter.assets.len()),
            ]
        );
        // The collections come first, so the work rows are the tail.
        assert!(level[..3]
            .iter()
            .all(|r| !matches!(r.kind, RowKind::Work { .. })));

        let node = store
            .read_node(&fx.node.epic_id, fx.node.number)
            .unwrap()
            .frontmatter;
        assert_eq!(
            collection_cells(&rows(store, &Level::Node(fx.node.clone())).unwrap()),
            vec![
                ("labels".to_string(), node.labels.len()),
                ("comments".to_string(), node.comments.len()),
                ("blocked-by".to_string(), node.blocked_by.len()),
                ("assets".to_string(), node.assets.len()),
            ]
        );
    }

    #[test]
    fn a_node_with_no_meta_and_no_subtickets_still_has_a_level() {
        let fx = Fixture::build();
        let level = rows(&fx.store, &Level::Node(fx.subnode.clone())).unwrap();

        // Every collection is a row whatever it holds, so the level is never
        // empty and there is always somewhere to stand.
        assert_eq!(collection_cells(&level).len(), 4);
        assert!(work_rows(&level).is_empty());
        assert!(level.iter().all(|r| r.children == 0));
        // An empty collection prints no count, and a row with no count is not
        // enterable — there is nothing below it to show.
        assert!(level.iter().all(|r| !r.enterable()));
    }

    #[test]
    fn an_epic_or_a_node_is_enterable_whether_or_not_it_has_children() {
        let fx = Fixture::build();
        for level in [Level::Epics, Level::Epic(fx.epic.clone())] {
            for row in work_rows(&rows(&fx.store, &level).unwrap()) {
                assert!(
                    row.enterable(),
                    "{:?} must be enterable: its collections are rows there",
                    row.selection
                );
            }
        }
    }

    #[test]
    fn a_collection_lists_its_members_and_every_member_is_a_leaf() {
        let fx = Fixture::build();
        let store = &fx.store;
        let container = Container::Node(fx.node.clone());

        // A tombstone is the one member a count could disagree with the level
        // about, so the promise is only worth testing with one present.
        let existing = match &ops::list_comments(store, &container.target(), true).unwrap()[0] {
            ops::CommentView::Live(c) => c.id,
            ops::CommentView::Tombstone { id, .. } => *id,
        };
        ops::delete_comment(
            store,
            &container.target(),
            existing,
            loti_core::Actor::Human,
        )
        .unwrap();

        let parent_rows = rows(store, &Level::Node(fx.node.clone())).unwrap();
        for kind in container.collections() {
            let level = Level::Collection(container.clone(), *kind);
            let members = rows(store, &level).unwrap();
            let collection_row = parent_rows
                .iter()
                .find(|row| row.selection == Selection::Collection(container.clone(), *kind))
                .expect("the container lists every collection");
            // The count on the collection row is exactly what entering it shows.
            assert_eq!(members.len(), collection_row.children);
            assert!(
                !members.is_empty(),
                "the fixture populates every collection"
            );
            for row in &members {
                assert!(!row.enterable(), "{:?} must be a leaf", row.selection);
                assert_eq!(row.children, 0);
            }
        }
    }

    #[test]
    fn a_collection_member_is_listed_in_stored_order() {
        let fx = Fixture::build();
        let container = Container::Node(fx.node.clone());
        let stored = fx
            .store
            .read_node(&fx.node.epic_id, fx.node.number)
            .unwrap()
            .frontmatter
            .labels;
        let listed: Vec<String> =
            rows(&fx.store, &Level::Collection(container, Collection::Labels))
                .unwrap()
                .iter()
                .map(|r| r.label.clone())
                .collect();
        // No sorting is re-decided here: a level on screen and the same level
        // printed by `list` have to agree.
        assert_eq!(listed, stored);
    }

    #[test]
    fn a_blocker_row_reads_as_work_and_carries_its_whole_reference() {
        let fx = Fixture::build();
        let blockers = rows(
            &fx.store,
            &Level::Collection(Container::Node(fx.node.clone()), Collection::BlockedBy),
        )
        .unwrap();

        let stored = fx
            .store
            .read_node(&fx.blocker.epic_id, fx.blocker.number)
            .unwrap()
            .frontmatter;
        assert_eq!(
            blockers,
            vec![Row {
                // The entry belongs to the blocked node and the document to the
                // blocking one, so both travel with the row.
                selection: Selection::Blocker(Container::Node(fx.node.clone()), fx.blocker.clone()),
                kind: RowKind::Work {
                    status: stored.status.wire_name().to_string(),
                    claimed_by: stored.claim.map(|claim| claim.by),
                },
                // A blocker may live in another epic, so a bare number would not
                // address it.
                label: fx.blocker.to_string(),
                name: stored.name,
                children: 0,
            }]
        );
    }

    /// Who the work rows of a level report as their claim holders, in row order.
    ///
    /// Only a work row can carry a claim, so a level's holders are read off the
    /// work rows alone rather than defaulted to absent for every other kind.
    fn holders(rows: &[Row]) -> Vec<Option<String>> {
        rows.iter()
            .filter_map(|r| match &r.kind {
                RowKind::Work { claimed_by, .. } => Some(claimed_by.clone()),
                RowKind::Collection(_)
                | RowKind::Member
                | RowKind::Comment { .. }
                | RowKind::Withdrawn
                | RowKind::Unreadable => None,
            })
            .collect()
    }

    #[test]
    fn a_blocker_row_carries_the_holder_from_the_read_it_already_does() {
        let fx = Fixture::build();
        let holder = fx.claim(&fx.blocker);
        let level = Level::Collection(Container::Node(fx.node.clone()), Collection::BlockedBy);

        let claimed = rows(&fx.store, &level).unwrap();
        let stored = fx
            .store
            .read_node(&fx.blocker.epic_id, fx.blocker.number)
            .unwrap()
            .frontmatter;
        // The status, the name and the holder are three fields of one read, so
        // all three are asserted together: a holder that arrived by displacing
        // one of the other two is not the row the reader is promised.
        assert_eq!(
            claimed[0].kind,
            RowKind::Work {
                status: stored.status.wire_name().to_string(),
                claimed_by: Some(holder),
            }
        );
        assert_eq!(claimed[0].name, stored.name);

        // A row is built from the store every time the level is listed, so a
        // released claim cannot leave a holder behind on it.
        fx.release(&fx.blocker);
        assert_eq!(holders(&rows(&fx.store, &level).unwrap()), vec![None]);
    }

    #[test]
    fn a_navigation_row_carries_the_holder_of_the_node_it_points_at() {
        let fx = Fixture::build();
        let holder = fx.claim(&fx.node);

        // The epic's level holds both tickets, so one listing shows a claimed row
        // and an unclaimed one: a level of one row cannot tell a holder taken
        // from the right node from one taken from any node.
        let tickets = work_rows(&rows(&fx.store, &Level::Epic(fx.epic.clone())).unwrap());
        assert_eq!(
            tickets
                .iter()
                .map(|r| r.selection.clone())
                .collect::<Vec<_>>(),
            vec![
                Selection::Node(fx.node.clone()),
                Selection::Node(fx.blocker.clone())
            ]
        );
        assert_eq!(holders(&tickets), vec![Some(holder), None]);
    }

    #[test]
    fn the_roster_carries_no_holder_however_its_epics_nodes_are_claimed() {
        let fx = Fixture::build();
        // Claim every node of the epic: a claim is taken on a unit of work, and an
        // epic is not one, so no roster row may report a holder whatever is held
        // beneath it.
        for node in [&fx.node, &fx.subnode, &fx.blocker] {
            fx.claim(node);
        }

        assert_eq!(
            holders(&rows(&fx.store, &Level::Epics).unwrap()),
            vec![None]
        );
    }

    #[test]
    fn a_withdrawn_comment_is_listed_and_says_so_in_a_word() {
        let fx = Fixture::build();
        let store = &fx.store;
        let target = ops::Target::Node(fx.node.clone());
        let live = ops::add_comment(store, &target, loti_core::Actor::Human, "gone\n".into())
            .unwrap()
            .id;
        ops::delete_comment(store, &target, live, loti_core::Actor::Human).unwrap();

        let level = Level::Collection(Container::Node(fx.node.clone()), Collection::Comments);
        let listed = rows(store, &level).unwrap();
        let withdrawn = listed
            .iter()
            .find(|r| r.selection == Selection::Comment(Container::Node(fx.node.clone()), live))
            .expect("a tombstone keeps its id and stays listed");

        assert_eq!(withdrawn.kind, RowKind::Withdrawn);
        // Dim alone would say nothing with colour disabled.
        assert!(withdrawn.name.contains("deleted"), "{:?}", withdrawn.name);
        // The id is never renumbered for display, and the live comment is still
        // there beside it.
        assert_eq!(withdrawn.label, live.to_string());
        // Derived from the store, not a fixture constant: a richer fixture must
        // not turn this into a false promise.
        assert_eq!(
            listed.len(),
            ops::list_comments(store, &target, true).unwrap().len()
        );
    }

    #[test]
    fn a_comment_row_says_whether_the_human_wrote_it() {
        let fx = Fixture::build();
        let agents = fx.an_agents_comment();
        let withdrawn = fx.a_withdrawn_comment();
        let container = Container::Epic(fx.epic.clone());
        let listed = rows(
            &fx.store,
            &Level::Collection(container.clone(), Collection::Comments),
        )
        .unwrap();
        let kind = |id: u64| {
            listed
                .iter()
                .find(|r| r.selection == Selection::Comment(container.clone(), id))
                .unwrap_or_else(|| panic!("comment {id} is listed"))
                .kind
                .clone()
        };

        // The author is on the row because what may be done to a comment turns on
        // it: the human's own is the only one the browser may rewrite or withdraw.
        // Three rows, because a row kind that answered the same for everyone would
        // look right against any one of them.
        let humans = fx.the_humans_comment();
        assert_ne!(humans, withdrawn, "the fixture's own comment was withdrawn");
        assert_eq!(kind(humans), RowKind::Comment { by_the_human: true });
        assert_eq!(
            kind(agents),
            RowKind::Comment {
                by_the_human: false
            }
        );
        // A tombstone is neither: there is no text to rewrite and withdrawing twice
        // means nothing, so whose it was does not come into it.
        assert_eq!(kind(withdrawn), RowKind::Withdrawn);
        // And the reader is told the same thing in the row's own words, with colour
        // off: the author leads the name column of every comment row.
        for (id, author) in [
            (humans, Actor::Human),
            (agents, Actor::Agent("builder".into())),
        ] {
            let row = listed
                .iter()
                .find(|r| r.selection == Selection::Comment(container.clone(), id))
                .unwrap();
            assert!(row.name.contains(&author.to_string()), "{:?}", row.name);
        }
    }

    #[test]
    fn an_asset_row_carries_its_name_and_its_size() {
        let fx = Fixture::build();
        let store = &fx.store;
        let container = Container::Epic(fx.epic.clone());
        ops::add_asset(
            store,
            &container.target(),
            "payload.bin",
            None,
            &vec![0u8; 2048],
        )
        .unwrap();

        let listed = rows(
            store,
            &Level::Collection(container.clone(), Collection::Assets),
        )
        .unwrap();
        let row = listed
            .iter()
            .find(|r| r.label == "payload.bin")
            .expect("an asset is listed under its own name");

        // Name and size are the whole row: a description can be long, so it is
        // the preview's business, but a size is what makes a level scannable.
        assert_eq!(row.name, human_size(2048));
        assert!(!row.name.is_empty() && !row.label.is_empty());
    }

    #[test]
    fn an_asset_whose_bytes_are_missing_is_listed_and_its_own_row_says_so() {
        let fx = Fixture::build();
        let container = Container::Epic(fx.epic.clone());
        let broken = fx.another_asset();
        fx.strip_an_assets_bytes(&broken);

        let listed = rows(
            &fx.store,
            &Level::Collection(container.clone(), Collection::Assets),
        )
        .expect("a payload the browser cannot read must not fail the level");
        // Every asset the store lists is a row, the unreadable one included: the
        // level opens on a store that cannot be read in full, which is exactly
        // when a browser is worth having.
        assert_eq!(
            listed.iter().map(|r| r.label.clone()).collect::<Vec<_>>(),
            fx.epic_assets()
        );

        let row = listed
            .iter()
            .find(|r| r.label == broken)
            .expect("the dangling entry keeps its own row");
        assert_eq!(row.kind, RowKind::Unreadable);
        // In words, not in colour alone, and the reason with them: a size is what
        // stands here normally, so "unreadable" has to be readable as the reason
        // this row has none.
        assert!(row.name.starts_with(UNREADABLE), "{:?}", row.name);
        assert!(row.name.contains("bytes"), "{:?}", row.name);
        // And it still points at the asset, so the reader can stand on the
        // dangling entry and take it off.
        assert_eq!(row.selection, Selection::Asset(container, broken.clone()));

        // One unreadable member is one row: the asset beside it reads as it did.
        let intact = listed
            .iter()
            .find(|r| r.label != broken)
            .expect("the fixture's own asset");
        assert_eq!(intact.kind, RowKind::Member);
        assert!(!intact.name.contains(UNREADABLE), "{:?}", intact.name);
    }

    #[test]
    fn a_blocker_naming_a_ticket_that_is_gone_is_listed_and_its_own_row_says_so() {
        let fx = Fixture::build();
        let container = Container::Node(fx.node.clone());
        fx.remove_the_blockers_file();

        let listed = rows(
            &fx.store,
            &Level::Collection(container.clone(), Collection::BlockedBy),
        )
        .expect("a dependency the browser cannot read must not fail the level");
        assert_eq!(
            listed.iter().map(|r| r.label.clone()).collect::<Vec<_>>(),
            fx.node_blockers()
        );

        let row = &listed[0];
        assert_eq!(row.kind, RowKind::Unreadable);
        assert!(row.name.starts_with(UNREADABLE), "{:?}", row.name);
        // The store's own sentence about what is missing, not a browser paraphrase
        // of it: the same rule reaches the reader here as from the command line.
        assert!(
            row.name.contains(
                &ops::read_node(&fx.store, &fx.blocker)
                    .expect_err("the ticket is gone")
                    .to_string()
            ),
            "{:?}",
            row.name
        );
        // A dependency on a ticket that is not there is a thing to remove, which
        // takes a row that still names the entry.
        assert_eq!(
            row.selection,
            Selection::Blocker(container, fx.blocker.clone())
        );
    }

    #[test]
    fn an_unparseable_blocker_is_listed_but_is_not_a_removal_target() {
        let fx = Fixture::build();
        let container = Container::Node(fx.node.clone());
        let entry = fx.replace_blockers_with_unparseable_entry();

        let listed = rows(
            &fx.store,
            &Level::Collection(container.clone(), Collection::BlockedBy),
        )
        .expect("a malformed dependency must not fail its level");
        assert_eq!(
            listed
                .iter()
                .map(|row| row.label.clone())
                .collect::<Vec<_>>(),
            fx.node_blockers(),
            "every stored entry stays visible by its exact text"
        );

        let row = listed
            .iter()
            .find(|row| row.label == entry)
            .expect("the malformed entry has a row");
        assert_eq!(row.kind, RowKind::Unreadable);
        assert_eq!(row.name, "cannot be removed");
        // The raw text is deliberately not a Blocker selection: only that parsed
        // selection reaches the removal write, so this row cannot mutate by text.
        assert_eq!(
            row.selection,
            Selection::UnremovableBlocker(container, entry.clone())
        );
        assert!(!row.enterable(), "a malformed entry is still a leaf");

        let before = fx.node_blockers();
        assert!(
            perform(&fx.store, &Write::RemoveBlocker(row.selection.clone())).is_err(),
            "a malformed entry must not reach a raw removal"
        );
        assert_eq!(
            fx.node_blockers(),
            before,
            "a refused removal changed the list"
        );
    }

    #[test]
    fn a_collection_and_a_label_keep_the_containers_own_document() {
        let fx = Fixture::build();
        let store = &fx.store;
        let container = Container::Node(fx.node.clone());
        // The document the seam produces for the container, never a fixed string:
        // a richer fixture must not break this.
        let own = preview(store, &container.selection()).unwrap();

        for kind in container.collections() {
            let shown = preview(store, &Selection::Collection(container.clone(), *kind)).unwrap();
            assert_eq!(shown, own, "a collection has no document of its own");
        }
        // A label has none either, and the label is readable in that document's
        // own metadata table, so the pane stays useful as the cursor moves.
        let label = rows(
            store,
            &Level::Collection(container.clone(), Collection::Labels),
        )
        .unwrap()[0]
            .selection
            .clone();
        assert_eq!(preview(store, &label).unwrap(), own);
    }

    #[test]
    fn a_blocker_previews_the_node_that_blocks_you() {
        let fx = Fixture::build();
        let store = &fx.store;
        let blocker = rows(
            store,
            &Level::Collection(Container::Node(fx.node.clone()), Collection::BlockedBy),
        )
        .unwrap()[0]
            .selection
            .clone();
        // What blocks you is readable without leaving the level.
        assert_eq!(
            preview(store, &blocker).unwrap(),
            preview(store, &Selection::Node(fx.blocker.clone())).unwrap()
        );
    }

    #[test]
    fn a_comment_previews_its_author_its_full_timestamp_and_its_text() {
        let fx = Fixture::build();
        let store = &fx.store;
        let stored = store
            .read_node(&fx.node.epic_id, fx.node.number)
            .unwrap()
            .frontmatter
            .comments
            .remove(0);

        let doc = preview(
            store,
            &Selection::Comment(Container::Node(fx.node.clone()), stored.id),
        )
        .unwrap();
        assert!(doc.contains(&stored.author.to_string()), "{doc}");
        // The full timestamp, not the row's relative age.
        assert!(doc.contains(&stored.created.to_string()), "{doc}");
        assert!(doc.contains(stored.text.trim()), "{doc}");
    }

    #[test]
    fn a_withdrawn_comment_previews_who_wrote_it_and_that_it_is_withdrawn() {
        let fx = Fixture::build();
        let store = &fx.store;
        let target = Target::Node(fx.node.clone());
        let id = ops::add_comment(store, &target, loti_core::Actor::Human, "secret\n".into())
            .unwrap()
            .id;
        ops::delete_comment(store, &target, id, loti_core::Actor::Human).unwrap();

        let doc = preview(
            store,
            &Selection::Comment(Container::Node(fx.node.clone()), id),
        )
        .unwrap();
        assert!(doc.contains("human"), "{doc}");
        assert!(doc.contains("withdrawn"), "{doc}");
        // A tombstone's text is retained by the store and withheld by the read,
        // so the pane must not show it — and must not claim it was destroyed.
        assert!(!doc.contains("secret"), "{doc}");
        // The pane must not claim erasure in any word: the store still has it.
        for claim in ["gone", "destroyed", "erased", "deleted for good"] {
            assert!(!doc.contains(claim), "{doc}");
        }
    }

    #[test]
    fn an_asset_previews_its_metadata_and_renders_text_content() {
        let fx = Fixture::build();
        let store = &fx.store;
        let target = Target::Epic(fx.epic.clone());
        ops::add_asset(
            store,
            &target,
            "notes.txt",
            Some("the notes".into()),
            b"line one\n",
        )
        .unwrap();

        let doc = preview(
            store,
            &Selection::Asset(Container::Epic(fx.epic.clone()), "notes.txt".into()),
        )
        .unwrap();
        assert!(doc.contains("notes.txt"), "{doc}");
        assert!(doc.contains("9 B"), "{doc}");
        assert!(doc.contains("text"), "{doc}");
        // A description can be long, so the row leaves it to the preview.
        assert!(doc.contains("the notes"), "{doc}");
        assert!(doc.contains("line one"), "{doc}");
    }

    #[test]
    fn a_binary_asset_previews_a_stub_and_never_its_bytes() {
        let fx = Fixture::build();
        let store = &fx.store;
        let target = Target::Epic(fx.epic.clone());
        // Valid UTF-8 and still binary: control bytes decode cleanly and would
        // scramble the terminal if they reached the pane.
        let bytes = [0u8, 1, 2, 3, 0x1b, b'x'];
        ops::add_asset(store, &target, "shot.png", None, &bytes).unwrap();

        let doc = preview(
            store,
            &Selection::Asset(Container::Epic(fx.epic.clone()), "shot.png".into()),
        )
        .unwrap();
        assert!(doc.contains("binary"), "{doc}");
        assert!(
            doc.chars().all(|c| !c.is_control() || c == '\n'),
            "raw bytes reached the preview: {doc:?}"
        );
        // The stub signposts the command that writes the payload out, since the
        // browser cannot.
        assert!(doc.contains("asset show"), "{doc}");
    }

    #[test]
    fn a_size_reads_in_the_units_a_file_is_measured_in() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn an_age_is_rounded_down_and_never_runs_backwards() {
        let now = Timestamp::now();
        let ago = |seconds: i64| age(now - jiff::Span::new().seconds(seconds), now);
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(60), "1m ago");
        // Rounded down, so "3h" is never read as four.
        assert_eq!(ago(4 * 3600 - 1), "3h ago");
        assert_eq!(ago(25 * 3600), "1d ago");
        assert_eq!(ago(8 * 86400), "1w ago");
        // A writer's clock ahead of ours is skew, not a comment from the future.
        assert_eq!(age(now + jiff::Span::new().hours(2), now), "just now");
    }

    #[test]
    fn a_label_removal_takes_the_one_label_it_names_and_refuses_anything_else() {
        let fx = Fixture::build();
        let before = fx.epic_labels();
        assert!(before.len() > 1, "the promise needs more than one label");

        perform(
            &fx.store,
            &Write::RemoveLabel(Selection::Label(
                Container::Epic(fx.epic.clone()),
                before[0].clone(),
            )),
        )
        .unwrap();
        assert_eq!(fx.epic_labels(), before[1..].to_vec());

        // Only a label row offers removal, so a selection that is not a label is a
        // caller that has lost track of what its row points at — refused by name,
        // and with nothing written on the way to refusing. Checked through the
        // seam the browser itself writes through, so a wrongly wired dialog meets
        // the same guard.
        let err = refusal_words(
            perform(&fx.store, &Write::RemoveLabel(fx.epic_selection()))
                .expect_err("an epic is not one of its own labels"),
        );
        assert!(err.contains(&fx.epic), "{err}");
        assert_eq!(fx.epic_labels(), before[1..].to_vec());
    }

    #[test]
    fn a_label_addition_writes_what_was_typed_and_refuses_anything_else() {
        let fx = Fixture::build();
        let before = fx.epic_labels();
        let set = Selection::Collection(Container::Epic(fx.epic.clone()), Collection::Labels);

        // Verbatim, spacing included: whether a string is a label the store will
        // take is the store's rule, so the browser passes the text through rather
        // than tidying it into something the reader did not type.
        perform(
            &fx.store,
            &Write::AddLabel(set.clone(), " a new label ".into()),
        )
        .unwrap();
        let mut expected = before.clone();
        expected.push(" a new label ".to_string());
        assert_eq!(fx.epic_labels(), expected);

        // A label the set already holds is the store's own no-op, not a refusal
        // the browser invents on its behalf.
        perform(&fx.store, &Write::AddLabel(set, before[0].clone())).unwrap();
        assert_eq!(fx.epic_labels(), expected);

        // Only the label set's own row offers an addition, so any other selection
        // is a caller that has lost track of what its row points at — refused by
        // name, through the seam the browser itself writes through, and with
        // nothing written on the way to refusing. A member of the set is refused
        // too, though it names the same container: a label is added from the set's
        // row, and a row pointing at one member is not that row.
        for wrong in [
            fx.epic_selection(),
            Selection::Label(Container::Epic(fx.epic.clone()), expected[0].clone()),
            Selection::Collection(Container::Epic(fx.epic.clone()), Collection::Comments),
        ] {
            let err = refusal_words(
                perform(
                    &fx.store,
                    &Write::AddLabel(wrong.clone(), "unwanted".into()),
                )
                .expect_err("only a label set takes a label"),
            );
            assert!(err.contains(&fx.epic), "{wrong:?}: {err}");
            assert_eq!(fx.epic_labels(), expected, "{wrong:?} wrote something");
        }
    }

    #[test]
    fn a_blocker_is_added_by_a_bare_number_or_by_a_whole_reference() {
        let fx = Fixture::build();
        let before = fx.node_blockers();
        let list = fx.blocked_by_selection();
        let (bare, whole) = fx.another_node();

        // A bare number names a node of the blocked node's own epic, which is the
        // one thing about the text the browser resolves — and the store records the
        // canonical form whichever way it was written.
        assert_eq!(
            perform(&fx.store, &Write::AddBlocker(list.clone(), bare)).unwrap(),
            Effect::AsAsked
        );
        let mut expected = before.clone();
        expected.push(whole.clone());
        assert_eq!(fx.node_blockers(), expected);

        // The same reference written whole is the same entry, so the store's own
        // no-op is what happens — not a refusal the browser invents on its behalf.
        assert_eq!(
            perform(&fx.store, &Write::AddBlocker(list.clone(), whole.clone())).unwrap(),
            Effect::AlreadyListed(whole.clone())
        );
        assert_eq!(fx.node_blockers(), expected);

        // A whole reference reaches a node of ANOTHER epic — which is the only
        // property that distinguishes the two forms, and is unprovable inside one
        // epic: a resolver that rewrote the epic to the blocked node's own would
        // land on the right node anyway. Blanks around it are not part of it
        // either: a reference is a token, not text.
        let (_, elsewhere) = fx.a_node_of_another_epic();
        assert!(
            !elsewhere.starts_with(&fx.epic),
            "the far reference is inside the blocked node's own epic: {elsewhere}"
        );
        perform(
            &fx.store,
            &Write::AddBlocker(list, format!("  {elsewhere}  ")),
        )
        .unwrap();
        expected.push(elsewhere);
        assert_eq!(fx.node_blockers(), expected);

        // And both forms name the blocker the way the store records it, so a
        // notice about a bare number does not read as a ticket of no epic.
        let canonical = format!("{}/1", fx.epic);
        assert_eq!(blocker_name(&fx.blocked_by_selection(), "1"), canonical);
        assert_eq!(blocker_name(&fx.blocked_by_selection(), " 1 "), canonical);
        assert_eq!(
            blocker_name(&fx.blocked_by_selection(), &whole),
            whole,
            "a whole reference is named as it stands"
        );
        // A reference that names no node is named back as it was typed, so what a
        // notice would say is still the reader's own words. Nothing is written for
        // one, so that name is never read — but a name that went missing here would
        // be a notice with a hole in it if it ever were.
        assert_eq!(
            blocker_name(&fx.blocked_by_selection(), " one/two/three "),
            "one/two/three"
        );
    }

    #[test]
    fn a_blocker_the_store_will_not_take_is_refused_in_the_stores_own_words() {
        let fx = Fixture::build();
        let before = fx.node_blockers();
        let list = fx.blocked_by_selection();
        let (own_number, own_reference) = fx.node_reference_forms();

        // Neither a blocker that does not exist nor a node blocking itself is the
        // browser's judgement: the write is attempted and what comes back is the
        // operation's own message, with no context wrapped round it and no rule
        // restated in words of the browser's own. Compared against what the
        // operation itself produces, which is what a reworded refusal would fail.
        for (typed, blocker) in [
            ("999".to_string(), NodeRef::new(&fx.epic, 999)),
            (own_number, NodeRef::new(&fx.epic, fx.node.number)),
            (own_reference, NodeRef::new(&fx.epic, fx.node.number)),
        ] {
            let shown = refusal_words(
                perform(&fx.store, &Write::AddBlocker(list.clone(), typed.clone()))
                    .expect_err("the store judges what may block what"),
            );
            let its_own = ops::add_blocked_by(&fx.store, &fx.node, std::slice::from_ref(&blocker))
                .expect_err("the store judges what may block what")
                .to_string();
            assert_eq!(shown, its_own, "{typed:?}");
            assert_eq!(fx.node_blockers(), before, "{typed:?} wrote something");
        }

        // A reference that is no reference at all is refused by the parser that
        // owns that rule, and reaches the reader in its words too.
        let shown = refusal_words(
            perform(&fx.store, &Write::AddBlocker(list, "one/two/three".into()))
                .expect_err("a reference is <epic-id>/<number>"),
        );
        assert_eq!(
            shown,
            NodeRef::parse("one/two/three").unwrap_err().to_string()
        );
        assert_eq!(fx.node_blockers(), before);
    }

    #[test]
    fn a_blocker_removal_takes_the_one_entry_it_names_and_refuses_anything_else() {
        let fx = Fixture::build();
        let list = fx.blocked_by_selection();
        let (bare, whole) = fx.another_node();
        perform(&fx.store, &Write::AddBlocker(list.clone(), bare)).unwrap();
        let before = fx.node_blockers();
        assert!(before.len() > 1, "the promise needs more than one entry");

        // The row carries both nodes, so the entry it names is the entry that goes
        // and every other one stays.
        perform(
            &fx.store,
            &Write::RemoveBlocker(Selection::Blocker(
                Container::Node(fx.node.clone()),
                NodeRef::parse(&whole).unwrap(),
            )),
        )
        .unwrap();
        let survivors: Vec<String> = before.iter().filter(|b| **b != whole).cloned().collect();
        assert_eq!(fx.node_blockers(), survivors);
        assert!(!survivors.is_empty(), "the whole list went");

        // Only a node's blocker row offers removal, so any other selection is a
        // caller that has lost track of what its row points at — refused by name,
        // through the seam the browser itself writes through, and with nothing
        // written on the way to refusing.
        for wrong in [
            fx.epic_selection(),
            list,
            Selection::Blocker(
                Container::Epic(fx.epic.clone()),
                NodeRef::parse(&whole).unwrap(),
            ),
        ] {
            let err = refusal_words(
                perform(&fx.store, &Write::RemoveBlocker(wrong.clone()))
                    .expect_err("only a blocker row takes a removal"),
            );
            assert!(err.contains(&fx.epic), "{wrong:?}: {err}");
            assert_eq!(fx.node_blockers(), survivors, "{wrong:?} wrote something");
        }
    }

    #[test]
    fn an_asset_deletion_takes_the_one_asset_it_names_and_refuses_anything_else() {
        let fx = Fixture::build();
        let doomed = fx.another_asset();
        let before = fx.epic_assets();
        assert!(before.len() > 1, "the promise needs more than one asset");
        let container = Container::Epic(fx.epic.clone());

        perform(
            &fx.store,
            &Write::DeleteAsset(Selection::Asset(container.clone(), doomed.clone())),
        )
        .unwrap();
        let survivors: Vec<String> = before.iter().filter(|a| **a != doomed).cloned().collect();
        assert_eq!(fx.epic_assets(), survivors);
        assert!(!survivors.is_empty(), "every asset went");
        // Hard, not withheld: the bytes go with the index entry, so the payload is
        // no longer readable at all. A comment keeps a tombstone; an asset does not.
        assert!(
            preview(
                &fx.store,
                &Selection::Asset(container.clone(), doomed.clone())
            )
            .is_err(),
            "the bytes outlived the deletion"
        );

        // A node's assets are addressed differently from an epic's, so a deletion
        // aimed at the wrong one of the two would pass every test above: the epic
        // is the only container they use.
        let on_the_node = fx.a_node_asset();
        assert!(fx.node_assets().contains(&on_the_node));
        let before_the_epics = fx.epic_assets();
        perform(
            &fx.store,
            &Write::DeleteAsset(Selection::Asset(
                Container::Node(fx.node.clone()),
                on_the_node.clone(),
            )),
        )
        .unwrap();
        assert!(!fx.node_assets().contains(&on_the_node));
        assert_eq!(
            fx.epic_assets(),
            before_the_epics,
            "a node's asset was deleted off its epic"
        );

        // Only an asset row offers a deletion, so any other selection is a caller
        // that has lost track of what its row points at — refused by name, through
        // the seam the browser itself writes through, and with nothing written on
        // the way to refusing. The collection's own row is refused too, though it
        // names the same container: a deletion acts on one member, and the row
        // pointing at the whole collection is not that row.
        for wrong in [
            fx.epic_selection(),
            Selection::Collection(container, Collection::Assets),
        ] {
            let err = refusal_words(
                perform(&fx.store, &Write::DeleteAsset(wrong.clone()))
                    .expect_err("only an asset row takes a deletion"),
            );
            assert!(err.contains(&fx.epic), "{wrong:?}: {err}");
            assert_eq!(fx.epic_assets(), survivors, "{wrong:?} wrote something");
        }
    }

    #[test]
    fn an_asset_deletion_the_store_refuses_carries_the_stores_own_message() {
        let fx = Fixture::build();
        let asset = fx.epic_assets()[0].clone();
        let selection = Selection::Asset(Container::Epic(fx.epic.clone()), asset.clone());

        // Whether an asset can be deleted is the store's judgement: the write is
        // attempted and what comes back is the operation's own message, with no
        // context wrapped round it and no rule restated in words of the browser's
        // own — compared against what the operation itself produces, which is what a
        // wrapped or reworded refusal would fail. Here the entity goes out from
        // under the write, as a concurrent writer closing the effort out would take
        // it.
        fx.remove_the_epics_file();
        let shown = refusal_words(
            perform(&fx.store, &Write::DeleteAsset(selection))
                .expect_err("the store refuses a deletion on a missing entity"),
        );
        let its_own = ops::delete_asset(&fx.store, &Target::Epic(fx.epic.clone()), &asset)
            .expect_err("the store refuses a deletion on a missing entity")
            .to_string();
        assert_eq!(shown, its_own);
    }

    #[test]
    fn a_comment_is_added_to_the_list_it_names_authored_by_the_human_and_numbered_by_the_store() {
        let fx = Fixture::build();
        let before = fx.epic_comments();
        let on_the_node = fx.node_comments();

        // Verbatim, trailing newline and all: whether a given string is a comment
        // the store will take is the store's rule, and the browser normalises none
        // of it.
        let written = "a remark\n\nwith a blank line above it\n";
        let effect = perform(
            &fx.store,
            &Write::AddComment(fx.comments_selection(), written.to_string()),
        )
        .unwrap();

        let after = fx.epic_comments();
        let added = after
            .iter()
            .find(|c| !before.iter().any(|had| had.id == c.id))
            .expect("the comment is on the list its row named")
            .clone();
        assert_eq!(added.text, written);
        // The browser writes as the human and only the human: there is no channel
        // here for an agent's attribution, and the store records who wrote it.
        assert_eq!(added.author, Actor::Human);
        // The number is the store's, assigned under the lock from what the list
        // already held, and it comes back so the reader can be told which comment
        // theirs became.
        assert_eq!(effect, Effect::Commented(added.id));
        assert!(!before.iter().any(|had| had.id == added.id));
        // A node's comments and an epic's are different lists, so a write aimed at
        // the wrong container of the two would look right against the epic alone.
        assert_eq!(fx.node_comments(), on_the_node);
        perform(
            &fx.store,
            &Write::AddComment(fx.node_comments_selection(), "on the ticket\n".to_string()),
        )
        .unwrap();
        assert_eq!(fx.node_comments().len(), on_the_node.len() + 1);
        assert!(
            fx.node_comments()
                .iter()
                .any(|c| c.text == "on the ticket\n"),
            "the ticket's own list did not take it"
        );
        assert!(
            !fx.epic_comments()
                .iter()
                .any(|c| c.text == "on the ticket\n"),
            "the ticket's comment landed on the epic"
        );

        // And a row that is not a comment list is a caller that has lost track of
        // what its row points at: refused by name, with nothing written.
        for wrong in [
            fx.epic_selection(),
            fx.blocked_by_selection(),
            fx.epic_comment_selection(added.id),
        ] {
            let err = refusal_words(
                perform(
                    &fx.store,
                    &Write::AddComment(wrong.clone(), "nowhere\n".into()),
                )
                .expect_err("only a comment list takes a comment"),
            );
            assert!(err.contains(&wrong.reference()), "{wrong:?}: {err}");
            assert_eq!(fx.epic_comments().len(), after.len(), "{wrong:?} wrote");
        }
    }

    /// What a level lists, as the selections its rows point at: the same listing
    /// the browser draws, so a test about something created asserts that the row is
    /// there rather than that a file is.
    fn listed(store: &Store, level: &Level) -> Vec<Selection> {
        rows(store, level)
            .unwrap()
            .into_iter()
            .map(|row| row.selection)
            .collect()
    }

    /// The units of work a level lists, dropping the collection rows every epic and
    /// node level leads with.
    fn work_selections(store: &Store, level: &Level) -> Vec<Selection> {
        work_rows(&rows(store, level).unwrap())
            .into_iter()
            .map(|row| row.selection)
            .collect()
    }

    /// The unit of work a creation answered with, by the reference the store gave
    /// it — which is the only name it has, so a test that could not read it back
    /// could not say what was made.
    fn created(effect: Effect) -> Selection {
        match effect {
            Effect::Created(reference) => Selection::Node(
                NodeRef::parse(&reference).expect("the store answers with a reference it recorded"),
            ),
            other => panic!("a creation did not name what it created: {other:?}"),
        }
    }

    #[test]
    fn a_ticket_is_created_in_the_container_its_row_names_and_the_store_names_what_it_made() {
        let fx = Fixture::build();
        let top_level = Level::Epic(fx.epic.clone());
        let under_the_ticket = Level::Node(fx.node.clone());
        let before_top = work_selections(&fx.store, &top_level);
        let before_under = work_selections(&fx.store, &under_the_ticket);

        // An epic's row is the container of its top-level tickets.
        let made = created(
            perform(
                &fx.store,
                &Write::CreateNode {
                    parent: fx.epic_selection(),
                    name: "A new thing".to_string(),
                    summary: "what it is for".to_string(),
                },
            )
            .unwrap(),
        );

        // The reference the write answered with names a ticket that is there, on the
        // level the row it was made from lists, and holds what was filled in.
        let after_top = work_selections(&fx.store, &top_level);
        let arrivals: Vec<&Selection> = after_top
            .iter()
            .filter(|s| !before_top.contains(s))
            .collect();
        assert_eq!(arrivals, vec![&made], "{after_top:?}");
        assert_eq!(fx.field(&made, FreeForm::Name), "A new thing");
        assert_eq!(fx.field(&made, FreeForm::Summary), "what it is for");
        // A creation form asks for neither, so a new unit of work starts with an
        // empty body and no labels: everything else is edited once the row exists.
        assert_eq!(fx.field(&made, FreeForm::Body), "");
        assert_eq!(
            work_selections(&fx.store, &under_the_ticket),
            before_under,
            "a ticket of the epic hung under one of its tickets"
        );

        // And a ticket's row is the container of its subtickets, so the same write
        // aimed at one makes a subticket of it and not another top-level ticket.
        let under = created(
            perform(
                &fx.store,
                &Write::CreateNode {
                    parent: fx.node_selection(),
                    name: "Under it".to_string(),
                    summary: String::new(),
                },
            )
            .unwrap(),
        );
        assert!(
            work_selections(&fx.store, &under_the_ticket).contains(&under),
            "the subticket is not under the ticket its row named"
        );
        assert!(
            !work_selections(&fx.store, &top_level).contains(&under),
            "a subticket was made a top-level ticket of the epic"
        );
        // A summary is a line a reader may leave for later, and an empty one is
        // written as empty rather than refused here: what makes a value acceptable
        // is the store's rule.
        assert_eq!(fx.field(&under, FreeForm::Summary), "");

        // A row that holds no units of work at all is a caller that has lost track
        // of what its row points at: refused by name, with nothing written.
        let unchanged = work_selections(&fx.store, &top_level);
        for wrong in [fx.comments_selection(), fx.blocked_by_selection()] {
            let err = refusal_words(
                perform(
                    &fx.store,
                    &Write::CreateNode {
                        parent: wrong.clone(),
                        name: "nowhere".to_string(),
                        summary: String::new(),
                    },
                )
                .expect_err("only a container of units of work takes a creation"),
            );
            assert!(err.contains(&wrong.reference()), "{wrong:?}: {err}");
            assert_eq!(
                work_selections(&fx.store, &top_level),
                unchanged,
                "{wrong:?} wrote something"
            );
        }
    }

    #[test]
    fn an_epic_is_created_under_the_id_it_was_given_and_a_taken_one_is_the_stores_refusal() {
        let fx = Fixture::build();
        let before = listed(&fx.store, &Level::Epics);
        let made = Selection::Epic("a-second-effort".to_string());
        assert!(!before.contains(&made), "the fixture already holds it");

        let effect = perform(
            &fx.store,
            &Write::CreateEpic {
                epic: made.clone(),
                name: "A second effort".to_string(),
                summary: "somewhere else".to_string(),
            },
        )
        .unwrap();
        // The reader typed the id, so the store knew nothing about the new epic that
        // the browser could not already say: unlike a ticket, whose number the store
        // allocates, this write reports exactly what it was asked for.
        assert_eq!(effect, Effect::AsAsked);

        assert!(
            listed(&fx.store, &Level::Epics).contains(&made),
            "the epic is not on the roster the browser lists"
        );
        assert_eq!(fx.field(&made, FreeForm::Name), "A second effort");
        assert_eq!(fx.field(&made, FreeForm::Summary), "somewhere else");
        assert_eq!(fx.field(&made, FreeForm::Body), "");

        // Whether an id is free is the store's rule and nothing here asks first: a
        // second epic under a taken id comes back refused in the store's own words,
        // compared against what the operation itself produces, which is what a
        // wrapped or reworded refusal would fail.
        let held = fx.epic_field(FreeForm::Name);
        let shown = refusal_words(
            perform(
                &fx.store,
                &Write::CreateEpic {
                    epic: fx.epic_selection(),
                    name: "Again".to_string(),
                    summary: String::new(),
                },
            )
            .expect_err("the store refuses an id it already holds"),
        );
        let its_own = ops::create_epic(
            &fx.store,
            NewEpic {
                epic_id: fx.epic.clone(),
                name: "Again".to_string(),
                summary: String::new(),
                labels: Vec::new(),
                body: String::new(),
            },
        )
        .expect_err("the store refuses an id it already holds")
        .to_string();
        assert_eq!(shown, its_own);
        assert_eq!(
            fx.epic_field(FreeForm::Name),
            held,
            "a refused creation overwrote the epic that holds the id"
        );

        // And an address that is not an epic's is a caller that has lost track of
        // what it is creating: refused by name, with nothing written.
        let roster = listed(&fx.store, &Level::Epics);
        let err = refusal_words(
            perform(
                &fx.store,
                &Write::CreateEpic {
                    epic: fx.node_selection(),
                    name: "nowhere".to_string(),
                    summary: String::new(),
                },
            )
            .expect_err("only an epic id addresses an epic"),
        );
        assert!(err.contains(&fx.node.to_string()), "{err}");
        assert_eq!(listed(&fx.store, &Level::Epics), roster);
    }

    #[test]
    fn a_comments_text_is_replaced_verbatim_under_the_stamp_it_was_read_at() {
        let fx = Fixture::build();
        let id = fx.epic_comments()[0].id;
        let selection = fx.epic_comment_selection(id);
        let read = comment_target(&fx.store, &selection).unwrap();
        let neighbour = fx.an_agents_comment();
        // Read out of the store rather than spelled here, so a richer fixture
        // cannot turn either half of this into a false promise.
        let held_before = fx.epic_comments();
        let text_of = |id: u64, of: &[loti_core::model::Comment]| {
            of.iter()
                .find(|c| c.id == id)
                .unwrap_or_else(|| panic!("comment {id} is held"))
                .text
                .clone()
        };
        let body = fx.epic_body();

        // The stamp is re-read after the neighbour arrived: a comment's guard is its
        // container's stamp, so the read a surface opens on is the read the write
        // names.
        let stamp = comment_target(&fx.store, &selection).unwrap().stamp;
        let written = "rewritten\n\nwith a blank line above it\n";
        perform(
            &fx.store,
            &Write::Replace {
                target: selection.clone(),
                field: Replaceable::CommentText,
                value: written.to_string(),
                expect: Some(stamp),
            },
        )
        .unwrap();

        let held = fx.epic_comments();
        assert_eq!(text_of(id, &held), written);
        assert_eq!(
            read.text,
            text_of(id, &held_before),
            "the buffer opened on text the store was not holding"
        );
        // The comment it named and no other: two comments of the same container are
        // one list, so a write that reached for the wrong entry would look right
        // against a store holding one.
        assert_eq!(
            text_of(neighbour, &held),
            text_of(neighbour, &held_before),
            "the neighbour was rewritten"
        );
        // And nothing of the container's own: a comment is one entry of a list in
        // the frontmatter, not a field of it.
        assert_eq!(fx.epic_body(), body);
    }

    #[test]
    fn a_comment_edit_naming_a_stamp_the_container_has_moved_past_is_refused_as_a_conflict() {
        let fx = Fixture::build();
        let selection = fx.epic_comment_selection(fx.epic_comments()[0].id);
        let read = comment_target(&fx.store, &selection).unwrap();

        // Somebody else writes to the container while the reader is composing
        // theirs. The stamp is the container's, so a change anywhere on it refuses
        // the edit — the same per-entity granularity every other replacement has.
        fx.rewrite_the_epics_body("theirs\n");
        let stale = Write::Replace {
            target: selection.clone(),
            field: Replaceable::CommentText,
            value: "mine\n".to_string(),
            expect: Some(read.stamp),
        };
        // Refused *as a conflict*, not merely refused: the browser asks about this
        // one and reports every other, so a refusal for an unrelated reason arriving
        // here would put the wrong question on screen.
        assert_eq!(perform(&fx.store, &stale), Err(Refusal::Conflict));
        assert_eq!(
            comment_target(&fx.store, &selection).unwrap().text,
            read.text,
            "a refused write wrote"
        );

        // And the same write with the precondition dropped applies over it, which is
        // what a reader asks for by answering the question with overwrite.
        perform(&fx.store, &stale.overwriting()).unwrap();
        assert_eq!(
            comment_target(&fx.store, &selection).unwrap().text,
            "mine\n"
        );
    }

    #[test]
    fn a_withdrawn_comment_keeps_its_number_and_gives_its_text_up() {
        let fx = Fixture::build();
        let held = fx.epic_comments();
        let id = held[0].id;
        let neighbour = fx.an_agents_comment();
        let on_the_node = fx.node_comments();

        perform(
            &fx.store,
            &Write::DeleteComment(fx.epic_comment_selection(id)),
        )
        .unwrap();

        let after = fx.epic_comments();
        let withdrawn = after.iter().find(|c| c.id == id).expect("the slot stays");
        assert!(withdrawn.deleted);
        // Hidden, never removed: the slot stays taken, so the number is never reused
        // and the reader who was told "comment 1" still means this one.
        assert_eq!(after.len(), held.len() + 1, "a slot went");
        assert!(
            !ops::list_comments(&fx.store, &Target::Epic(fx.epic.clone()), false)
                .unwrap()
                .iter()
                .any(|view| matches!(view, CommentView::Live(c) if c.id == id)),
            "a withdrawn comment is still listed as live"
        );
        // The one it named and no other, on the container it named: a list of one
        // cannot tell a withdrawal of the wrong entry from the right one, and a node
        // and an epic are addressed differently.
        assert!(!after.iter().any(|c| c.id == neighbour && c.deleted));
        assert_eq!(fx.node_comments(), on_the_node);
        let on_the_ticket = on_the_node[0].id;
        perform(
            &fx.store,
            &Write::DeleteComment(Selection::Comment(
                Container::Node(fx.node.clone()),
                on_the_ticket,
            )),
        )
        .unwrap();
        assert!(
            fx.node_comments()
                .iter()
                .any(|c| c.id == on_the_ticket && c.deleted),
            "the ticket's own comment was not withdrawn"
        );

        // A second withdrawal is the store's refusal, in the store's own words: the
        // browser offers nothing on a tombstone, so reaching one at all is a caller
        // that has lost track of its row.
        let refused = refusal_words(
            perform(
                &fx.store,
                &Write::DeleteComment(fx.epic_comment_selection(id)),
            )
            .expect_err("a comment is withdrawn once"),
        );
        let its_own =
            ops::delete_comment(&fx.store, &Target::Epic(fx.epic.clone()), id, Actor::Human)
                .expect_err("a comment is withdrawn once")
                .to_string();
        assert_eq!(refused, its_own);
    }

    #[test]
    fn a_comment_opens_on_the_text_it_holds_and_the_stamp_a_precondition_accepts() {
        let fx = Fixture::build();
        // On an epic's comment and on a node's, because the two containers are
        // addressed differently: a read aimed at the wrong one of them would look
        // right against either alone.
        for (selection, stored) in [
            (
                fx.epic_comment_selection(fx.epic_comments()[0].id),
                fx.epic_comments()[0].text.clone(),
            ),
            (
                Selection::Comment(Container::Node(fx.node.clone()), fx.node_comments()[0].id),
                fx.node_comments()[0].text.clone(),
            ),
        ] {
            let read = comment_target(&fx.store, &selection).unwrap();
            assert_eq!(read.selection, selection);
            assert_eq!(read.text, stored);
            // The stamp is the one the store accepts as a precondition: a write
            // naming it goes through, which no comparison of timestamps here could
            // prove.
            perform(
                &fx.store,
                &Write::Replace {
                    target: selection.clone(),
                    field: Replaceable::CommentText,
                    value: stored.clone(),
                    expect: Some(read.stamp),
                },
            )
            .unwrap_or_else(|e| panic!("{selection:?}: {e:?}"));
        }

        // A comment that has been withdrawn since the letter was offered has no text
        // to open a buffer on, and one that is not there at all is refused the same
        // way — by name, so the reader is told which row went.
        let withdrawn = fx.epic_comment_selection(fx.a_withdrawn_comment());
        for gone in [withdrawn, fx.epic_comment_selection(9999)] {
            let err = comment_target(&fx.store, &gone)
                .expect_err("there is no text to edit")
                .to_string();
            assert!(err.contains(&gone.reference()), "{gone:?}: {err}");
        }
        // And a row that is not a comment at all says so rather than reading one.
        let err = comment_target(&fx.store, &fx.epic_selection())
            .expect_err("an epic is not a comment")
            .to_string();
        assert!(err.contains(&fx.epic), "{err}");
    }

    #[test]
    fn a_container_is_named_by_the_noun_the_command_line_addresses_it_by() {
        let fx = Fixture::build();
        // A message that tells a reader which command does a job the browser does
        // not has to name the command that exists: an epic's collections and a
        // node's collections are different commands, and every node is a `ticket`
        // there — a subticket included, since the command line has no second noun
        // for a node with a parent.
        assert_eq!(Container::Epic(fx.epic.clone()).cli_noun(), "epic");
        assert_eq!(Container::Node(fx.node.clone()).cli_noun(), "ticket");
        assert_eq!(Container::Node(fx.subnode.clone()).cli_noun(), "ticket");
    }

    #[test]
    fn an_epic_is_never_offered_a_dependency_list_to_write_to() {
        let fx = Fixture::build();
        let before = fx.epic_labels();
        // An epic is not a unit of work that can be blocked, so it carries no
        // dependency list among its collections — and the seam refuses one by name
        // rather than writing somewhere else, so a row that could not exist cannot
        // become a write either.
        assert!(!Container::Epic(fx.epic.clone())
            .collections()
            .contains(&Collection::BlockedBy));
        let epics_list =
            Selection::Collection(Container::Epic(fx.epic.clone()), Collection::BlockedBy);
        let err = refusal_words(
            perform(&fx.store, &Write::AddBlocker(epics_list, "1".into()))
                .expect_err("an epic has no dependency list"),
        );
        assert!(err.contains(&fx.epic), "{err}");
        assert_eq!(fx.epic_labels(), before, "something else was written");
    }

    #[test]
    fn an_epic_target_carries_its_stored_fields_and_stamp() {
        let fx = Fixture::build();
        let store = &fx.store;
        let target = edit_target(store, &fx.epic_selection()).unwrap();
        let stored = store.read_epic(&fx.epic).unwrap();
        assert_eq!(target.name, stored.frontmatter.name);
        assert_eq!(target.summary, stored.frontmatter.summary);
        assert_eq!(target.body, stored.body);
        assert_eq!(target.stamp, Stamp(stored.frontmatter.updated));
    }

    #[test]
    fn a_node_target_carries_its_stored_fields_and_stamp() {
        let fx = Fixture::build();
        let (store, r) = (&fx.store, &fx.node);
        let target = edit_target(store, &Selection::Node(r.clone())).unwrap();
        let stored = store.read_node(&r.epic_id, r.number).unwrap();
        assert_eq!(target.name, stored.frontmatter.name);
        assert_eq!(target.summary, stored.frontmatter.summary);
        assert_eq!(target.body, stored.body);
        assert_eq!(target.stamp, Stamp(stored.frontmatter.updated));
    }

    #[test]
    fn a_target_is_re_read_so_a_change_since_the_last_listing_is_already_in_it() {
        let fx = Fixture::build();
        let (store, r) = (&fx.store, &fx.node);
        let before = edit_target(store, &Selection::Node(r.clone())).unwrap();

        // Someone else's write between two reads: the second read must show it,
        // which is why an editing surface re-reads instead of trusting a preview.
        ops::edit_node(
            store,
            r,
            NodeEdits {
                body: Some("theirs\n".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let after = edit_target(store, &Selection::Node(r.clone())).unwrap();
        assert_eq!(after.body, "theirs\n");
        assert_ne!(after.stamp, before.stamp);
    }

    #[test]
    fn the_stamp_of_an_epic_target_is_the_one_a_precondition_accepts() {
        let fx = Fixture::build();
        let store = &fx.store;
        let target = edit_target(store, &fx.epic_selection()).unwrap();

        // Nothing changed since the read, so the write the stamp guards applies.
        ops::edit_epic(
            store,
            &fx.epic,
            EpicEdits {
                body: Some("mine\n".into()),
                expect_updated: Some(target.stamp.0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(store.read_epic(&fx.epic).unwrap().body, "mine\n");

        // The write bumped the stamp, so the one read before it is now stale and
        // the same precondition refuses — as a conflict specifically, not merely
        // as some error, so a regression that turns this into a different
        // refusal (the version gate, a lock failure) does not pass unnoticed.
        let err = ops::edit_epic(
            store,
            &fx.epic,
            EpicEdits {
                body: Some("again\n".into()),
                expect_updated: Some(target.stamp.0),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, ops::OpError::Store(StoreError::Conflict { .. })),
            "unexpected error: {err}"
        );
        assert_eq!(store.read_epic(&fx.epic).unwrap().body, "mine\n");
    }

    #[test]
    fn the_stamp_of_a_node_target_is_the_one_a_precondition_accepts() {
        let fx = Fixture::build();
        let (store, r) = (&fx.store, &fx.node);
        let target = edit_target(store, &Selection::Node(r.clone())).unwrap();

        ops::edit_node(
            store,
            r,
            NodeEdits {
                body: Some("mine\n".into()),
                expect_updated: Some(target.stamp.0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            store.read_node(&r.epic_id, r.number).unwrap().body,
            "mine\n"
        );

        let err = ops::edit_node(
            store,
            r,
            NodeEdits {
                body: Some("again\n".into()),
                expect_updated: Some(target.stamp.0),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, ops::OpError::Store(StoreError::Conflict { .. })),
            "unexpected error: {err}"
        );
        assert_eq!(
            store.read_node(&r.epic_id, r.number).unwrap().body,
            "mine\n"
        );
    }

    #[test]
    fn a_target_that_is_gone_names_it_rather_than_a_path() {
        let fx = Fixture::build();
        let (store, r) = (&fx.store, &fx.node);
        std::fs::remove_file(store.node_path(&r.epic_id, r.number)).unwrap();
        let err = edit_target(store, &Selection::Node(r.clone()))
            .unwrap_err()
            .to_string();
        // The reference alone is not evidence: a raw I/O error quotes the file
        // path, which contains the reference as a substring. The message must be
        // the existence refusal, with no path in it at all.
        assert_eq!(err, format!("node {r} does not exist"));
        assert!(!err.contains(".md"), "unexpected message: {err}");
    }

    #[test]
    fn a_body_is_written_verbatim_under_the_stamp_its_text_was_read_at() {
        let fx = Fixture::build();
        let target = edit_target(&fx.store, &fx.epic_selection()).unwrap();
        let name = ops::read_epic(&fx.store, &fx.epic)
            .unwrap()
            .frontmatter
            .name;

        // Nothing has changed since the read, so the precondition holds and the
        // text lands exactly as the reader left it: line breaks, blank lines and a
        // last line with no break after it. Whether a body is acceptable is the
        // store's rule and the browser normalises none of it.
        let written = "# mine\n\nwith a blank line above, and no break after this";
        perform(
            &fx.store,
            &Write::Replace {
                target: fx.epic_selection(),
                field: Replaceable::Field(FreeForm::Body),
                value: written.to_string(),
                expect: Some(target.stamp),
            },
        )
        .unwrap();
        assert_eq!(fx.epic_body(), written);
        // The body and nothing else: a whole-field replacement that took anything
        // else with it would be a save that quietly reverted something.
        assert_eq!(
            ops::read_epic(&fx.store, &fx.epic)
                .unwrap()
                .frontmatter
                .name,
            name
        );

        // A node is addressed differently from an epic, so a write aimed at the
        // wrong one of the two would pass everything above.
        let node = Selection::Node(fx.node.clone());
        let stamp = edit_target(&fx.store, &node).unwrap().stamp;
        perform(
            &fx.store,
            &Write::Replace {
                target: node,
                field: Replaceable::Field(FreeForm::Body),
                value: "the ticket's own\n".to_string(),
                expect: Some(stamp),
            },
        )
        .unwrap();
        assert_eq!(
            ops::read_node(&fx.store, &fx.node).unwrap().body,
            "the ticket's own\n"
        );
        assert_eq!(fx.epic_body(), written, "the epic's body was written too");
    }

    #[test]
    fn a_replacement_lands_in_the_field_it_names_and_leaves_the_others_as_they_were() {
        let fx = Fixture::build();
        // On an epic and on a node, because the two are addressed differently and
        // carry the same three fields: a write that reached for the wrong field of
        // the right entity, or the right field of the wrong kind, looks correct from
        // any single case.
        for target in [fx.epic_selection(), fx.node_selection()] {
            for field in FreeForm::ALL.iter().copied() {
                let read = edit_target(&fx.store, &target).unwrap();
                let written = format!("the replacement {} of {target:?}", field.noun());
                perform(
                    &fx.store,
                    &Write::Replace {
                        target: target.clone(),
                        field: Replaceable::Field(field),
                        value: written.clone(),
                        expect: Some(read.stamp),
                    },
                )
                .unwrap_or_else(|e| panic!("{field:?} on {target:?}: {e:?}"));
                // The field named, and the other two exactly as they were: a
                // replacement that carried a neighbour along would be a save that
                // quietly blanked something the reader never opened.
                let after = edit_target(&fx.store, &target).unwrap();
                for other in FreeForm::ALL.iter().copied() {
                    let expected = match other == field {
                        true => &written,
                        false => other.of(&read),
                    };
                    assert_eq!(
                        other.of(&after),
                        expected,
                        "replacing the {} of {target:?} wrote the {}",
                        field.noun(),
                        other.noun()
                    );
                }
            }
        }
    }

    #[test]
    fn no_two_replaceable_fields_are_named_the_same_word_or_read_off_the_same_value() {
        let fx = Fixture::build();
        // The noun is what every surface, notice and warning calls the field, and
        // the value is what a surface opens on. Two fields sharing either would put
        // one field's text under another's name — and write it back there.
        let target = edit_target(&fx.store, &fx.epic_selection()).unwrap();
        // Every whole field the browser replaces, a comment's text among them: the
        // letter that opens a body opens that instead on a comment row, so the two
        // are read and written by one path and must not be called one thing.
        let mut nouns: Vec<&str> = Replaceable::ALL.iter().map(|f| f.noun()).collect();
        nouns.sort_unstable();
        nouns.dedup();
        assert_eq!(nouns.len(), Replaceable::ALL.len());
        let mut values: Vec<&str> = FreeForm::ALL.iter().map(|f| f.of(&target)).collect();
        values.sort_unstable();
        values.dedup();
        assert_eq!(
            values.len(),
            FreeForm::ALL.len(),
            "the fixture cannot tell the fields apart: {target:?}"
        );
        // And each of them is the value the store holds under that name, so the
        // three are not merely distinct but the right way round.
        assert_eq!(FreeForm::Name.of(&target), target.name);
        assert_eq!(FreeForm::Summary.of(&target), target.summary);
        assert_eq!(FreeForm::Body.of(&target), target.body);
    }

    #[test]
    fn a_body_write_naming_a_stamp_the_entity_has_moved_past_is_refused_as_a_conflict() {
        let fx = Fixture::build();
        let stamp = edit_target(&fx.store, &fx.epic_selection()).unwrap().stamp;

        // Somebody else writes while the reader is composing theirs, which is the
        // one window no lock can cover: the stamp is what closes it.
        fx.rewrite_the_epics_body("theirs\n");
        let refused = perform(
            &fx.store,
            &Write::Replace {
                target: fx.epic_selection(),
                field: Replaceable::Field(FreeForm::Body),
                value: "mine\n".to_string(),
                expect: Some(stamp),
            },
        )
        .expect_err("a stale stamp is refused");
        // Refused *as a conflict*, not merely refused: the browser asks about this
        // one and reports every other, so a refusal for an unrelated reason
        // arriving here would put the wrong question on screen.
        assert_eq!(refused, Refusal::Conflict);
        assert_eq!(fx.epic_body(), "theirs\n", "a refused write wrote");

        // And the same write with the precondition dropped applies over it, which
        // is what a reader asks for by answering the question with overwrite.
        perform(
            &fx.store,
            &Write::Replace {
                target: fx.epic_selection(),
                field: Replaceable::Field(FreeForm::Body),
                value: "mine\n".to_string(),
                expect: Some(stamp),
            }
            .overwriting(),
        )
        .unwrap();
        assert_eq!(fx.epic_body(), "mine\n");
    }

    #[test]
    fn a_change_to_anything_else_on_the_entity_refuses_a_body_write_too() {
        let fx = Fixture::build();
        let stamp = edit_target(&fx.store, &fx.epic_selection()).unwrap().stamp;
        let before = fx.epic_body();

        // The precondition is the entity's, not the field's: a comment arriving
        // mid-edit refuses a body save. Accepted deliberately — per-field stamps
        // would buy precision for a lot of machinery, and a refusal costs the
        // reader one answer.
        ops::add_comment(
            &fx.store,
            &Target::Epic(fx.epic.clone()),
            loti_core::Actor::Human,
            "arrived mid-edit\n".to_string(),
        )
        .unwrap();
        assert_eq!(
            perform(
                &fx.store,
                &Write::Replace {
                    target: fx.epic_selection(),
                    field: Replaceable::Field(FreeForm::Body),
                    value: "mine\n".to_string(),
                    expect: Some(stamp),
                },
            ),
            Err(Refusal::Conflict)
        );
        assert_eq!(fx.epic_body(), before);
    }

    #[test]
    fn only_a_row_that_holds_a_whole_field_may_have_it_replaced() {
        let fx = Fixture::build();
        let before = fx.epic_body();
        let comment = fx.epic_comment_selection(fx.epic_comments()[0].id);
        // Which rows hold which whole field: an epic and a node have a name, a
        // summary and a body, a comment has its text, and no row holds both. A
        // collection and its other members are edited by their own operations. Every
        // pairing the browser does not offer is a caller that has lost track of what
        // its row points at — refused by name, with nothing written on the way to
        // refusing.
        for wrong in [
            Selection::Collection(Container::Epic(fx.epic.clone()), Collection::Labels),
            Selection::Label(
                Container::Epic(fx.epic.clone()),
                fx.epic_labels()[0].clone(),
            ),
            Selection::Asset(
                Container::Epic(fx.epic.clone()),
                fx.epic_assets()[0].clone(),
            ),
            comment.clone(),
        ] {
            // Every replaceable field, because each is refused for the same reason
            // and each has to say which field the row does not have.
            for field in Replaceable::ALL.iter().copied() {
                // The one pairing that is not misdirected at all: a comment's text
                // on a comment is the write this whole path exists for.
                if wrong == comment && field == Replaceable::CommentText {
                    continue;
                }
                let err = refusal_words(
                    perform(
                        &fx.store,
                        &Write::Replace {
                            target: wrong.clone(),
                            field,
                            value: "nowhere\n".to_string(),
                            expect: None,
                        },
                    )
                    .expect_err("the row has no such field"),
                );
                assert!(err.contains(&fx.epic), "{wrong:?}: {err}");
                // An entity's field says which field the row has not got; a
                // comment's text says the row is not a comment, since there is no
                // field of the row to name.
                if let Replaceable::Field(named) = field {
                    assert!(err.contains(named.noun()), "{wrong:?}: {err}");
                }
                assert_eq!(fx.epic_body(), before, "{wrong:?} wrote something");
            }
        }
        // And the other way round: a comment's text is not a field an epic or a node
        // has, so the write that reaches one is refused rather than landing in a
        // body.
        for entity in [fx.epic_selection(), fx.node_selection()] {
            let err = refusal_words(
                perform(
                    &fx.store,
                    &Write::Replace {
                        target: entity.clone(),
                        field: Replaceable::CommentText,
                        value: "nowhere\n".to_string(),
                        expect: None,
                    },
                )
                .expect_err("an entity has no comment text of its own"),
            );
            assert!(err.contains(&entity.reference()), "{entity:?}: {err}");
        }
        assert_eq!(fx.epic_body(), before);
    }

    #[test]
    fn a_claim_is_taken_for_the_holder_typed_and_reassigned_to_the_next_one() {
        let fx = Fixture::build();
        assert_eq!(fx.node_claim(), None, "the fixture starts unclaimed");

        // Verbatim, as the reader typed it: a holder is freeform text, and what
        // makes one acceptable is the store's rule rather than a browser's tidying.
        perform(
            &fx.store,
            &Write::TakeClaim(fx.node_selection(), "a human".into()),
        )
        .unwrap();
        let taken = fx.node_claim().expect("the ticket reads back claimed");
        assert_eq!(taken.by, "a human");
        // The instant is the store's, and no write from here can name one: it is
        // the same instant the store stamped the entity's own change with.
        assert_eq!(taken.at, fx.node_updated());

        // A claim has one holder, so taking one that is already held reassigns it:
        // the holder that was there is replaced rather than joined.
        perform(
            &fx.store,
            &Write::TakeClaim(fx.node_selection(), "somebody else".into()),
        )
        .unwrap();
        let reassigned = fx.node_claim().expect("the ticket is still claimed");
        assert_eq!(reassigned.by, "somebody else");
        assert_eq!(reassigned.at, fx.node_updated());

        // A holder with nothing in it is the store's refusal, in the store's own
        // words, and it leaves the claim that was there alone: the browser
        // reimplements no store rule, so a row is never left marked for nobody.
        let err = refusal_words(
            perform(
                &fx.store,
                &Write::TakeClaim(fx.node_selection(), "  ".into()),
            )
            .expect_err("a claim needs a holder"),
        );
        assert!(!err.is_empty(), "the refusal says nothing");
        assert_eq!(fx.node_claim(), Some(reassigned));
    }

    #[test]
    fn a_claim_is_released_from_the_ticket_it_names_holder_and_instant_together() {
        let fx = Fixture::build();
        let held = fx.claim(&fx.node);
        assert_eq!(fx.node_claim().map(|c| c.by), Some(held));

        perform(&fx.store, &Write::ReleaseClaim(fx.node_selection())).unwrap();
        // Both halves go: a claim never carries a holder without an instant, so one
        // left behind would be a claim the reader cannot see and cannot release.
        assert_eq!(fx.node_claim(), None);

        // Releasing a claim nobody holds is the store's own no-op rather than a
        // refusal the browser invents on its behalf.
        perform(&fx.store, &Write::ReleaseClaim(fx.node_selection())).unwrap();
        assert_eq!(fx.node_claim(), None);
    }

    #[test]
    fn only_a_unit_of_work_can_be_claimed_or_released() {
        let fx = Fixture::build();
        let held = fx.claim(&fx.node);

        // A claim is taken on a unit of work: an epic is not one, and neither is a
        // collection or one of its members. Each is refused by name, through the
        // seam the browser itself writes through, with nothing written on the way to
        // refusing — so a wrongly wired row meets the same guard.
        for wrong in [
            fx.epic_selection(),
            fx.blocked_by_selection(),
            Selection::Label(
                Container::Epic(fx.epic.clone()),
                fx.epic_labels()[0].clone(),
            ),
        ] {
            for write in [
                Write::TakeClaim(wrong.clone(), "nobody".into()),
                Write::ReleaseClaim(wrong.clone()),
            ] {
                let err =
                    refusal_words(perform(&fx.store, &write).expect_err("only a node has a claim"));
                assert!(err.contains(&fx.epic), "{write:?}: {err}");
                // And the claim that is held elsewhere is untouched: a misdirected
                // write must not land on the nearest thing that could take it.
                assert_eq!(
                    fx.node_claim().map(|c| c.by),
                    Some(held.clone()),
                    "{write:?} wrote something"
                );
            }
        }
    }

    /// A state pick, as the surface builds one.
    fn pick(selection: &Selection, state: State, reason: &str, cascade: bool) -> Write {
        Write::SetState {
            target: selection.clone(),
            state,
            reason: reason.to_string(),
            cascade,
        }
    }

    #[test]
    fn the_states_a_row_offers_are_its_own_kinds_and_are_named_as_the_store_names_them() {
        let fx = Fixture::build();

        // A unit of work offers the five states of the state machine, in the order it
        // reads. Their words come from the store's own type rather than being spelled
        // again here, so the browser cannot call a state something the command line
        // does not.
        let work = State::offered(&fx.node_selection()).expect("a ticket has a state");
        assert_eq!(
            work,
            [
                NodeState::ToDo,
                NodeState::InProgress,
                NodeState::Blocked,
                NodeState::Done,
                NodeState::Closed,
            ]
            .map(State::Work)
        );
        for state in work {
            let State::Work(held) = state else {
                panic!("{state:?} is not a unit of work's state")
            };
            assert_eq!(state.wire_name(), held.wire_name());
        }

        // An epic offers its stored flag, off and on, and nothing else — in
        // particular not the state computed from its nodes, which nobody sets.
        let epic = State::offered(&fx.epic_selection()).expect("an epic has a state");
        assert_eq!(epic, [State::EpicOpen, State::EpicClosed]);
        assert_eq!(epic[0].wire_name(), EpicStatus::Open.wire_name());
        assert_eq!(epic[1].wire_name(), EpicStatus::Closed.wire_name());
        for offered in [work, epic].concat() {
            assert_ne!(
                offered.wire_name(),
                EpicStatus::Completed.wire_name(),
                "a computed state is offered as one to pick"
            );
        }

        // And nothing else has a state of its own: a collection is structure and its
        // members carry none, so no row of one is offered a picker at all.
        for stateless in [
            fx.blocked_by_selection(),
            Selection::Label(
                Container::Epic(fx.epic.clone()),
                fx.epic_labels()[0].clone(),
            ),
        ] {
            assert_eq!(State::offered(&stateless), None, "{stateless:?}");
            state_target(&fx.store, &stateless).expect_err("a row with no state opens no picker");
        }
    }

    #[test]
    fn every_state_the_store_will_not_take_without_a_reason_is_one_the_surface_asks_for_one_in() {
        // Derived from the store rather than restated: whether a state says why is a
        // store rule, and the surface reveals its reason field for exactly the states
        // that rule refuses without one. A leaf, so that the one state with a
        // precondition of its own is refused for the reason under test or not at all.
        for state in State::offered(&Selection::Node(Fixture::build().subnode.clone()))
            .expect("a ticket has states")
            .iter()
            .copied()
        {
            let fx = Fixture::build();
            let refused = perform(
                &fx.store,
                &pick(&fx.subnode_selection(), state, "  ", false),
            );
            assert_eq!(
                refused.is_err(),
                state.needs_reason(),
                "{state:?} with no reason: {refused:?}"
            );
        }

        // A reason the store takes is written as the reader left it, on the state that
        // says why and on no other: leaving a state that carries one clears it, which
        // is what stops a resolved row explaining itself with somebody's old words.
        let fx = Fixture::build();
        let blocked = State::Work(NodeState::Blocked);
        perform(
            &fx.store,
            &pick(&fx.node_selection(), blocked, "waiting on review", false),
        )
        .unwrap();
        assert_eq!(
            fx.node_state(&fx.node),
            (
                blocked.wire_name().to_string(),
                Some("waiting on review".to_string()),
                None
            )
        );
        let closed = State::Work(NodeState::Closed);
        perform(
            &fx.store,
            &pick(&fx.node_selection(), closed, "not wanted", false),
        )
        .unwrap();
        assert_eq!(
            fx.node_state(&fx.node),
            (
                closed.wire_name().to_string(),
                None,
                Some("not wanted".to_string())
            )
        );
        let started = State::Work(NodeState::InProgress);
        perform(&fx.store, &pick(&fx.node_selection(), started, "", false)).unwrap();
        assert_eq!(
            fx.node_state(&fx.node),
            (started.wire_name().to_string(), None, None)
        );
    }

    #[test]
    fn a_state_pick_moves_the_row_it_names_and_leaves_every_other_row_alone() {
        let fx = Fixture::build();
        let before = fx.node_state(&fx.subnode);
        let done = State::Work(NodeState::Done);

        // The leaf, because a state pick aimed at the wrong node of an epic looks
        // right for as long as only one of them is read back.
        let effect = perform(&fx.store, &pick(&fx.subnode_selection(), done, "", false)).unwrap();
        assert_eq!(
            fx.node_state(&fx.subnode).0,
            done.wire_name(),
            "the row the pick named did not move"
        );
        // Nothing about it was more than what was asked for, so the notice about it
        // is the one the surface worded.
        assert_eq!(effect, Effect::AsAsked);
        // Its parent and its sibling are where they were: one pick moves one row.
        assert_eq!(fx.node_state(&fx.node), before);
        assert_eq!(fx.node_state(&fx.blocker), before);
        assert_eq!(fx.epic_closed(), (false, None));
    }

    #[test]
    fn an_epics_flag_is_what_its_pick_sets_and_its_nodes_are_never_touched_by_it() {
        let fx = Fixture::build();
        let states: Vec<(String, Option<String>, Option<String>)> = [&fx.node, &fx.subnode]
            .iter()
            .map(|node| fx.node_state(node))
            .collect();

        let effect = perform(
            &fx.store,
            &pick(&fx.epic_selection(), State::EpicClosed, "shipped", true),
        )
        .unwrap();
        assert_eq!(fx.epic_closed(), (true, Some("shipped".to_string())));
        // An epic's closed flag never touches its nodes, so nothing under it moves —
        // even when the pick was asked to cascade, which an epic has no way to do.
        for (node, was) in [&fx.node, &fx.subnode].iter().zip(&states) {
            assert_eq!(&fx.node_state(node), was, "{node} moved with its epic");
        }
        assert_eq!(effect, Effect::AsAsked);

        // And reopening clears the flag and the reason together: a reopened epic must
        // not still say why it was closed.
        perform(
            &fx.store,
            &pick(&fx.epic_selection(), State::EpicOpen, "", false),
        )
        .unwrap();
        assert_eq!(fx.epic_closed(), (false, None));
    }

    #[test]
    fn closing_alone_leaves_an_open_descendant_and_a_cascade_closes_it_and_says_how_many() {
        let fx = Fixture::build();
        let closed = State::Work(NodeState::Closed);
        let open = fx.open_descendants(&fx.node);
        assert!(open > 0, "the fixture's ticket has nothing to cascade to");

        // A plain close resolves the row and nothing under it, so the row can be
        // reopened later without its subtree having been rewritten.
        let effect = perform(&fx.store, &pick(&fx.node_selection(), closed, "a", false)).unwrap();
        assert_eq!(fx.node_state(&fx.node).0, closed.wire_name());
        assert_eq!(fx.open_descendants(&fx.node), open, "a close cascaded");
        assert_eq!(effect, Effect::AsAsked);

        // A cascade closes them, and how many it closed is the store's answer: the
        // count a surface showed was the plan as it stood when the surface opened.
        let effect = perform(&fx.store, &pick(&fx.node_selection(), closed, "a", true)).unwrap();
        assert_eq!(fx.open_descendants(&fx.node), 0);
        assert_eq!(effect, Effect::AlsoClosed(open));

        // A cascade that finds nothing left to close — somebody else got there first —
        // is an ordinary close and is reported as one rather than as a cascade of
        // nothing.
        let effect = perform(&fx.store, &pick(&fx.node_selection(), closed, "a", true)).unwrap();
        assert_eq!(effect, Effect::AsAsked);
    }

    #[test]
    fn the_gate_on_a_row_with_open_descendants_is_the_stores_own_refusal_word_for_word() {
        let fx = Fixture::build();
        let done = State::Work(NodeState::Done);
        assert!(fx.open_descendants(&fx.node) > 0, "nothing gates the pick");
        let was = fx.node_state(&fx.node);

        // Nothing is pre-checked here: the state is offered, the pick is attempted,
        // and what comes back is the store's own words — which is why the browser
        // cannot go stale when the rule gains a nuance. Compared against the words the
        // operation itself produces rather than against a copy written out here.
        let expected = ops::set_node_status(&fx.store, &fx.node, NodeStatusChange::Done)
            .expect_err("the store gates this pick")
            .to_string();
        let refused = refusal_words(
            perform(&fx.store, &pick(&fx.node_selection(), done, "", false))
                .expect_err("the store gates this pick"),
        );
        assert_eq!(refused, expected);
        assert_eq!(
            fx.node_state(&fx.node),
            was,
            "a refused pick wrote something"
        );
    }

    #[test]
    fn a_cascade_that_stops_partway_reports_that_the_store_changed() {
        let fx = Fixture::build();
        let closed = State::Work(NodeState::Closed);
        // A second descendant lets the first write before the controlled failure.
        let store = Store::at(fx.store.root()).with_lock_config(LockConfig {
            stale_threshold: Duration::from_millis(80),
            retry_interval: Duration::from_millis(5),
        });
        let tail = ops::create_node(
            &store,
            NewNode {
                epic_id: fx.epic.clone(),
                parent: Some(fx.subnode.clone()),
                name: "cascade tail".into(),
                summary: String::new(),
                labels: Vec::new(),
                body: String::new(),
            },
        )
        .expect("the tail can be created");
        let held = lock::try_acquire(&store.node_path(&fx.epic, tail.frontmatter.number))
            .expect("the tail lock can be taken")
            .expect("the tail was unlocked");

        let refused = perform(&store, &pick(&fx.node_selection(), closed, "a", true))
            .expect_err("the cascade stops after its first descendant");
        drop(held);

        assert!(refused.changed(), "partial progress must request a reload");
        let Refusal::Partial(_) = refused else {
            panic!("the partial cascade was not classified as changed");
        };
        assert_eq!(fx.node_state(&fx.node).0, NodeState::ToDo.wire_name());
        assert_eq!(fx.node_state(&fx.subnode).0, NodeState::Closed.wire_name());
    }

    #[test]
    fn a_cascade_that_stops_at_its_first_descendant_is_unchanged_and_not_partial() {
        let fx = Fixture::build();
        let closed = State::Work(NodeState::Closed);
        let store = Store::at(fx.store.root()).with_lock_config(LockConfig {
            stale_threshold: Duration::from_millis(80),
            retry_interval: Duration::from_millis(5),
        });
        let was = (
            fx.node_state(&fx.node),
            fx.node_state(&fx.subnode),
            fx.open_descendants(&fx.node),
        );
        // The first planned descendant cannot be locked, so no independent step
        // can publish before the controlled refusal.
        let held = lock::try_acquire(&store.node_path(&fx.epic, fx.subnode.number))
            .expect("the first descendant lock can be taken")
            .expect("the first descendant was unlocked");
        let refused = perform(&store, &pick(&fx.node_selection(), closed, "a", true))
            .expect_err("the cascade stops before its first descendant");
        // The operation supplies the displayed words. Run it while the same lock
        // remains held, so this proves the browser did not paraphrase its refusal.
        let expected = ops::set_node_status(
            &store,
            &fx.node,
            NodeStatusChange::Closed {
                reason: Some("a".into()),
                cascade: true,
            },
        )
        .expect_err("the store still refuses the first descendant")
        .to_string();
        // Release before assertions so fixture cleanup is never coupled to one.
        drop(held);

        assert!(
            !refused.changed(),
            "a cascade that wrote nothing must not request a reload"
        );
        let Refusal::Rule(words) = refused else {
            panic!("a cascade that stopped before writing was classified as partial");
        };
        assert_eq!(words, expected);
        assert_eq!(
            (
                fx.node_state(&fx.node),
                fx.node_state(&fx.subnode),
                fx.open_descendants(&fx.node),
            ),
            was,
            "a refusal at the first descendant changed the store"
        );
    }

    #[test]
    fn a_state_no_row_of_that_kind_has_is_refused_by_name_and_writes_nothing() {
        let fx = Fixture::build();
        let was = (fx.node_state(&fx.node), fx.epic_closed());

        // An epic's flag is not a state of the work and the state machine is not an
        // epic's, so each aimed at the other is a caller that has lost track of what
        // its row points at — refused by name, with nothing written on the way.
        for wrong in [
            pick(&fx.node_selection(), State::EpicClosed, "a", false),
            pick(
                &fx.epic_selection(),
                State::Work(NodeState::Done),
                "",
                false,
            ),
            pick(
                &fx.blocked_by_selection(),
                State::Work(NodeState::Done),
                "",
                false,
            ),
        ] {
            let err = refusal_words(perform(&fx.store, &wrong).expect_err("no such state here"));
            assert!(
                err.contains(&wrong.target().reference()),
                "{wrong:?} refused without naming what it was aimed at: {err}"
            );
            assert_eq!(
                (fx.node_state(&fx.node), fx.epic_closed()),
                was,
                "{wrong:?} wrote something"
            );
        }
    }

    #[test]
    fn a_picker_opens_on_the_state_the_store_holds_and_on_how_much_a_cascade_would_close() {
        let fx = Fixture::build();

        // The state the store holds now, and the plan as it stands now: both are read
        // when the letter is pressed, so a picker never marks a state the row has
        // already left.
        let ticket = state_target(&fx.store, &fx.node_selection()).unwrap();
        assert_eq!(ticket.current.wire_name(), fx.node_state(&fx.node).0);
        assert_eq!(ticket.open_descendants, fx.open_descendants(&fx.node));
        assert!(ticket.open_descendants > 0);

        // It follows the store: a descendant resolved behind the browser's back is a
        // descendant the next picker does not offer to close.
        perform(
            &fx.store,
            &pick(
                &fx.subnode_selection(),
                State::Work(NodeState::Done),
                "",
                false,
            ),
        )
        .unwrap();
        let ticket = state_target(&fx.store, &fx.node_selection()).unwrap();
        assert_eq!(ticket.open_descendants, 0);

        // An epic's picker opens on its stored flag rather than on the state its row
        // shows: an epic whose every node is resolved reads as completed, and
        // completed is not a state anybody picks — the flag is still off.
        perform(
            &fx.store,
            &pick(
                &fx.node_selection(),
                State::Work(NodeState::Done),
                "",
                false,
            ),
        )
        .unwrap();
        perform(
            &fx.store,
            &pick(
                &Selection::Node(fx.blocker.clone()),
                State::Work(NodeState::Done),
                "",
                false,
            ),
        )
        .unwrap();
        let shown = rows(&fx.store, &Level::Epics).unwrap();
        let RowKind::Work { status, .. } = &shown[0].kind else {
            panic!("an epic's row is work")
        };
        assert_eq!(status, EpicStatus::Completed.wire_name());
        let epic = state_target(&fx.store, &fx.epic_selection()).unwrap();
        assert_eq!(epic.current, State::EpicOpen);
        // And an epic never offers a cascade: its flag does not reach its nodes, so
        // there is nothing for the count to be about.
        assert_eq!(epic.open_descendants, 0);

        perform(
            &fx.store,
            &pick(&fx.epic_selection(), State::EpicClosed, "done with", false),
        )
        .unwrap();
        assert_eq!(
            state_target(&fx.store, &fx.epic_selection())
                .unwrap()
                .current,
            State::EpicClosed
        );
    }

    #[test]
    fn every_write_says_what_it_is_aimed_at_and_only_a_stamped_one_drops_a_precondition() {
        let fx = Fixture::build();
        let stamp = edit_target(&fx.store, &fx.epic_selection()).unwrap().stamp;
        let body = Write::Replace {
            target: fx.epic_selection(),
            field: Replaceable::Field(FreeForm::Body),
            value: "mine\n".to_string(),
            expect: Some(stamp),
        };
        // Every write names the row it is aimed at, so a question raised about one
        // names the entity rather than re-deriving a reference of its own.
        for write in [
            Write::AddLabel(fx.blocked_by_selection(), "x".into()),
            Write::RemoveLabel(fx.epic_selection()),
            Write::AddBlocker(fx.blocked_by_selection(), "1".into()),
            Write::RemoveBlocker(fx.epic_selection()),
            Write::DeleteAsset(fx.epic_selection()),
            Write::AddComment(fx.epic_selection(), "a remark".into()),
            Write::DeleteComment(fx.epic_selection()),
            Write::TakeClaim(fx.node_selection(), "a human".into()),
            Write::ReleaseClaim(fx.node_selection()),
            Write::SetState {
                target: fx.node_selection(),
                state: State::Work(NodeState::Done),
                reason: String::new(),
                cascade: false,
            },
            body.clone(),
        ] {
            // Dropping the precondition changes the precondition and nothing else:
            // a write that came back aimed somewhere else would overwrite the
            // wrong entity with the reader's text.
            assert_eq!(write.overwriting().target(), write.target(), "{write:?}");
        }
        // A stamped write loses its stamp and keeps everything else — which field,
        // and the text the reader left — for every field it may name: dropping the
        // precondition is the reader saying "write it anyway", not "write something
        // else".
        for field in Replaceable::ALL.iter().copied() {
            let stamped = Write::Replace {
                target: fx.epic_selection(),
                field,
                value: "mine\n".to_string(),
                expect: Some(stamp),
            };
            assert_eq!(
                stamped.overwriting(),
                Write::Replace {
                    target: fx.epic_selection(),
                    field,
                    value: "mine\n".to_string(),
                    expect: None,
                }
            );
        }
        let unstamped = Write::RemoveLabel(fx.epic_selection());
        assert_eq!(unstamped.overwriting(), unstamped);
    }

    #[test]
    fn a_store_this_binary_may_write_is_not_read_only() {
        let fx = Fixture::build();
        assert_eq!(read_only(&fx.store), None);
    }

    #[test]
    fn every_reason_the_gate_refuses_a_mutation_for_is_listed() {
        // The list is what every surface that needs one marker per reason walks, so
        // a reason missing from it is a reason with no marker, no width reserved and
        // no test coverage — none of which fails on its own. The exhaustive match is
        // what makes leaving one out a compile error rather than a silent gap; the
        // length is what stops a variant being listed twice in place of the missing
        // one.
        for state in ReadOnly::ALL.iter().copied() {
            match state {
                ReadOnly::NeedsMigration
                | ReadOnly::MigrationInProgress
                | ReadOnly::NeedsNewerLoti
                | ReadOnly::VersionUnreadable => {}
            }
        }
        assert_eq!(ReadOnly::ALL.len(), 4);
    }

    #[test]
    fn every_read_only_state_is_the_stores_own_verdict_worded_in_its_own_refusal() {
        let fx = Fixture::build();
        for state in ReadOnly::ALL.iter().copied() {
            if !fixture::turn_read_only(&fx.store, state) {
                // Out of reach for this binary's version; the state is still
                // covered by the marker and the wording it is drawn with.
                continue;
            }
            assert_eq!(read_only(&fx.store), Some(state), "{state:?}");
            // The words the reader is told why in are the gate's own, so the
            // browser and the command line teach one rule in one sentence — and
            // the state that carries them is the state the gate named, never the
            // neighbouring one.
            let refused = fx
                .store
                .verify_mutable()
                .expect_err("the gate refuses this version")
                .to_string();
            assert_eq!(state.refusal(), refused, "{state:?}");
        }

        // And a migration that commits takes the state away again: read-only is
        // entered and left under a running browser, not settled at startup.
        fixture::turn_writable(&fx.store);
        assert_eq!(read_only(&fx.store), None);
    }

    #[test]
    fn no_two_read_only_states_are_explained_by_the_same_refusal() {
        // The gate's reasons and these states are one mapping: two states sharing
        // a refusal would be a reason the browser could not tell apart, and a
        // marker naming the wrong remedy.
        let mut worded: Vec<String> = ReadOnly::ALL
            .iter()
            .copied()
            .map(ReadOnly::refusal)
            .collect();
        worded.sort();
        worded.dedup();
        assert_eq!(worded.len(), ReadOnly::ALL.len());
    }
}
