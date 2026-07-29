//! The `loti-core` seam: everything the browser knows about a store is loaded
//! here and nowhere else.
//!
//! Invariant: no other module in this crate touches `loti_core`. The rest of
//! the crate deals in [`Row`]s and rendered markdown, so which core call backs
//! a screen — and whether an operation reads or writes — never leaks into the
//! navigation model or the drawing code.

use anyhow::Result;
use jiff::Timestamp;
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
pub fn edit_target(store: &Store, selection: &Selection) -> Result<EditTarget> {
    let (name, summary, body, updated) = match selection {
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
            label: id.to_string(),
            name: format!("the {id} epic"),
            status: "open".to_string(),
            children,
        }
    }

    /// A node row; see [`epic_row`].
    pub(crate) fn node_row(epic: &str, number: u64, children: usize) -> Row {
        Row {
            selection: Selection::Node(NodeRef::new(epic, number)),
            label: number.to_string(),
            name: format!("ticket {number}"),
            status: "to-do".to_string(),
            children,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::Fixture;
    use super::*;
    use loti_core::ops::{self, EpicEdits, NodeEdits};

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
        let tickets = rows(&fx.store, &Level::Epic(fx.epic.clone())).unwrap();
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
        // Only the ticket with the subticket is enterable, which is what lets a
        // test exercise both a level with something under it and a leaf.
        assert_eq!((tickets[0].children, tickets[1].children), (1, 0));

        let subtickets = rows(&fx.store, &Level::Node(fx.node.clone())).unwrap();
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
