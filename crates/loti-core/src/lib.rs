//! `loti-core` — the UI-agnostic core of `loti` (LOcal TIckets).
//!
//! This crate owns the domain model, on-disk storage, the concurrency
//! primitive and format versioning/migration. It is deliberately free of any
//! CLI/`clap` types: the `loti-cli` crate (and any future TUI/web surface) is
//! a thin adapter over this seam.
//!
//! It currently pins the store format version and the domain vocabulary, plus
//! the on-disk storage layer (layout, frontmatter/body split, tolerant
//! round-trip, store metadata, root discovery), the concurrency primitive
//! (atomic writes bracketed by a deterministic temp-file lock), the domain
//! rules and state machine the commands enforce, and the write-side business
//! operations that wire them together. Migration is layered on later.

pub mod discovery;
pub mod domain;
pub mod filter;
pub mod frontmatter;
pub mod launch;
pub mod lock;
pub mod matcher;
pub mod meta;
pub mod migrate;
pub mod model;
pub mod ops;
pub mod read;
pub mod render;
pub mod resource;
pub mod store;

/// Store `format-version` as `(major, minor)`. Written by `loti init` into
/// `<container>/meta`, and carried at store granularity. The container is the
/// only directory loti owns — it holds `meta` and every epic dir — and is
/// `.loti` by default. A store major newer than the binary is refused; an older
/// major is read-only until migrated; minor differences within a major stay
/// compatible in both directions.
pub const FORMAT_VERSION: (u32, u32) = (1, 2);

/// The five node states. `Done`/`Closed` are terminal ("resolved").
///
/// Placeholder enum establishing the `loti-core` domain seam; the full
/// type-state modelling of the state machine is not implemented yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    ToDo,
    InProgress,
    Blocked,
    Done,
    Closed,
}

impl NodeState {
    /// `done` and `closed` are the terminal ("resolved") states.
    pub fn is_terminal(self) -> bool {
        matches!(self, NodeState::Done | NodeState::Closed)
    }
}

/// The actor behind an operation. Attribution is required only on comment
/// operations; every other operation is actor-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// The single, unnamed human.
    Human,
    /// A named agent.
    Agent(String),
}

impl std::fmt::Display for Actor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Actor::Human => write!(f, "human"),
            Actor::Agent(name) => write!(f, "agent:{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states() {
        assert!(NodeState::Done.is_terminal());
        assert!(NodeState::Closed.is_terminal());
        assert!(!NodeState::ToDo.is_terminal());
        assert!(!NodeState::InProgress.is_terminal());
        assert!(!NodeState::Blocked.is_terminal());
    }

    #[test]
    fn actor_output_format() {
        assert_eq!(Actor::Human.to_string(), "human");
        assert_eq!(Actor::Agent("bot".into()).to_string(), "agent:bot");
    }
}
