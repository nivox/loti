//! The `loti-core` seam: everything the browser knows about a store is loaded
//! here and nowhere else.
//!
//! Invariant: no other module in this crate touches `loti_core`. The rest of
//! the crate deals in [`Row`]s and rendered markdown, so which core call backs
//! a screen — and whether an operation reads or writes — never leaks into the
//! navigation model or the drawing code.

use anyhow::{Context, Result};
use jiff::Timestamp;
use loti_core::domain::NodeRef;
use loti_core::lock::VersionRefusal;
use loti_core::ops::{self, CommentView, Target};
use loti_core::read;
use loti_core::render;
use loti_core::store::Store;

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
}

impl Selection {
    /// How the selection is named to a reader: a reference as it is typed for an
    /// epic or a node, and otherwise the container plus the member's own id or
    /// name, which is how the CLI addresses a member too.
    ///
    /// A collection and a label name their container, because the container's
    /// document is what the preview shows for them; a blocker names the node it
    /// points at, for the same reason.
    pub fn reference(&self) -> String {
        match self {
            Selection::Epic(id) => id.clone(),
            Selection::Node(r) | Selection::Blocker(_, r) => r.to_string(),
            Selection::Collection(c, _) | Selection::Label(c, _) => c.selection().reference(),
            Selection::Comment(c, id) => format!("{} comment {id}", c.selection().reference()),
            Selection::Asset(c, name) => format!("{} asset {name}", c.selection().reference()),
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
            | Selection::Blocker(..) => None,
        }
    }
}

/// What a row stands for, which is what decides how it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// An epic or a node, carrying its state's wire name. Only a work row has a
    /// state, so a filled glyph column is itself the signal that a row is work.
    Work(String),
    /// A collection of the level's container. It is structure rather than work,
    /// and has no state to invent a glyph for.
    Collection(Collection),
    /// A member of a collection, which has no state either.
    Member,
    /// A withdrawn comment. Its text is withheld rather than destroyed — the
    /// store retains it — so the row says so in a word as well as in colour,
    /// because colour alone carries nothing in this crate.
    Withdrawn,
    /// A member the store lists but whose own data could not be read: an asset
    /// whose bytes are gone, a blocker naming a ticket that is not there.
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
            | Selection::Blocker(..) => false,
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
            for epic in read::list_epics(store)? {
                // An epic's children are its top-level tickets, not every node
                // in the epic: the count must describe what descending reveals.
                let children = read::epic_children(store, &epic.id)?.len();
                out.push(Row {
                    selection: Selection::Epic(epic.id.clone()),
                    kind: RowKind::Work(epic.status),
                    label: epic.id,
                    name: epic.name,
                    children,
                });
            }
            Ok(out)
        }
        Level::Epic(id) => {
            let container = Container::Epic(id.clone());
            let mut out = collection_rows(store, &container)?;
            out.extend(child_rows(store, read::epic_children(store, id)?)?);
            Ok(out)
        }
        Level::Node(r) => {
            let container = Container::Node(r.clone());
            let mut out = collection_rows(store, &container)?;
            out.extend(child_rows(store, read::node_children(store, r)?)?);
            Ok(out)
        }
        Level::Collection(container, kind) => member_rows(store, container, *kind),
    }
}

/// Turn a core children listing into rows, resolving each child's own child
/// count so the level can be drawn without a second pass.
fn child_rows(store: &Store, children: Vec<render::ChildRow>) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    for child in children {
        let node_ref = NodeRef::parse(&child.reference)?;
        let grandchildren = read::node_children(store, &node_ref)?.len();
        out.push(Row {
            label: node_ref.number.to_string(),
            selection: Selection::Node(node_ref),
            kind: RowKind::Work(child.status),
            name: child.name,
            children: grandchildren,
        });
    }
    Ok(out)
}

/// A container's collection rows: one per collection it carries, present
/// whether or not it has members, so there is always a row to stand on.
fn collection_rows(store: &Store, container: &Container) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    for kind in container.collections() {
        out.push(Row {
            selection: Selection::Collection(container.clone(), *kind),
            kind: RowKind::Collection(*kind),
            label: String::new(),
            name: kind.name().to_string(),
            children: collection_len(store, container, *kind)?,
        });
    }
    Ok(out)
}

/// How many members a collection has — the count on its row, which must be what
/// entering the row reveals and nothing else.
fn collection_len(store: &Store, container: &Container, kind: Collection) -> Result<usize> {
    let target = container.target();
    Ok(match kind {
        Collection::Labels => ops::list_labels(store, &target)?.len(),
        // Tombstones are listed, so they are counted: the count promises how
        // many rows are below, not how many comments can still be read.
        Collection::Comments => ops::list_comments(store, &target, true)?.len(),
        Collection::BlockedBy => match container {
            Container::Node(r) => ops::list_blocked_by(store, r)?.len(),
            // An epic carries no dependency list, so it is never offered one.
            Container::Epic(_) => 0,
        },
        Collection::Assets => ops::list_assets(store, &target)?.len(),
    })
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
                    CommentView::Live(c) => (c.id, c.author.to_string(), c.created, false),
                    CommentView::Tombstone {
                        id,
                        author,
                        created,
                    } => (id, author.to_string(), created, true),
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
                    kind: if withdrawn {
                        RowKind::Withdrawn
                    } else {
                        RowKind::Member
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
                let blocker = NodeRef::parse(&reference)?;
                // A blocker may live in another epic, so the row carries the whole
                // reference rather than the bare number a sibling would.
                let selection = Selection::Blocker(container.clone(), blocker.clone());
                out.push(match ops::read_node(store, &blocker) {
                    Ok(blocking) => Row {
                        label: reference,
                        selection,
                        // A blocker reads as a work row, glyph and all, because
                        // what it points at is work.
                        kind: RowKind::Work(blocking.frontmatter.status.wire_name().to_string()),
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

/// The word a row unreadable for any reason leads with, so one word covers every
/// corruption a member can carry rather than a reader learning one per kind.
const UNREADABLE: &str = "unreadable";

/// A member the store lists but whose own data could not be read.
///
/// The level still opens and the row still points at the member, so the reader
/// sees what the store claims to hold and can still act on the entry — a dangling
/// index entry is a thing to delete, which takes a row to stand on.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp(pub Timestamp);

/// One entity as an editing surface starts from it: the fields a surface can
/// replace, plus the stamp they were read at.
///
/// The fields are the free-form replacements — a whole `name`, `summary` or
/// `body` — which are exactly the writes that can silently discard someone
/// else's text, so they are the writes that carry [`Stamp`] as a precondition.
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
        | Selection::Blocker(..) => {
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
    /// Replace the whole body of the epic or node the row names.
    ///
    /// A whole-field replacement, so it is the one write here that can silently
    /// discard text somebody else wrote — which is what the stamp is for.
    SetBody {
        /// The epic or node whose body is replaced.
        target: Selection,
        /// The replacement, exactly as the reader left it: what makes a body
        /// acceptable is the store's rule, and the browser normalises none of it.
        body: String,
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
            Write::AddLabel(target, _)
            | Write::RemoveLabel(target)
            | Write::AddBlocker(target, _)
            | Write::RemoveBlocker(target)
            | Write::DeleteAsset(target)
            | Write::SetBody { target, .. } => target,
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
            Write::SetBody { target, body, .. } => Write::SetBody {
                target: target.clone(),
                body: body.clone(),
                expect: None,
            },
            other => other.clone(),
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
    /// Every other refusal, in the store's own words, so the browser and the
    /// command line teach the same rule in the same words.
    Rule(String),
}

/// Carry out a write, returning the store's own refusal when it refuses.
///
/// The browser never judges a write itself: only the store can, so the action is
/// offered, attempted, and whatever comes back is shown — which is why nothing
/// here pre-checks a store rule.
pub fn perform(store: &Store, write: &Write) -> Result<(), Refusal> {
    match write {
        Write::AddLabel(selection, label) => add_label(store, selection, label),
        Write::RemoveLabel(selection) => remove_label(store, selection),
        Write::AddBlocker(selection, reference) => add_blocker(store, selection, reference),
        Write::RemoveBlocker(selection) => remove_blocker(store, selection),
        Write::DeleteAsset(selection) => delete_asset(store, selection),
        Write::SetBody {
            target,
            body,
            expect,
        } => set_body(store, target, body, *expect),
    }
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
        other => Refusal::Rule(other.to_string()),
    }
}

/// A refusal the browser itself makes: a write aimed at a row that cannot take
/// it, which is a caller that has lost track of what its row points at rather
/// than anything the store was asked.
fn misdirected(message: String) -> Refusal {
    Refusal::Rule(message)
}

/// Replace an epic's or a node's body with the text the reader left, applying
/// only while the entity still carries the stamp that text was read at.
///
/// This is the one write the browser makes that replaces a whole field, so it is
/// the one that can discard text somebody else wrote: the stamp is the store's
/// precondition, checked under the lock, and a mismatch writes nothing. Whether
/// the body is acceptable at all is the store's rule; the text is written exactly
/// as the reader left it, trailing newline and all, as the command line writes
/// what it is given.
fn set_body(
    store: &Store,
    selection: &Selection,
    body: &str,
    expect: Option<Stamp>,
) -> Result<(), Refusal> {
    let expect_updated = expect.map(|stamp| stamp.0);
    match selection {
        Selection::Epic(id) => ops::edit_epic(
            store,
            id,
            ops::EpicEdits {
                body: Some(body.to_string()),
                expect_updated,
                ..Default::default()
            },
        )
        .map(|_| ())
        .map_err(refusal),
        Selection::Node(r) => ops::edit_node(
            store,
            r,
            ops::NodeEdits {
                body: Some(body.to_string()),
                expect_updated,
                ..Default::default()
            },
        )
        .map(|_| ())
        .map_err(refusal),
        // Only an epic and a node have a body of their own, and only their rows
        // offer the action, so any other selection is a caller that has lost track
        // of what its row points at.
        Selection::Collection(..)
        | Selection::Label(..)
        | Selection::Comment(..)
        | Selection::Asset(..)
        | Selection::Blocker(..) => Err(misdirected(format!(
            "{} has no body of its own",
            selection.reference()
        ))),
    }
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
fn add_blocker(store: &Store, selection: &Selection, reference: &str) -> Result<(), Refusal> {
    let (node, blocker) = blocked_and_blocking(selection, reference).map_err(misdirected)?;
    ops::add_blocked_by(store, &node, std::slice::from_ref(&blocker)).map_err(refusal)?;
    Ok(())
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
/// in that document's own metadata table. A comment and an asset get a document
/// composed here, because no other surface needs one.
pub fn preview(store: &Store, selection: &Selection) -> Result<String> {
    let (value, children, comments) = match selection {
        Selection::Epic(id) => (
            read::epic_json(store, id)?,
            read::epic_children(store, id)?,
            read::comment_lines(store, &Target::Epic(id.clone()), false)?,
        ),
        Selection::Node(r) => (
            read::node_json(store, r)?,
            read::node_children(store, r)?,
            read::comment_lines(store, &Target::Node(r.clone()), false)?,
        ),
        Selection::Collection(container, _) | Selection::Label(container, _) => {
            return preview(store, &container.selection())
        }
        Selection::Comment(container, id) => return comment_document(store, container, *id),
        Selection::Asset(container, name) => return asset_document(store, container, name),
        // A blocker's own document, so what blocks you is readable without
        // leaving the level.
        Selection::Blocker(_, r) => (
            read::node_json(store, r)?,
            read::node_children(store, r)?,
            read::comment_lines(store, &Target::Node(r.clone()), false)?,
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

        /// The epic as a selection, which is how a surface addresses it.
        pub(crate) fn epic_selection(&self) -> Selection {
            Selection::Epic(self.epic.clone())
        }

        /// The epic's body as the store holds it, so a test about a buffer opened
        /// on stored text asserts against the store rather than against a constant
        /// that a richer fixture would leave behind.
        pub(crate) fn epic_body(&self) -> String {
            ops::read_epic(&self.store, &self.epic).unwrap().body
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
            kind: RowKind::Work("open".to_string()),
            label: id.to_string(),
            name: format!("the {id} epic"),
            children,
        }
    }

    /// A node row; see [`epic_row`].
    pub(crate) fn node_row(epic: &str, number: u64, children: usize) -> Row {
        Row {
            selection: Selection::Node(NodeRef::new(epic, number)),
            kind: RowKind::Work("to-do".to_string()),
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
    use loti_core::ops::{EpicEdits, NodeEdits};

    /// The words a refusal is shown in.
    ///
    /// A conflict has none: it is the one refusal the reader is asked about rather
    /// than told, in the browser's own words, so a test reading a refusal's words
    /// must not silently accept one in its place.
    fn refusal_words(refused: Refusal) -> String {
        match refused {
            Refusal::Rule(words) => words,
            Refusal::Conflict => panic!("refused as a conflict rather than by a rule"),
        }
    }

    /// The work rows of a level, dropping the collection rows every epic and node
    /// level leads with.
    fn work_rows(rows: &[Row]) -> Vec<Row> {
        rows.iter()
            .filter(|r| matches!(r.kind, RowKind::Work(_)))
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
            .all(|r| !matches!(r.kind, RowKind::Work(_))));

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

        for kind in container.collections() {
            let level = Level::Collection(container.clone(), *kind);
            let members = rows(store, &level).unwrap();
            // The count on the collection row is exactly what entering it shows.
            assert_eq!(
                members.len(),
                collection_len(store, &container, *kind).unwrap()
            );
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
                kind: RowKind::Work(stored.status.wire_name().to_string()),
                // A blocker may live in another epic, so a bare number would not
                // address it.
                label: fx.blocker.to_string(),
                name: stored.name,
                children: 0,
            }]
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
        perform(&fx.store, &Write::AddBlocker(list.clone(), bare)).unwrap();
        let mut expected = before.clone();
        expected.push(whole.clone());
        assert_eq!(fx.node_blockers(), expected);

        // The same reference written whole is the same entry, so the store's own
        // no-op is what happens — not a refusal the browser invents on its behalf.
        perform(&fx.store, &Write::AddBlocker(list.clone(), whole.clone())).unwrap();
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
        // the same precondition refuses.
        assert!(ops::edit_epic(
            store,
            &fx.epic,
            EpicEdits {
                body: Some("again\n".into()),
                expect_updated: Some(target.stamp.0),
                ..Default::default()
            },
        )
        .is_err());
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

        assert!(ops::edit_node(
            store,
            r,
            NodeEdits {
                body: Some("again\n".into()),
                expect_updated: Some(target.stamp.0),
                ..Default::default()
            },
        )
        .is_err());
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
            &Write::SetBody {
                target: fx.epic_selection(),
                body: written.to_string(),
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
            &Write::SetBody {
                target: node,
                body: "the ticket's own\n".to_string(),
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
    fn a_body_write_naming_a_stamp_the_entity_has_moved_past_is_refused_as_a_conflict() {
        let fx = Fixture::build();
        let stamp = edit_target(&fx.store, &fx.epic_selection()).unwrap().stamp;

        // Somebody else writes while the reader is composing theirs, which is the
        // one window no lock can cover: the stamp is what closes it.
        fx.rewrite_the_epics_body("theirs\n");
        let refused = perform(
            &fx.store,
            &Write::SetBody {
                target: fx.epic_selection(),
                body: "mine\n".to_string(),
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
            &Write::SetBody {
                target: fx.epic_selection(),
                body: "mine\n".to_string(),
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
                &Write::SetBody {
                    target: fx.epic_selection(),
                    body: "mine\n".to_string(),
                    expect: Some(stamp),
                },
            ),
            Err(Refusal::Conflict)
        );
        assert_eq!(fx.epic_body(), before);
    }

    #[test]
    fn only_an_epic_or_a_node_has_a_body_to_write() {
        let fx = Fixture::build();
        let before = fx.epic_body();
        // A collection and its members are edited by their own operations, so any
        // other selection is a caller that has lost track of what its row points
        // at — refused by name, with nothing written on the way to refusing.
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
        ] {
            let err = refusal_words(
                perform(
                    &fx.store,
                    &Write::SetBody {
                        target: wrong.clone(),
                        body: "nowhere\n".to_string(),
                        expect: None,
                    },
                )
                .expect_err("only an epic or a node has a body"),
            );
            assert!(err.contains(&fx.epic), "{wrong:?}: {err}");
            assert_eq!(fx.epic_body(), before, "{wrong:?} wrote something");
        }
    }

    #[test]
    fn every_write_says_what_it_is_aimed_at_and_only_a_stamped_one_drops_a_precondition() {
        let fx = Fixture::build();
        let stamp = edit_target(&fx.store, &fx.epic_selection()).unwrap().stamp;
        let body = Write::SetBody {
            target: fx.epic_selection(),
            body: "mine\n".to_string(),
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
            body.clone(),
        ] {
            // Dropping the precondition changes the precondition and nothing else:
            // a write that came back aimed somewhere else would overwrite the
            // wrong entity with the reader's text.
            assert_eq!(write.overwriting().target(), write.target(), "{write:?}");
        }
        // The stamped write loses its stamp, and a write that never carried one is
        // unchanged: it cannot conflict, so there is nothing to drop.
        assert_eq!(
            body.overwriting(),
            Write::SetBody {
                target: fx.epic_selection(),
                body: "mine\n".to_string(),
                expect: None,
            }
        );
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
