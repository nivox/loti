//! The `loti-core` seam: everything the browser knows about a store is loaded
//! here and nowhere else.
//!
//! Invariant: no other module in this crate touches `loti_core`. The rest of
//! the crate deals in [`Row`]s and rendered markdown, so which core call backs
//! a screen — and whether an operation reads or writes — never leaks into the
//! navigation model or the drawing code.

use anyhow::Result;
use loti_core::domain::NodeRef;
use loti_core::ops::Target;
use loti_core::read;
use loti_core::render;
use loti_core::store::Store;

/// What a navigation row points at: an epic, or a node at any depth. This is
/// the target every operation on the highlighted row needs, so it is carried
/// as one value rather than re-derived per operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// An epic, addressed by its id.
    Epic(String),
    /// A node, addressed by `<epic-id>/<number>`.
    Node(NodeRef),
}

impl Selection {
    /// The reference as a user types it: an epic id, or `<epic-id>/<n>`.
    pub fn reference(&self) -> String {
        match self {
            Selection::Epic(id) => id.clone(),
            Selection::Node(r) => r.to_string(),
        }
    }
}

/// One row of the navigation pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What the row points at, and the key the cursor is tracked by.
    pub selection: Selection,
    /// The left-hand identifier column: an epic id, or a bare node number.
    /// A node's epic is already named in the breadcrumb, so the number alone
    /// addresses it unambiguously within a level.
    pub label: String,
    /// The one-line name.
    pub name: String,
    /// The state's wire name, as the shared palette keys on.
    pub status: String,
    /// How many direct children the row has. Zero means the row is a leaf, and
    /// leaves are not enterable — the count is the only affordance telling a
    /// reader whether descending will do anything.
    pub children: usize,
}

/// Which level of the browser a listing came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    /// The epic roster — the root level, which always exists.
    Epics,
    /// An epic's top-level tickets.
    Epic(String),
    /// A node's direct subtickets.
    Node(NodeRef),
}

impl Level {
    /// The level's own entity, or `None` for the roster, which has no entity.
    pub fn selection(&self) -> Option<Selection> {
        match self {
            Level::Epics => None,
            Level::Epic(id) => Some(Selection::Epic(id.clone())),
            Level::Node(r) => Some(Selection::Node(r.clone())),
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
/// epics by id, siblings by ascending number. Ordering is not re-decided here —
/// a level browsed in the UI and the same level printed by `list` must agree.
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
                    label: epic.id,
                    name: epic.name,
                    status: epic.status,
                    children,
                });
            }
            Ok(out)
        }
        Level::Epic(id) => child_rows(store, read::epic_children(store, id)?),
        Level::Node(r) => child_rows(store, read::node_children(store, r)?),
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
            name: child.name,
            status: child.status,
            children: grandchildren,
        });
    }
    Ok(out)
}

/// The preview body for a selection: byte-for-byte the markdown
/// `loti epic show` / `loti ticket show` print. The preview is that command's
/// output, so there is no second document shape to keep in sync with it.
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
    };
    Ok(render::show_markdown(&value, &children, &comments))
}
