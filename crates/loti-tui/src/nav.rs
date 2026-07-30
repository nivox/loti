//! The navigation model: a stack of levels, one cursor per level.
//!
//! The browser shows exactly one level at a time — the children of the deepest
//! breadcrumb entry — so the breadcrumb is the only thing that says where you
//! are, and it is never redundant with the list.
//!
//! Two rules shape this module:
//!   * only an enterable row opens a level, and every level a row opens has at
//!     least one row of its own — an epic's or a node's collections are always
//!     listed, and a collection is enterable only when it has members — so every
//!     level on the stack is non-empty and the cursor always has something to
//!     point at;
//!   * a cursor is remembered by its row's selection, not its index, so a
//!     reload that adds or removes siblings leaves the highlight on the same
//!     ticket rather than on whatever slid into that position.

use crate::data::{Level, Row, Selection};

/// One level on the stack: what it lists, how it is named in the breadcrumb,
/// its rows, and where its cursor sits.
#[derive(Debug, Clone)]
pub struct Frame {
    /// The level this frame lists the children of.
    pub level: Level,
    /// The breadcrumb text for this level.
    pub crumb: String,
    /// The rows of this level.
    pub rows: Vec<Row>,
    /// Index into `rows`; always in range while `rows` is non-empty.
    pub cursor: usize,
}

impl Frame {
    /// The highlighted row, or `None` for a level with no rows (only possible
    /// at the roster of an empty store: every other level is entered through a
    /// row that guarantees it has something in it).
    pub fn current(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }
}

/// The whole browser position: a non-empty stack whose first frame is always
/// the epic roster.
#[derive(Debug, Clone)]
pub struct Nav {
    stack: Vec<Frame>,
}

impl Nav {
    /// Start at the epic roster with the given rows.
    pub fn new(rows: Vec<Row>) -> Self {
        Self {
            stack: vec![Frame {
                level: Level::Epics,
                crumb: "epics".to_string(),
                rows,
                cursor: 0,
            }],
        }
    }

    /// The level currently on screen.
    pub fn frame(&self) -> &Frame {
        // The stack is seeded with the roster and `ascend` never pops it.
        self.stack.last().expect("the roster frame is never popped")
    }

    fn frame_mut(&mut self) -> &mut Frame {
        self.stack
            .last_mut()
            .expect("the roster frame is never popped")
    }

    /// The rows on screen.
    pub fn rows(&self) -> &[Row] {
        &self.frame().rows
    }

    /// The cursor index on screen.
    pub fn cursor(&self) -> usize {
        self.frame().cursor
    }

    /// What the preview shows: the highlighted row, or — at a level with no
    /// rows — the level's own entity, so the pane still describes where you are.
    pub fn preview_target(&self) -> Option<Selection> {
        match self.frame().current() {
            Some(row) => Some(row.selection.clone()),
            None => self.frame().level.selection(),
        }
    }

    /// The breadcrumb, outermost first.
    pub fn crumbs(&self) -> Vec<&str> {
        self.stack.iter().map(|f| f.crumb.as_str()).collect()
    }

    /// Move the cursor down one row, stopping at the last.
    pub fn cursor_down(&mut self) {
        let last = self.frame().rows.len().saturating_sub(1);
        let f = self.frame_mut();
        f.cursor = (f.cursor + 1).min(last);
    }

    /// Move the cursor up one row, stopping at the first.
    pub fn cursor_up(&mut self) {
        let f = self.frame_mut();
        f.cursor = f.cursor.saturating_sub(1);
    }

    /// Move the cursor to the first row.
    pub fn cursor_first(&mut self) {
        self.frame_mut().cursor = 0;
    }

    /// Move the cursor to the last row.
    pub fn cursor_last(&mut self) {
        let last = self.frame().rows.len().saturating_sub(1);
        self.frame_mut().cursor = last;
    }

    /// Whether the highlighted row can be entered.
    pub fn can_descend(&self) -> bool {
        self.frame().current().is_some_and(Row::enterable)
    }

    /// Enter the highlighted row. `load` supplies the new level's rows. A row
    /// that is not enterable does nothing, so a keypress can never lead to an
    /// empty level.
    pub fn descend<F, E>(&mut self, load: F) -> Result<(), E>
    where
        F: FnOnce(&Level) -> Result<Vec<Row>, E>,
    {
        if !self.can_descend() {
            return Ok(());
        }
        let row = self
            .frame()
            .current()
            .expect("can_descend implies a highlighted row")
            .clone();
        let Some(level) = row.selection.level() else {
            // A leaf has no level, and `can_descend` has already refused it.
            return Ok(());
        };
        let rows = load(&level)?;
        let crumb = crumb_for(&row);
        self.stack.push(Frame {
            level,
            crumb,
            rows,
            cursor: 0,
        });
        Ok(())
    }

    /// Leave the current level for its parent. The parent frame kept its cursor,
    /// so you return to the row you entered from. At the roster this does
    /// nothing — there is no level above the epics.
    pub fn ascend(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    /// Whether there is a level above the current one.
    pub fn can_ascend(&self) -> bool {
        self.stack.len() > 1
    }

    /// Whether the level on screen is a collection's members. Structure rather
    /// than work, which is what its breadcrumb entry has to say.
    pub fn at_collection(&self) -> bool {
        matches!(self.frame().level, Level::Collection(..))
    }

    /// Re-read every level from the store, keeping each cursor on the same
    /// selection.
    ///
    /// A level whose entity has vanished (an epic removed, a ticket reparented
    /// out from under us) is dropped along with everything below it, so the
    /// browser lands on the deepest level that still exists rather than
    /// resetting to the roster. A surviving level that has lost the row the
    /// cursor sat on falls back to the nearest position, never past the end.
    ///
    /// Re-reading cannot fail the position: a level the store could not be read
    /// for keeps the rows it has, because a failure to read is not evidence that
    /// the level is gone — only an empty listing is. That is also what makes a
    /// reload safe to run after a write has already committed: there is no outcome
    /// here that could keep the reader from being told what the write did.
    pub fn reload<F, E>(&mut self, mut load: F)
    where
        F: FnMut(&Level) -> Result<Vec<Row>, E>,
    {
        let mut kept = 0usize;
        for depth in 0..self.stack.len() {
            let level = self.stack[depth].level.clone();
            let Ok(rows) = load(&level) else {
                // Keep this level as it stands, and stop: what is below it was
                // read through rows this reload could not confirm.
                kept = depth;
                break;
            };
            // A level below the roster only exists because a parent row had
            // children; if it now has none, the level itself is gone.
            if rows.is_empty() && depth > 0 {
                break;
            }
            let frame = &mut self.stack[depth];
            let previous = frame.current().map(|r| r.selection.clone());
            frame.cursor = match previous {
                Some(selection) => rows
                    .iter()
                    .position(|r| r.selection == selection)
                    .unwrap_or_else(|| frame.cursor.min(rows.len().saturating_sub(1))),
                None => 0,
            };
            frame.rows = rows;
            kept = depth;
        }
        self.stack.truncate(kept + 1);
    }
}

/// The breadcrumb text for a level entered through `row`: an epic contributes
/// its id, a node its number and name — enough to retrace the path without the
/// list that was on screen when it was entered — and a collection its own name,
/// which is as deep as a path ever goes.
fn crumb_for(row: &Row) -> String {
    match &row.selection {
        Selection::Epic(id) => id.clone(),
        Selection::Node(r) => format!("{} {}", r.number, row.name),
        Selection::Collection(_, kind) => kind.name().to_string(),
        // A member is a leaf, so it is never entered and never becomes a crumb;
        // the arm exists only because the mapping has to be total.
        Selection::Label(..)
        | Selection::Comment(..)
        | Selection::Asset(..)
        | Selection::Blocker(..) => row.label.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::fixture::{collection_row, epic_row, label_row, node_row};
    use crate::data::{Collection, Container};

    /// Loader for a fixture store: epic `a` has nodes 1 (with child 5) and 2,
    /// and every epic and node level leads with its collection rows.
    fn load(level: &Level) -> Result<Vec<Row>, ()> {
        let container = |c: &Container, populated: bool| {
            c.collections()
                .iter()
                .map(|kind| {
                    // Only `labels` has members, so one level carries both an
                    // enterable collection and empty ones.
                    let members = usize::from(populated && *kind == Collection::Labels);
                    collection_row(c.clone(), *kind, members)
                })
                .collect::<Vec<_>>()
        };
        Ok(match level {
            Level::Epics => vec![epic_row("a", 2), epic_row("b", 0)],
            Level::Epic(id) => {
                let mut rows = container(&Container::Epic(id.clone()), id == "a");
                if id == "a" {
                    rows.extend([node_row("a", 1, 1), node_row("a", 2, 0)]);
                }
                rows
            }
            Level::Node(r) => {
                let mut rows = container(&Container::Node(r.clone()), false);
                if r.number == 1 {
                    rows.push(node_row("a", 5, 0));
                }
                rows
            }
            Level::Collection(c, Collection::Labels) => vec![label_row(c.clone(), "ui")],
            Level::Collection(..) => vec![],
        })
    }

    /// The rows of the level on screen that are work rather than structure.
    fn work_rows(nav: &Nav) -> Vec<String> {
        nav.rows()
            .iter()
            .filter(|r| matches!(r.selection, Selection::Epic(_) | Selection::Node(_)))
            .map(|r| r.label.clone())
            .collect()
    }

    /// Put the cursor on the first work row of the level on screen.
    fn to_first_work_row(nav: &mut Nav) {
        let index = nav
            .rows()
            .iter()
            .position(|r| matches!(r.selection, Selection::Epic(_) | Selection::Node(_)))
            .expect("the level has a work row");
        nav.cursor_first();
        for _ in 0..index {
            nav.cursor_down();
        }
    }

    #[test]
    fn an_epic_with_no_tickets_is_still_enterable() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.cursor_down(); // epic "b", no tickets
                           // Its collections are rows there whatever it holds, so a missing child
                           // count means "no tickets", not "nothing below".
        assert!(nav.can_descend());
        nav.descend(load).unwrap();
        assert_eq!(nav.crumbs(), vec!["epics", "b"]);
        assert!(work_rows(&nav).is_empty());
        assert_eq!(
            nav.rows().len(),
            Container::Epic("b".into()).collections().len()
        );
    }

    #[test]
    fn a_collection_with_no_members_is_not_enterable() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.descend(load).unwrap(); // into epic "a"
        nav.cursor_down(); // `comments`, which is empty
        assert!(!nav.can_descend());
        nav.descend(load).unwrap();
        assert_eq!(nav.crumbs(), vec!["epics", "a"]);
    }

    #[test]
    fn a_collection_member_is_a_leaf() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.descend(load).unwrap(); // into epic "a"
        nav.descend(load).unwrap(); // into `labels`, its one populated collection
        assert_eq!(nav.crumbs(), vec!["epics", "a", "labels"]);
        assert!(nav.at_collection());
        assert!(!nav.can_descend());
        nav.descend(load).unwrap();
        assert_eq!(nav.crumbs().len(), 3, "a member must not become a crumb");
    }

    #[test]
    fn descending_pushes_a_crumb_per_level() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.descend(load).unwrap();
        to_first_work_row(&mut nav);
        nav.descend(load).unwrap();
        assert_eq!(nav.crumbs(), vec!["epics", "a", "1 ticket 1"]);
        assert_eq!(work_rows(&nav), vec!["5".to_string()]);
        assert!(!nav.at_collection());
    }

    #[test]
    fn ascending_returns_to_the_row_we_entered_from() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.descend(load).unwrap(); // into epic "a"
        to_first_work_row(&mut nav); // node 1
        nav.cursor_down(); // node 2
        nav.cursor_up(); // node 1
        nav.descend(load).unwrap(); // into node 1
        nav.ascend();
        assert_eq!(nav.crumbs(), vec!["epics", "a"]);
        assert_eq!(
            nav.frame().current().map(|r| r.label.clone()),
            Some("1".to_string())
        );
    }

    #[test]
    fn the_roster_is_never_popped() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        assert!(!nav.can_ascend());
        nav.ascend();
        assert_eq!(nav.crumbs(), vec!["epics"]);
    }

    #[test]
    fn reload_keeps_the_cursor_on_its_own_row_when_a_sibling_appears() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.cursor_down(); // epic "b"
        nav.reload(|level| match level {
            // A new epic sorts ahead of "b" and would shift it by index.
            Level::Epics => Ok(vec![epic_row("a", 2), epic_row("aa", 0), epic_row("b", 0)]),
            other => load(other),
        });
        assert_eq!(
            nav.frame().current().map(|r| r.label.clone()),
            Some("b".to_string())
        );
    }

    #[test]
    fn reload_falls_back_to_the_nearest_row_when_the_cursor_row_is_gone() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.cursor_down(); // epic "b"
        nav.reload(|level| match level {
            Level::Epics => Ok(vec![epic_row("a", 2)]),
            other => load(other),
        });
        assert_eq!(nav.cursor(), 0);
        assert_eq!(nav.rows().len(), 1);
    }

    #[test]
    fn reload_drops_levels_whose_rows_are_gone() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.descend(load).unwrap(); // epic "a"
        nav.descend(load).unwrap(); // its `labels`
        assert_eq!(nav.crumbs().len(), 3);
        nav.reload(|level| match level {
            // The last label was removed, so that level no longer exists. Only a
            // collection level can vanish this way: an epic's and a node's own
            // level always keeps its collection rows.
            Level::Collection(..) => Ok(vec![]),
            other => load(other),
        });
        assert_eq!(nav.crumbs(), vec!["epics", "a"]);
    }

    #[test]
    fn reload_keeps_a_level_it_could_not_re_read_rather_than_dropping_it() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.descend(load).unwrap(); // epic "a"
        nav.descend(load).unwrap(); // its `labels`
        let before: Vec<String> = nav.rows().iter().map(|r| r.label.clone()).collect();

        nav.reload(|level| match level {
            // "I could not read it" is not the answer "it has no members": a
            // failure is no evidence the level is gone, so dropping it — as an
            // emptied level is dropped — would claim something the store did not
            // say, and yank the reader up a level over a store it cannot read.
            Level::Collection(..) => Err(()),
            other => load(other),
        });

        assert_eq!(nav.crumbs(), vec!["epics", "a", "labels"]);
        assert_eq!(
            nav.rows()
                .iter()
                .map(|r| r.label.clone())
                .collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn preview_follows_the_cursor_and_falls_back_to_the_level_itself() {
        let nav = Nav::new(load(&Level::Epics).unwrap());
        assert_eq!(nav.preview_target(), Some(Selection::Epic("a".to_string())));
        let mut empty = Nav::new(vec![]);
        assert_eq!(empty.preview_target(), None);
        empty.cursor_down();
        assert_eq!(empty.cursor(), 0);
    }

    #[test]
    fn cursor_stops_at_both_ends() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.cursor_up();
        assert_eq!(nav.cursor(), 0);
        nav.cursor_last();
        assert_eq!(nav.cursor(), 1);
        nav.cursor_down();
        assert_eq!(nav.cursor(), 1);
        nav.cursor_first();
        assert_eq!(nav.cursor(), 0);
    }
}
