//! The navigation model: a stack of levels, one cursor per level.
//!
//! The browser shows exactly one level at a time — the children of the deepest
//! breadcrumb entry — so the breadcrumb is the only thing that says where you
//! are, and it is never redundant with the list.
//!
//! Two rules shape this module:
//!   * a row with no children is not enterable, so every level on the stack is
//!     guaranteed non-empty and the cursor always has something to point at;
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
    /// at the roster of an empty store, since a childless row is not enterable).
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

    /// Whether the highlighted row can be entered — that is, whether it has
    /// children to show.
    pub fn can_descend(&self) -> bool {
        self.frame().current().is_some_and(|r| r.children > 0)
    }

    /// Enter the highlighted row. `load` supplies the new level's rows. A row
    /// with no children is not enterable and descending it does nothing, so a
    /// keypress can never lead to an empty level.
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
        let level = match &row.selection {
            Selection::Epic(id) => Level::Epic(id.clone()),
            Selection::Node(r) => Level::Node(r.clone()),
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

    /// Re-read every level from the store, keeping each cursor on the same
    /// selection.
    ///
    /// A level whose entity has vanished (an epic removed, a ticket reparented
    /// out from under us) is dropped along with everything below it, so the
    /// browser lands on the deepest level that still exists rather than
    /// resetting to the roster. A surviving level that has lost the row the
    /// cursor sat on falls back to the nearest position, never past the end.
    pub fn reload<F, E>(&mut self, mut load: F) -> Result<(), E>
    where
        F: FnMut(&Level) -> Result<Vec<Row>, E>,
    {
        let mut kept = 0usize;
        for depth in 0..self.stack.len() {
            let level = self.stack[depth].level.clone();
            let Ok(rows) = load(&level) else { break };
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
        Ok(())
    }
}

/// The breadcrumb text for a level entered through `row`: an epic contributes
/// its id, a node its number and name — enough to retrace the path without the
/// list that was on screen when it was entered.
fn crumb_for(row: &Row) -> String {
    match &row.selection {
        Selection::Epic(id) => id.clone(),
        Selection::Node(r) => format!("{} {}", r.number, row.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::fixture::{epic_row, node_row};

    /// Loader for a fixture store: epic `a` has nodes 1 (with child 5) and 2.
    fn load(level: &Level) -> Result<Vec<Row>, ()> {
        Ok(match level {
            Level::Epics => vec![epic_row("a", 2), epic_row("b", 0)],
            Level::Epic(id) if id == "a" => vec![node_row("a", 1, 1), node_row("a", 2, 0)],
            Level::Node(r) if r.number == 1 => vec![node_row("a", 5, 0)],
            _ => vec![],
        })
    }

    #[test]
    fn childless_row_is_not_enterable() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.cursor_down(); // epic "b", no tickets
        assert!(!nav.can_descend());
        nav.descend(load).unwrap();
        assert_eq!(nav.crumbs(), vec!["epics"]);
    }

    #[test]
    fn descending_pushes_a_crumb_per_level() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.descend(load).unwrap();
        nav.descend(load).unwrap();
        assert_eq!(nav.crumbs(), vec!["epics", "a", "1 ticket 1"]);
        assert_eq!(nav.rows().len(), 1);
    }

    #[test]
    fn ascending_returns_to_the_row_we_entered_from() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.descend(load).unwrap(); // into epic "a"
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
        })
        .unwrap();
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
        })
        .unwrap();
        assert_eq!(nav.cursor(), 0);
        assert_eq!(nav.rows().len(), 1);
    }

    #[test]
    fn reload_drops_levels_whose_rows_are_gone() {
        let mut nav = Nav::new(load(&Level::Epics).unwrap());
        nav.descend(load).unwrap(); // epic "a"
        nav.descend(load).unwrap(); // node 1
        assert_eq!(nav.crumbs().len(), 3);
        nav.reload(|level| match level {
            // Node 1's only child was deleted, so that level no longer exists.
            Level::Node(_) => Ok(vec![]),
            other => load(other),
        })
        .unwrap();
        assert_eq!(nav.crumbs(), vec!["epics", "a"]);
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
