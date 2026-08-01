//! Cooperative visibility policy for a launched agent session.
//!
//! The marker environment is a convention passed to a child agent, not an
//! access-control boundary: a child can alter its own environment. This value
//! keeps the convention pure after environment capture, so CLI and launch
//! preparation apply the same presence-based rule without separately parsing
//! or validating marker values.

use std::collections::BTreeMap;

/// Marks an active cooperative agent session. Its value names the launched
/// target, but presence alone (including an empty value) activates the policy.
pub const SESSION_ENV_VAR: &str = "LOTI_AGENT_SESSION";

/// Names the workflow a cooperative agent session is bound to. Presence alone
/// (including an empty value) activates the policy and selects that exact ID.
pub const WORKFLOW_ENV_VAR: &str = "LOTI_AGENT_WORKFLOW";

/// The cooperative visibility rules derived from an inherited environment.
///
/// Marker values are deliberately opaque here. Resource discovery decides
/// whether an ID is valid or resolves; this policy only records marker presence
/// and, when present, the exact workflow string to make visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPolicy {
    agent_namespace_available: bool,
    workflow_id: Option<String>,
}

impl SessionPolicy {
    /// Derive the policy from an environment snapshot. Either marker makes the
    /// operator-facing `agent` namespace unavailable, even if its value is
    /// empty. Only the workflow marker narrows workflow visibility.
    pub fn from_env(env: &BTreeMap<String, String>) -> Self {
        let workflow_id = env.get(WORKFLOW_ENV_VAR).cloned();
        Self {
            agent_namespace_available: !env.contains_key(SESSION_ENV_VAR) && workflow_id.is_none(),
            workflow_id,
        }
    }

    /// Whether operator-facing agent-profile commands remain available.
    pub fn agent_namespace_available(&self) -> bool {
        self.agent_namespace_available
    }

    /// The exact workflow selected for this session, when its marker exists.
    pub fn workflow_id(&self) -> Option<&str> {
        self.workflow_id.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neither_marker_leaves_operator_visibility_unrestricted() {
        let policy = SessionPolicy::from_env(&BTreeMap::new());
        assert!(policy.agent_namespace_available());
        assert_eq!(policy.workflow_id(), None);
    }

    #[test]
    fn either_marker_hides_agent_commands_by_presence_not_value() {
        for env in [
            BTreeMap::from([(SESSION_ENV_VAR.to_string(), String::new())]),
            BTreeMap::from([(WORKFLOW_ENV_VAR.to_string(), String::new())]),
            BTreeMap::from([
                (SESSION_ENV_VAR.to_string(), "target".to_string()),
                (WORKFLOW_ENV_VAR.to_string(), "workflow".to_string()),
            ]),
        ] {
            assert!(
                !SessionPolicy::from_env(&env).agent_namespace_available(),
                "marker environment {env:?}"
            );
        }
    }

    #[test]
    fn only_the_workflow_marker_selects_workflow_visibility() {
        let policy = SessionPolicy::from_env(&BTreeMap::from([
            (SESSION_ENV_VAR.to_string(), "target".to_string()),
            (WORKFLOW_ENV_VAR.to_string(), "review".to_string()),
        ]));
        assert_eq!(policy.workflow_id(), Some("review"));
    }
}
