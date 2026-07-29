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
                let blocking = ops::read_node(store, &blocker)?;
                out.push(Row {
                    // A blocker may live in another epic, so it carries the whole
                    // reference rather than the bare number a sibling would.
                    label: reference,
                    selection: Selection::Blocker(container.clone(), blocker),
                    // A blocker reads as a work row, glyph and all, because what
                    // it points at is work.
                    kind: RowKind::Work(blocking.frontmatter.status.wire_name().to_string()),
                    name: blocking.frontmatter.name,
                    children: 0,
                });
            }
        }
        Collection::Assets => {
            for asset in ops::list_assets(store, &target)? {
                let size = asset_size(store, container, &asset.name)?;
                out.push(Row {
                    selection: Selection::Asset(container.clone(), asset.name.clone()),
                    kind: RowKind::Member,
                    label: asset.name,
                    // A description can be long, so it is left to the preview;
                    // the size is what makes a row worth scanning.
                    name: human_size(size),
                    children: 0,
                });
            }
        }
    }
    Ok(out)
}

/// An asset's size, from the file's own metadata rather than by reading it: a
/// level of assets must not pull every payload into memory to draw its rows.
fn asset_size(store: &Store, container: &Container, name: &str) -> Result<u64> {
    let path = match container {
        Container::Epic(id) => store.epic_asset_dir(id).join(name),
        Container::Node(r) => store.node_asset_dir(&r.epic_id, r.number).join(name),
    };
    Ok(std::fs::metadata(path)
        .with_context(|| format!("asset {name} is indexed but its bytes are missing"))?
        .len())
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
            let noun = match container {
                Container::Epic(_) => "epic",
                Container::Node(_) => "ticket",
            };
            doc.push_str(&format!(
                "_Not text, so it is not shown. Write the bytes out with_\n\n```\nloti {noun} asset show {} {name}\n```\n",
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
    use loti_core::ops::{self, NewEpic, NewNode, Target};
    use loti_core::Actor;

    use super::*;

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
                    body: "epic body\n".into(),
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
}
