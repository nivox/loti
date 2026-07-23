//! The structured filter families for the roster reader.
//!
//! Four filter families narrow a scoped candidate set: scope, label, state, and
//! match. They combine with a logical AND across families — a node survives only
//! if it satisfies every family that was requested. The structured families
//! (label and state) are pure predicates evaluated here; the match family runs
//! an external process over the survivors and lives in [`crate::matcher`].
//!
//! Family semantics enforced here:
//!
//!   * **Labels.** Each `--label` occurrence is an AND term; commas within one
//!     occurrence are an OR-group. So two occurrences `a` and `b,c` mean
//!     `a AND (b OR c)`. `--not-label` means "has none of" — the node is
//!     rejected if it carries any excluded label (comma and repeat both union).
//!   * **State.** A positive state selector is an OR set of states, and it may
//!     be given only once (states are mutually exclusive as a positive filter,
//!     so repeating the selector is a usage error). `--not-status` is the
//!     symmetric exclusion. The `open` and `resolved` aggregators are shorthand
//!     positive selectors and are mutually exclusive with each other and with an
//!     explicit state selector.
//!
//! The label and state predicates are total functions over a node's own fields,
//! so they are order-independent and directly unit-testable without any store.

use crate::model::NodeFile;
use crate::NodeState;

/// A parsed, validated set of structured filters (label and state families).
/// Scope is resolved before this, and match runs after it; this value carries
/// only the two families evaluated as pure predicates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuredFilters {
    /// AND of OR-groups: the node must satisfy every group, and satisfies a
    /// group by carrying at least one of its labels. An empty list is no
    /// constraint.
    pub label_and_groups: Vec<Vec<String>>,
    /// The node is rejected if it carries any of these labels ("has none of").
    /// An empty set is no constraint.
    pub not_labels: Vec<String>,
    /// Positive state set: the node's state must be one of these. `None` is no
    /// constraint; `Some(set)` requires membership.
    pub states: Option<Vec<NodeState>>,
    /// Excluded state set: the node's state must not be one of these. An empty
    /// set is no constraint.
    pub not_states: Vec<NodeState>,
}

/// Why a set of filter flags is not a usable request. These are usage errors —
/// the combination of flags is contradictory or malformed — surfaced before any
/// store access so the message can point straight at the offending flags.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    /// A positive state selector was given more than once. States are mutually
    /// exclusive as a positive filter, so several are expressed as one
    /// comma-separated selector, not by repeating the flag.
    #[error(
        "the state filter may be given once with a comma-separated list; \
         repeating it is not allowed because states are mutually exclusive"
    )]
    RepeatedStateSelector,
    /// More than one of the mutually-exclusive positive state selectors was
    /// given (an explicit state set, the open aggregator, the resolved
    /// aggregator).
    #[error(
        "choose only one positive state filter: an explicit state list, \
         the open aggregator, or the resolved aggregator"
    )]
    ConflictingStateSelectors,
    /// A state name was not one of the recognised states.
    #[error(
        "unknown state '{0}': expected one of to-do, in-progress, blocked, \
         done, closed"
    )]
    UnknownState(String),
    /// An empty state name (e.g. a stray comma) was given.
    #[error("a state filter entry is empty")]
    EmptyState,
    /// An empty label name (e.g. a stray comma) was given.
    #[error("a label filter entry is empty")]
    EmptyLabel,
}

/// The raw label/state flag inputs, before validation. This mirrors the shape a
/// CLI exposes (repeatable strings, comma-joined groups, two aggregator
/// booleans) so the adapter passes flags straight through and this module owns
/// the parsing and the conflict rules.
#[derive(Debug, Clone, Default)]
pub struct FilterInput {
    /// Each element is one `--label` occurrence, its commas an OR-group.
    pub labels: Vec<String>,
    /// Each element is one `--not-label` occurrence; commas union too.
    pub not_labels: Vec<String>,
    /// Each element is one `--status` occurrence; commas are an OR list. More
    /// than one occurrence is a usage error.
    pub states: Vec<String>,
    /// Each element is one `--not-status` occurrence; commas union.
    pub not_states: Vec<String>,
    /// The `open` aggregator: to-do, in-progress, or blocked.
    pub open: bool,
    /// The `resolved` aggregator: done or closed.
    pub resolved: bool,
}

/// The three non-terminal ("open") states the open aggregator expands to.
const OPEN_STATES: [NodeState; 3] = [NodeState::ToDo, NodeState::InProgress, NodeState::Blocked];

/// The two terminal ("resolved") states the resolved aggregator expands to.
const RESOLVED_STATES: [NodeState; 2] = [NodeState::Done, NodeState::Closed];

/// Parse one state name in filter vocabulary. Rejects the empty string so a
/// stray comma is a clear error rather than a silent no-op.
fn parse_state(name: &str) -> Result<NodeState, FilterError> {
    if name.is_empty() {
        return Err(FilterError::EmptyState);
    }
    NodeState::from_wire(name).ok_or_else(|| FilterError::UnknownState(name.to_string()))
}

/// Split one comma-joined occurrence into its trimmed, non-empty parts.
/// Whitespace around a part is ignored so `a, b` and `a,b` behave the same.
fn split_group(occurrence: &str) -> Vec<String> {
    occurrence
        .split(',')
        .map(|s| s.trim().to_string())
        .collect()
}

/// Validate and normalise raw filter flags into a [`StructuredFilters`],
/// enforcing every cross-flag conflict rule up front.
pub fn parse_filters(input: &FilterInput) -> Result<StructuredFilters, FilterError> {
    // Labels: one AND-group per occurrence, its commas an OR set. An empty part
    // (stray comma) is rejected so the request is unambiguous.
    let mut label_and_groups = Vec::new();
    for occurrence in &input.labels {
        let group = split_group(occurrence);
        if group.iter().any(|l| l.is_empty()) {
            return Err(FilterError::EmptyLabel);
        }
        label_and_groups.push(group);
    }

    // Excluded labels union across commas and repeats — comma and repeat
    // coincide for exclusion, so they flatten into one set.
    let mut not_labels = Vec::new();
    for occurrence in &input.not_labels {
        for label in split_group(occurrence) {
            if label.is_empty() {
                return Err(FilterError::EmptyLabel);
            }
            if !not_labels.contains(&label) {
                not_labels.push(label);
            }
        }
    }

    // Positive state selectors are mutually exclusive: an explicit list, the
    // open aggregator, and the resolved aggregator are three ways to say the
    // same kind of thing, so at most one may appear.
    let positive_selectors =
        (!input.states.is_empty()) as u8 + input.open as u8 + input.resolved as u8;
    if positive_selectors > 1 {
        return Err(FilterError::ConflictingStateSelectors);
    }

    // A positive state filter may be given only once; several states go in one
    // comma-separated selector.
    if input.states.len() > 1 {
        return Err(FilterError::RepeatedStateSelector);
    }

    let states = if input.open {
        Some(OPEN_STATES.to_vec())
    } else if input.resolved {
        Some(RESOLVED_STATES.to_vec())
    } else if let Some(occurrence) = input.states.first() {
        let mut set = Vec::new();
        for name in split_group(occurrence) {
            let state = parse_state(&name)?;
            if !set.contains(&state) {
                set.push(state);
            }
        }
        Some(set)
    } else {
        None
    };

    // Excluded states union across commas and repeats.
    let mut not_states = Vec::new();
    for occurrence in &input.not_states {
        for name in split_group(occurrence) {
            let state = parse_state(&name)?;
            if !not_states.contains(&state) {
                not_states.push(state);
            }
        }
    }

    Ok(StructuredFilters {
        label_and_groups,
        not_labels,
        states,
        not_states,
    })
}

impl StructuredFilters {
    /// Whether no structured constraint at all was requested, so the filters are
    /// a pass-through.
    pub fn is_empty(&self) -> bool {
        self.label_and_groups.is_empty()
            && self.not_labels.is_empty()
            && self.states.is_none()
            && self.not_states.is_empty()
    }

    /// Whether a node satisfies every requested structured family. Families
    /// combine with AND: a node passes only if it clears the label groups, the
    /// label exclusion, the positive state set, and the state exclusion.
    pub fn matches(&self, node: &NodeFile) -> bool {
        self.matches_labels(&node.frontmatter.labels) && self.matches_state(node.frontmatter.status)
    }

    /// The label family: every AND-group must be satisfied (the node carries at
    /// least one label from the group), and the node must carry none of the
    /// excluded labels.
    fn matches_labels(&self, node_labels: &[String]) -> bool {
        let has = |label: &String| node_labels.iter().any(|l| l == label);
        let all_groups_satisfied = self
            .label_and_groups
            .iter()
            .all(|group| group.iter().any(has));
        let none_excluded = !self.not_labels.iter().any(has);
        all_groups_satisfied && none_excluded
    }

    /// The state family: the node's state must be in the positive set (when
    /// given) and not in the excluded set.
    fn matches_state(&self, state: NodeState) -> bool {
        let in_positive = match &self.states {
            Some(set) => set.contains(&state),
            None => true,
        };
        let not_excluded = !self.not_states.contains(&state);
        in_positive && not_excluded
    }
}

/// The built-in matcher: a regular expression tested against a node's name,
/// summary, and body. This is the default match implementation and needs no
/// external process, so a store has a working `match` out of the box.
///
/// The name of this implementation is reserved: it always means this built-in
/// behaviour and cannot be redefined by configuration.
pub const BUILTIN_MATCHER_NAME: &str = "regex";

/// A compiled built-in regex matcher over name + summary + body.
#[derive(Debug, Clone)]
pub struct RegexMatcher {
    regex: regex::Regex,
}

/// Why a built-in regex query could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegexError {
    /// The query was not a valid regular expression.
    #[error("invalid match pattern: {0}")]
    Invalid(String),
}

impl RegexMatcher {
    /// Compile a query into a matcher. An invalid pattern is a usage error.
    pub fn compile(query: &str) -> Result<Self, RegexError> {
        let regex = regex::Regex::new(query).map_err(|e| RegexError::Invalid(e.to_string()))?;
        Ok(Self { regex })
    }

    /// Whether the pattern is found anywhere in the node's name, summary, or
    /// body. These three fields are the built-in match surface.
    pub fn matches(&self, node: &NodeFile) -> bool {
        let fm = &node.frontmatter;
        self.regex.is_match(&fm.name)
            || self.regex.is_match(&fm.summary)
            || self.regex.is_match(&node.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeFile, NodeFrontmatter};
    use jiff::Timestamp;
    use serde_yaml::Mapping;

    fn ts() -> Timestamp {
        "2024-01-01T00:00:00Z".parse().unwrap()
    }

    fn node(name: &str, summary: &str, body: &str, status: NodeState, labels: &[&str]) -> NodeFile {
        NodeFile {
            frontmatter: NodeFrontmatter {
                number: 1,
                name: name.into(),
                summary: summary.into(),
                status,
                labels: labels.iter().map(|s| s.to_string()).collect(),
                parent: None,
                blocked_by: Vec::new(),
                block_reason: None,
                close_reason: None,
                assets: Vec::new(),
                comments: Vec::new(),
                created: ts(),
                updated: ts(),
                extra: Mapping::new(),
            },
            body: body.into(),
        }
    }

    fn input() -> FilterInput {
        FilterInput::default()
    }

    // -- label family ------------------------------------------------------

    #[test]
    fn label_repeats_are_and_commas_are_or() {
        // `--label a --label b,c` => a AND (b OR c).
        let f = parse_filters(&FilterInput {
            labels: vec!["a".into(), "b,c".into()],
            ..input()
        })
        .unwrap();
        assert_eq!(f.label_and_groups, vec![vec!["a"], vec!["b", "c"]]);
        // Has a and b: passes.
        assert!(f.matches(&node("n", "s", "", NodeState::ToDo, &["a", "b"])));
        // Has a and c: passes (c satisfies the OR-group).
        assert!(f.matches(&node("n", "s", "", NodeState::ToDo, &["a", "c"])));
        // Has a only: fails the (b OR c) group.
        assert!(!f.matches(&node("n", "s", "", NodeState::ToDo, &["a"])));
        // Has b and c but not a: fails the a group.
        assert!(!f.matches(&node("n", "s", "", NodeState::ToDo, &["b", "c"])));
    }

    #[test]
    fn not_label_is_has_none_of() {
        let f = parse_filters(&FilterInput {
            not_labels: vec!["x".into(), "y,z".into()],
            ..input()
        })
        .unwrap();
        // Union of excluded: x, y, z.
        assert_eq!(f.not_labels, vec!["x", "y", "z"]);
        // Carries none of them: passes.
        assert!(f.matches(&node("n", "s", "", NodeState::ToDo, &["a"])));
        // Carries one excluded: fails.
        assert!(!f.matches(&node("n", "s", "", NodeState::ToDo, &["a", "z"])));
    }

    #[test]
    fn label_and_not_label_combine_with_and() {
        let f = parse_filters(&FilterInput {
            labels: vec!["keep".into()],
            not_labels: vec!["drop".into()],
            ..input()
        })
        .unwrap();
        assert!(f.matches(&node("n", "s", "", NodeState::ToDo, &["keep"])));
        assert!(!f.matches(&node("n", "s", "", NodeState::ToDo, &["keep", "drop"])));
        assert!(!f.matches(&node("n", "s", "", NodeState::ToDo, &["other"])));
    }

    #[test]
    fn empty_label_entry_is_an_error() {
        assert_eq!(
            parse_filters(&FilterInput {
                labels: vec!["a,".into()],
                ..input()
            }),
            Err(FilterError::EmptyLabel)
        );
    }

    // -- state family ------------------------------------------------------

    #[test]
    fn state_commas_are_or() {
        let f = parse_filters(&FilterInput {
            states: vec!["to-do,blocked".into()],
            ..input()
        })
        .unwrap();
        assert_eq!(f.states, Some(vec![NodeState::ToDo, NodeState::Blocked]));
        assert!(f.matches(&node("n", "s", "", NodeState::ToDo, &[])));
        assert!(f.matches(&node("n", "s", "", NodeState::Blocked, &[])));
        assert!(!f.matches(&node("n", "s", "", NodeState::Done, &[])));
    }

    #[test]
    fn repeated_state_selector_is_an_error() {
        assert_eq!(
            parse_filters(&FilterInput {
                states: vec!["to-do".into(), "done".into()],
                ..input()
            }),
            Err(FilterError::RepeatedStateSelector)
        );
    }

    #[test]
    fn not_state_is_symmetric_exclusion() {
        let f = parse_filters(&FilterInput {
            not_states: vec!["done,closed".into()],
            ..input()
        })
        .unwrap();
        assert!(f.matches(&node("n", "s", "", NodeState::ToDo, &[])));
        assert!(!f.matches(&node("n", "s", "", NodeState::Done, &[])));
        assert!(!f.matches(&node("n", "s", "", NodeState::Closed, &[])));
    }

    #[test]
    fn open_aggregator_expands_to_non_terminal_states() {
        let f = parse_filters(&FilterInput {
            open: true,
            ..input()
        })
        .unwrap();
        assert!(f.matches(&node("n", "s", "", NodeState::ToDo, &[])));
        assert!(f.matches(&node("n", "s", "", NodeState::InProgress, &[])));
        assert!(f.matches(&node("n", "s", "", NodeState::Blocked, &[])));
        assert!(!f.matches(&node("n", "s", "", NodeState::Done, &[])));
        assert!(!f.matches(&node("n", "s", "", NodeState::Closed, &[])));
    }

    #[test]
    fn resolved_aggregator_expands_to_terminal_states() {
        let f = parse_filters(&FilterInput {
            resolved: true,
            ..input()
        })
        .unwrap();
        assert!(f.matches(&node("n", "s", "", NodeState::Done, &[])));
        assert!(f.matches(&node("n", "s", "", NodeState::Closed, &[])));
        assert!(!f.matches(&node("n", "s", "", NodeState::ToDo, &[])));
    }

    #[test]
    fn aggregators_conflict_with_each_other() {
        assert_eq!(
            parse_filters(&FilterInput {
                open: true,
                resolved: true,
                ..input()
            }),
            Err(FilterError::ConflictingStateSelectors)
        );
    }

    #[test]
    fn aggregator_conflicts_with_explicit_state() {
        assert_eq!(
            parse_filters(&FilterInput {
                open: true,
                states: vec!["done".into()],
                ..input()
            }),
            Err(FilterError::ConflictingStateSelectors)
        );
        assert_eq!(
            parse_filters(&FilterInput {
                resolved: true,
                states: vec!["to-do".into()],
                ..input()
            }),
            Err(FilterError::ConflictingStateSelectors)
        );
    }

    #[test]
    fn unknown_state_is_an_error() {
        assert_eq!(
            parse_filters(&FilterInput {
                states: vec!["frozen".into()],
                ..input()
            }),
            Err(FilterError::UnknownState("frozen".into()))
        );
    }

    // -- AND across families ----------------------------------------------

    #[test]
    fn label_and_state_families_combine_with_and() {
        let f = parse_filters(&FilterInput {
            labels: vec!["urgent".into()],
            states: vec!["in-progress".into()],
            ..input()
        })
        .unwrap();
        // Right label and right state: passes.
        assert!(f.matches(&node("n", "s", "", NodeState::InProgress, &["urgent"])));
        // Right label, wrong state: fails.
        assert!(!f.matches(&node("n", "s", "", NodeState::ToDo, &["urgent"])));
        // Wrong label, right state: fails.
        assert!(!f.matches(&node("n", "s", "", NodeState::InProgress, &["later"])));
    }

    #[test]
    fn empty_filters_pass_everything() {
        let f = parse_filters(&input()).unwrap();
        assert!(f.is_empty());
        assert!(f.matches(&node("n", "s", "", NodeState::ToDo, &[])));
    }

    // -- built-in regex matcher -------------------------------------------

    #[test]
    fn regex_matches_name_summary_body() {
        let m = RegexMatcher::compile("needle").unwrap();
        assert!(m.matches(&node("has needle here", "s", "", NodeState::ToDo, &[])));
        assert!(m.matches(&node("n", "the needle", "", NodeState::ToDo, &[])));
        assert!(m.matches(&node("n", "s", "body needle", NodeState::ToDo, &[])));
        assert!(!m.matches(&node("n", "s", "nothing", NodeState::ToDo, &[])));
    }

    #[test]
    fn regex_supports_patterns_not_just_substrings() {
        let m = RegexMatcher::compile("^Fix").unwrap();
        assert!(m.matches(&node("Fix the bug", "s", "", NodeState::ToDo, &[])));
        assert!(!m.matches(&node("Please Fix", "s", "", NodeState::ToDo, &[])));
    }

    #[test]
    fn invalid_regex_is_an_error() {
        assert!(matches!(
            RegexMatcher::compile("("),
            Err(RegexError::Invalid(_))
        ));
    }

    #[test]
    fn builtin_matcher_name_is_reserved() {
        assert_eq!(BUILTIN_MATCHER_NAME, "regex");
    }
}
