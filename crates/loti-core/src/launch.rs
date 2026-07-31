//! Rendering a resolved target, effective workflow, and effective harness
//! profile into a validated, ready-to-spawn launch plan.
//!
//! This module is the one place that turns a harness profile's templated
//! `command`/`args`/`cwd`/`env` into concrete values a `Command` can run
//! directly. It never touches a process (it does not spawn a child, does not
//! read `std::env`, and does not walk the filesystem for discovery) and it
//! never mutates tracker state — every input it needs (the target, the
//! selected workflow id, the profile, and the caller's own environment/paths)
//! is passed in explicitly, so the same preparation is reachable from a CLI
//! command or a TUI action without either reimplementing it.
//!
//! ## Cooperative agent sessions
//!
//! A launch is refused outright while a cooperative agent session is already
//! active in the caller's environment ([`session_active`]): this stops an
//! agent that was itself launched by loti from recursively starting another
//! session. The two marker variables ([`SESSION_ENV_VAR`],
//! [`WORKFLOW_ENV_VAR`]) are guardrails a launched agent could remove from its
//! own environment; they are not a security boundary.
//!
//! ## Templates
//!
//! A template string may contain `{{ variable }}` placeholders (optional
//! interior whitespace, an exact variable name). A single `{` or `}` that is
//! not doubled is always literal text. Six variables are recognized:
//! `loti_prompt` (the generated bootstrap instruction), `project_root`,
//! `current_directory`, `loti_ref` and `loti_ref_name` (the target's
//! reference and display name), and `loti_workflow` (the selected workflow
//! id). `command` and every environment key are literal — only `args`, `cwd`,
//! and environment *values* are rendered.
//!
//! `args` must contain exactly one element whose complete template is
//! `{{ loti_prompt }}`; an occurrence embedded in a longer argument does not
//! count, so every harness is guaranteed to receive the bootstrap as one
//! whole argument.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::domain::NodeRef;
use crate::resource::{Profile, ResourceId};

/// Marks an active cooperative agent session: set to the launched target's
/// reference. Presence alone (even an empty value) is the signal; the value
/// is not parsed by [`session_active`].
pub const SESSION_ENV_VAR: &str = "LOTI_AGENT_SESSION";

/// Marks the workflow a cooperative agent session is bound to. Presence alone
/// (even an empty value) is the signal; the value is not parsed by
/// [`session_active`].
pub const WORKFLOW_ENV_VAR: &str = "LOTI_AGENT_WORKFLOW";

/// The case-sensitive prefix reserved for loti's own launch-environment keys.
/// A profile may never define a key under this prefix, so it can never shadow
/// or corrupt the session markers this module adds.
const RESERVED_ENV_PREFIX: &str = "LOTI_";

/// What a launch is scoped to: an epic or one of its tickets. Each variant
/// carries both the identifier/reference a launched agent uses to inspect its
/// target through the CLI, and the display name shown in the bootstrap
/// instruction, so neither has to be re-derived from the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// An epic, identified by its id.
    Epic {
        /// The epic's id.
        id: String,
        /// The epic's display name.
        name: String,
    },
    /// A single ticket (any node below its epic), identified by its
    /// `<epic-id>/<number>` reference.
    Ticket {
        /// The ticket's reference.
        reference: NodeRef,
        /// The ticket's display name.
        name: String,
    },
}

impl Target {
    /// The target's reference as printed in the bootstrap instruction and
    /// carried by the `LOTI_AGENT_SESSION` marker: an epic's bare id, or a
    /// ticket's `<epic-id>/<number>` reference.
    pub fn reference(&self) -> String {
        match self {
            Target::Epic { id, .. } => id.clone(),
            Target::Ticket { reference, .. } => reference.to_string(),
        }
    }

    /// The target's display name.
    pub fn name(&self) -> &str {
        match self {
            Target::Epic { name, .. } | Target::Ticket { name, .. } => name,
        }
    }
}

/// Everything about where and as whom loti itself is running, as distinct
/// from the profile being rendered. `project_root` and `current_directory` are
/// supplied already resolved — this module performs no marker discovery and
/// reads no process state itself, so a caller (CLI today, others later) is
/// free to resolve them however it locates a project. `env` is the caller's
/// own inherited environment snapshot: the base the prepared plan's
/// environment is built on top of, and where [`session_active`] looks for the
/// cooperative-session markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerContext {
    /// The project root: the rendered default for `cwd`, and the value of the
    /// `project_root` template variable.
    pub project_root: PathBuf,
    /// The caller's current directory: the value of the `current_directory`
    /// template variable.
    pub current_directory: PathBuf,
    /// The caller's inherited environment snapshot, in stable key order.
    pub env: BTreeMap<String, String>,
}

/// Whether a cooperative agent session is already active in `env`: either
/// [`SESSION_ENV_VAR`] or [`WORKFLOW_ENV_VAR`] is present, including with an
/// empty value. This is cooperative signalling only — a child process can
/// always alter its own environment — so it is never treated as a security
/// boundary, only as a guardrail an ordinary launch respects.
pub fn session_active(env: &BTreeMap<String, String>) -> bool {
    env.contains_key(SESSION_ENV_VAR) || env.contains_key(WORKFLOW_ENV_VAR)
}

/// A validated, ready-to-spawn direct launch: no shell text, no shell
/// evaluation. `program` and `args` are handed straight to a process
/// constructor; `cwd` is an absolute, existing directory; `env` is the
/// complete environment the child should run with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// The executable to run directly, never through a shell. This is the
    /// profile's literal `command`; preparation does not resolve `PATH` or
    /// require the executable to exist.
    pub program: String,
    /// The rendered argument vector, in order.
    pub args: Vec<String>,
    /// The rendered, validated (absolute, existing-directory) working
    /// directory.
    pub cwd: PathBuf,
    /// The complete environment, in stable key order: caller inheritance,
    /// then the session markers, then the rendered profile values.
    pub env: BTreeMap<String, String>,
}

/// The six recognized template variables. Public only because it appears in
/// [`RenderError::NonUtf8Path`]; a caller matches or displays it, but never
/// constructs or parses one directly (that is always driven by a template
/// string, via [`prepare`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variable {
    LotiPrompt,
    ProjectRoot,
    CurrentDirectory,
    LotiRef,
    LotiRefName,
    LotiWorkflow,
}

impl Variable {
    /// Look up a variable by its exact placeholder name (already trimmed of
    /// interior whitespace). `None` means the name is not one of the six
    /// recognized variables.
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "loti_prompt" => Variable::LotiPrompt,
            "project_root" => Variable::ProjectRoot,
            "current_directory" => Variable::CurrentDirectory,
            "loti_ref" => Variable::LotiRef,
            "loti_ref_name" => Variable::LotiRefName,
            "loti_workflow" => Variable::LotiWorkflow,
            _ => return None,
        })
    }
}

impl fmt::Display for Variable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Variable::LotiPrompt => "loti_prompt",
            Variable::ProjectRoot => "project_root",
            Variable::CurrentDirectory => "current_directory",
            Variable::LotiRef => "loti_ref",
            Variable::LotiRefName => "loti_ref_name",
            Variable::LotiWorkflow => "loti_workflow",
        })
    }
}

/// One field a template error or render error is reported against, so a
/// failure names exactly where it occurred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateField {
    /// The argument at this zero-based index.
    Arg(usize),
    /// The `cwd` template.
    Cwd,
    /// The environment value stored under this key.
    Env(String),
}

impl fmt::Display for TemplateField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateField::Arg(i) => write!(f, "args[{i}]"),
            TemplateField::Cwd => write!(f, "cwd"),
            TemplateField::Env(key) => write!(f, "env.{key}"),
        }
    }
}

/// A template string parsed into literal runs and variable references, with
/// no context-dependent lookups performed yet. Parsing alone can already
/// reject a malformed placeholder or an unrecognized variable name, since the
/// set of six variable names is fixed and needs no caller context to check
/// against.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Placeholder(Variable),
}

/// Why a template string failed to parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// A `{{` was never followed by a matching `}}`.
    #[error("an opening placeholder delimiter has no matching closing delimiter")]
    UnmatchedOpeningDelimiter,
    /// A placeholder's interior was empty (or all whitespace).
    #[error("a placeholder is empty")]
    EmptyPlaceholder,
    /// A placeholder's interior, once trimmed, did not name one of the six
    /// recognized variables.
    #[error("'{0}' is not a recognized template variable")]
    UnknownVariable(String),
}

/// Why rendering a parsed template failed. Parsing alone cannot fail this
/// way: it never needs to know a variable's actual value, only its name. A
/// path-derived variable's value comes from a caller-supplied [`PathBuf`],
/// which is not guaranteed to be valid UTF-8; rendering is where that value is
/// actually needed as template text, so that is where the failure surfaces.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    /// The named variable's value is not valid UTF-8 and cannot be rendered
    /// into template text.
    #[error("'{0}' is not valid UTF-8 and cannot be rendered")]
    NonUtf8Path(Variable),
}

/// Why the requested count of standalone `{{ loti_prompt }}` arguments (which
/// must be exactly one) did not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptCount(pub usize);

/// Why a launch could not be prepared. Every variant states the rule it
/// enforces; [`prepare`] reports the first violated rule in a fixed order, so
/// a caller never has to guess which of several problems was actually acted
/// on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    /// A cooperative agent session is already active in the caller's
    /// environment; preparing another launch is refused.
    #[error(
        "a cooperative agent session is already active (LOTI_AGENT_SESSION or \
         LOTI_AGENT_WORKFLOW is set); launching another agent is refused"
    )]
    SessionActive,
    /// The profile's `command` is empty.
    #[error("profile command must not be empty")]
    EmptyCommand,
    /// The profile defines an environment key under the reserved `LOTI_`
    /// prefix.
    #[error(
        "profile environment key '{0}' is reserved: a profile may not define \
         a key starting with 'LOTI_'"
    )]
    ReservedEnvKey(String),
    /// A template string failed to parse.
    #[error("{field} has a malformed template: {reason}")]
    Template {
        /// Where the malformed template was found.
        field: TemplateField,
        /// Why it was rejected.
        reason: TemplateError,
    },
    /// `args` did not contain exactly one element whose complete template is
    /// `{{ loti_prompt }}`.
    #[error(
        "args must contain exactly one argument whose complete template is \
         '{{{{ loti_prompt }}}}'; found {}",
        _0.0
    )]
    PromptPlaceholderCount(PromptCount),
    /// A parsed template could not be rendered.
    #[error("{field} could not be rendered: {reason}")]
    Render {
        /// Where rendering failed.
        field: TemplateField,
        /// Why it failed.
        reason: RenderError,
    },
    /// The rendered `cwd` is not an absolute path.
    #[error("cwd '{}' is not an absolute path", _0.display())]
    CwdNotAbsolute(PathBuf),
    /// The rendered `cwd` does not exist, or is not a directory.
    #[error("cwd '{}' does not exist or is not a directory", _0.display())]
    CwdNotADirectory(PathBuf),
}

/// Parse a template string into literal and placeholder segments. A `{{` with
/// no later `}}` is [`TemplateError::UnmatchedOpeningDelimiter`]; an interior
/// that is empty after trimming is [`TemplateError::EmptyPlaceholder`]; an
/// interior that does not name one of the six recognized variables is
/// [`TemplateError::UnknownVariable`]. A `{` or `}` that is never doubled is
/// never treated as the start of a placeholder at all, so it always ends up as
/// ordinary literal text.
fn parse_template(input: &str) -> Result<Vec<Segment>, TemplateError> {
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut rest = input;
    loop {
        let Some(open) = rest.find("{{") else {
            literal.push_str(rest);
            break;
        };
        literal.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            return Err(TemplateError::UnmatchedOpeningDelimiter);
        };
        let trimmed = after_open[..close].trim();
        if trimmed.is_empty() {
            return Err(TemplateError::EmptyPlaceholder);
        }
        let variable = Variable::parse(trimmed)
            .ok_or_else(|| TemplateError::UnknownVariable(trimmed.to_string()))?;
        if !literal.is_empty() {
            segments.push(Segment::Literal(std::mem::take(&mut literal)));
        }
        segments.push(Segment::Placeholder(variable));
        rest = &after_open[close + 2..];
    }
    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }
    Ok(segments)
}

/// Whether a parsed template is exactly one placeholder naming `loti_prompt`
/// and nothing else — the "complete template" rule an occurrence embedded in
/// a longer argument does not satisfy.
fn is_standalone_prompt(segments: &[Segment]) -> bool {
    matches!(segments, [Segment::Placeholder(Variable::LotiPrompt)])
}

/// Convert a caller-supplied path to the template text it renders as. Fails
/// only when the path is not valid UTF-8.
fn path_to_str(path: &Path, variable: Variable) -> Result<&str, RenderError> {
    path.to_str().ok_or(RenderError::NonUtf8Path(variable))
}

/// The rendering context a parsed template is substituted against: the
/// precomputed bootstrap instruction plus everything a variable's value is
/// derived from.
struct RenderContext<'a> {
    prompt: &'a str,
    target: &'a Target,
    workflow: &'a ResourceId,
    caller: &'a CallerContext,
}

/// Resolve one variable's rendered value.
fn variable_value(var: Variable, ctx: &RenderContext<'_>) -> Result<String, RenderError> {
    Ok(match var {
        Variable::LotiPrompt => ctx.prompt.to_string(),
        Variable::ProjectRoot => path_to_str(&ctx.caller.project_root, var)?.to_string(),
        Variable::CurrentDirectory => path_to_str(&ctx.caller.current_directory, var)?.to_string(),
        Variable::LotiRef => ctx.target.reference(),
        Variable::LotiRefName => ctx.target.name().to_string(),
        Variable::LotiWorkflow => ctx.workflow.as_str().to_string(),
    })
}

/// Render a parsed template to its final text.
fn render(segments: &[Segment], ctx: &RenderContext<'_>) -> Result<String, RenderError> {
    let mut out = String::new();
    for segment in segments {
        match segment {
            Segment::Literal(text) => out.push_str(text),
            Segment::Placeholder(var) => out.push_str(&variable_value(*var, ctx)?),
        }
    }
    Ok(out)
}

/// The fixed loti-owned bootstrap instruction for a target: the sole value of
/// the `loti_prompt` template variable. It is the one place this text is
/// generated, so the epic and ticket forms cannot drift into disagreement
/// with each other.
fn bootstrap_instruction(target: &Target, workflow: &ResourceId) -> String {
    let (kind, reference, name, show_command) = match target {
        Target::Epic { id, name } => (
            "epic",
            id.clone(),
            name.clone(),
            format!("loti epic show {id}"),
        ),
        Target::Ticket { reference, name } => (
            "ticket",
            reference.to_string(),
            name.clone(),
            format!("loti ticket show {reference}"),
        ),
    };
    format!(
        "You are working as an agent in the loti workflow \"{workflow}\" on {kind} \"{reference}\" ({name}).\n\
\n\
Before acting:\n\
1. Run `loti skill` to learn how to operate loti.\n\
2. Run `loti workflow show {workflow}` to read your instructions.\n\
3. Run `{show_command}` to fill your context.\n\
\n\
Follow the instructions in the named workflow; this is your main goal.\n\
\n\
IMPORTANT: If the `loti` command is not available, stop IMMEDIATELY and notify the user. Do not try to circumvent or fix the issue.",
    )
}

/// Prepare a validated, ready-to-spawn launch plan from a resolved target, the
/// selected workflow id, an effective harness profile, and the caller's own
/// context. This performs no I/O beyond the `cwd` existing-directory check,
/// spawns no process, and mutates no tracker state.
///
/// Preparation is refused outright while a cooperative agent session is
/// already active (see [`session_active`]). Otherwise, the first violated
/// rule is reported in this fixed order: a non-empty `command`; no
/// profile-defined `LOTI_`-prefixed environment key; every template parses
/// (checked over `args` in order, then `cwd`, then environment values in
/// stable key order); `args` contains exactly one standalone `{{ loti_prompt
/// }}`; every template renders (same field order); the rendered `cwd` is
/// absolute; the rendered `cwd` is an existing directory.
pub fn prepare(
    target: &Target,
    profile: &Profile,
    workflow: &ResourceId,
    caller: &CallerContext,
) -> Result<LaunchPlan, LaunchError> {
    if session_active(&caller.env) {
        return Err(LaunchError::SessionActive);
    }

    if profile.command.is_empty() {
        return Err(LaunchError::EmptyCommand);
    }

    if let Some(env) = &profile.env {
        // `env` is a `BTreeMap`, so this already visits keys in stable order.
        if let Some(key) = env.keys().find(|k| k.starts_with(RESERVED_ENV_PREFIX)) {
            return Err(LaunchError::ReservedEnvKey(key.clone()));
        }
    }

    // Parse every template, in the fixed field order: args, then cwd, then
    // environment values in stable key order. `collect` on a `Result`
    // short-circuits at the first `Err`, which is what keeps this "in order".
    let arg_templates: Vec<Vec<Segment>> = profile
        .args
        .iter()
        .enumerate()
        .map(|(i, raw)| {
            parse_template(raw).map_err(|reason| LaunchError::Template {
                field: TemplateField::Arg(i),
                reason,
            })
        })
        .collect::<Result<_, _>>()?;

    let cwd_template = match &profile.cwd {
        Some(raw) => Some(parse_template(raw).map_err(|reason| LaunchError::Template {
            field: TemplateField::Cwd,
            reason,
        })?),
        None => None,
    };

    let env_templates: Vec<(String, Vec<Segment>)> = match &profile.env {
        Some(env) => env
            .iter()
            .map(|(key, raw)| {
                parse_template(raw)
                    .map(|segments| (key.clone(), segments))
                    .map_err(|reason| LaunchError::Template {
                        field: TemplateField::Env(key.clone()),
                        reason,
                    })
            })
            .collect::<Result<_, _>>()?,
        None => Vec::new(),
    };

    // Every template parsed; now the standalone-prompt count, which is a
    // structural property of the already-parsed args alone.
    let prompt_count = arg_templates
        .iter()
        .filter(|segments| is_standalone_prompt(segments))
        .count();
    if prompt_count != 1 {
        return Err(LaunchError::PromptPlaceholderCount(PromptCount(
            prompt_count,
        )));
    }

    let prompt = bootstrap_instruction(target, workflow);
    let render_ctx = RenderContext {
        prompt: &prompt,
        target,
        workflow,
        caller,
    };

    // Render, in the same fixed field order as parsing.
    let rendered_args: Vec<String> = arg_templates
        .iter()
        .enumerate()
        .map(|(i, segments)| {
            render(segments, &render_ctx).map_err(|reason| LaunchError::Render {
                field: TemplateField::Arg(i),
                reason,
            })
        })
        .collect::<Result<_, _>>()?;

    let rendered_cwd = match &cwd_template {
        Some(segments) => render(segments, &render_ctx).map_err(|reason| LaunchError::Render {
            field: TemplateField::Cwd,
            reason,
        })?,
        // No override: the default is the project root itself, rendered the
        // same way the `project_root` variable would be.
        None => path_to_str(&caller.project_root, Variable::ProjectRoot)
            .map_err(|reason| LaunchError::Render {
                field: TemplateField::Cwd,
                reason,
            })?
            .to_string(),
    };

    let mut rendered_env: Vec<(String, String)> = Vec::with_capacity(env_templates.len());
    for (key, segments) in &env_templates {
        let value = render(segments, &render_ctx).map_err(|reason| LaunchError::Render {
            field: TemplateField::Env(key.clone()),
            reason,
        })?;
        rendered_env.push((key.clone(), value));
    }

    // All rendering succeeded; only now are the rendered `cwd`'s own two
    // properties checked.
    let cwd = PathBuf::from(rendered_cwd);
    if !cwd.is_absolute() {
        return Err(LaunchError::CwdNotAbsolute(cwd));
    }
    if !cwd.is_dir() {
        return Err(LaunchError::CwdNotADirectory(cwd));
    }

    // Caller inheritance, then fresh session markers, then rendered profile
    // values — each stage may overwrite a same-named key from the stage
    // before it. A profile can never collide with the markers: every `LOTI_`
    // key was already rejected above.
    let mut env = caller.env.clone();
    env.insert(SESSION_ENV_VAR.to_string(), target.reference());
    env.insert(WORKFLOW_ENV_VAR.to_string(), workflow.as_str().to_string());
    for (key, value) in rendered_env {
        env.insert(key, value);
    }

    Ok(LaunchPlan {
        program: profile.command.clone(),
        args: rendered_args,
        cwd,
        env,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow(id: &str) -> ResourceId {
        ResourceId::parse(id).unwrap()
    }

    fn epic_target() -> Target {
        Target::Epic {
            id: "agent-integration-impl".to_string(),
            name: "Implement agent integration".to_string(),
        }
    }

    fn ticket_target() -> Target {
        Target::Ticket {
            reference: NodeRef::new("agent-integration-impl", 2),
            name: "Prepare validated foreground launch plans".to_string(),
        }
    }

    fn context(project_root: &Path) -> CallerContext {
        CallerContext {
            project_root: project_root.to_path_buf(),
            current_directory: project_root.to_path_buf(),
            env: BTreeMap::new(),
        }
    }

    fn minimal_profile() -> Profile {
        Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti_prompt }}".to_string()],
            cwd: None,
            env: None,
        }
    }

    // -- bootstrap text ------------------------------------------------------

    #[test]
    fn epic_bootstrap_text_is_exact() {
        let dir = tempfile::tempdir().unwrap();
        let plan = prepare(
            &epic_target(),
            &minimal_profile(),
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap();
        assert_eq!(
            plan.args[0],
            "You are working as an agent in the loti workflow \"review\" on epic \"agent-integration-impl\" (Implement agent integration).\n\
\n\
Before acting:\n\
1. Run `loti skill` to learn how to operate loti.\n\
2. Run `loti workflow show review` to read your instructions.\n\
3. Run `loti epic show agent-integration-impl` to fill your context.\n\
\n\
Follow the instructions in the named workflow; this is your main goal.\n\
\n\
IMPORTANT: If the `loti` command is not available, stop IMMEDIATELY and notify the user. Do not try to circumvent or fix the issue."
        );
    }

    #[test]
    fn ticket_bootstrap_text_is_exact() {
        let dir = tempfile::tempdir().unwrap();
        let plan = prepare(
            &ticket_target(),
            &minimal_profile(),
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap();
        assert_eq!(
            plan.args[0],
            "You are working as an agent in the loti workflow \"review\" on ticket \"agent-integration-impl/2\" (Prepare validated foreground launch plans).\n\
\n\
Before acting:\n\
1. Run `loti skill` to learn how to operate loti.\n\
2. Run `loti workflow show review` to read your instructions.\n\
3. Run `loti ticket show agent-integration-impl/2` to fill your context.\n\
\n\
Follow the instructions in the named workflow; this is your main goal.\n\
\n\
IMPORTANT: If the `loti` command is not available, stop IMMEDIATELY and notify the user. Do not try to circumvent or fix the issue."
        );
    }

    // -- literal rendering and argv order ------------------------------------

    #[test]
    fn template_variables_render_literally_across_args_cwd_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let current = root.join("current");
        std::fs::create_dir(&current).unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec![
                "--session={{loti_ref}}-{{ loti_workflow }}".to_string(),
                "{{ loti_prompt }}".to_string(),
                "plain arg with no placeholder".to_string(),
            ],
            cwd: Some("{{ project_root }}".to_string()),
            env: Some(BTreeMap::from([(
                "GREETING".to_string(),
                "hello {{ loti_ref_name }} from {{ current_directory }}".to_string(),
            )])),
        };

        let plan = prepare(
            &ticket_target(),
            &profile,
            &workflow("review"),
            &CallerContext {
                project_root: root.to_path_buf(),
                current_directory: current.clone(),
                env: BTreeMap::new(),
            },
        )
        .unwrap();

        // argv order is preserved exactly, and each literal/placeholder mix
        // renders as plain concatenated text.
        assert_eq!(plan.args[0], "--session=agent-integration-impl/2-review");
        assert!(plan.args[1].starts_with("You are working"));
        assert_eq!(plan.args[2], "plain arg with no placeholder");
        assert_eq!(plan.cwd, root);
        assert_eq!(
            plan.env.get("GREETING"),
            Some(&format!(
                "hello Prepare validated foreground launch plans from {}",
                current.display()
            ))
        );
    }

    #[test]
    fn program_is_the_literal_command_never_a_shell() {
        let dir = tempfile::tempdir().unwrap();
        let plan = prepare(
            &epic_target(),
            &minimal_profile(),
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap();
        assert_eq!(plan.program, "pi");
    }

    // -- template grammar -----------------------------------------------------

    #[test]
    fn isolated_single_braces_are_literal_text() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec![
                "{ not a placeholder }".to_string(),
                "{{ loti_prompt }}".to_string(),
            ],
            cwd: None,
            env: None,
        };
        let plan = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap();
        assert_eq!(plan.args[0], "{ not a placeholder }");
    }

    #[test]
    fn unmatched_opening_delimiter_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti_prompt".to_string()],
            cwd: None,
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(
            err,
            LaunchError::Template {
                field: TemplateField::Arg(0),
                reason: TemplateError::UnmatchedOpeningDelimiter,
            }
        );
    }

    #[test]
    fn empty_placeholder_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti_prompt }}".to_string(), "{{   }}".to_string()],
            cwd: None,
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(
            err,
            LaunchError::Template {
                field: TemplateField::Arg(1),
                reason: TemplateError::EmptyPlaceholder,
            }
        );
    }

    #[test]
    fn unknown_variable_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti_prompt }}".to_string()],
            cwd: Some("{{ nonsense }}".to_string()),
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(
            err,
            LaunchError::Template {
                field: TemplateField::Cwd,
                reason: TemplateError::UnknownVariable("nonsense".to_string()),
            }
        );
    }

    #[test]
    fn invalid_identifier_placeholder_interior_is_an_unknown_variable() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti-prompt }}".to_string()],
            cwd: None,
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(
            err,
            LaunchError::Template {
                field: TemplateField::Arg(0),
                reason: TemplateError::UnknownVariable("loti-prompt".to_string()),
            }
        );
    }

    // -- standalone prompt placement/count -------------------------------------

    #[test]
    fn missing_standalone_prompt_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["no prompt here".to_string()],
            cwd: None,
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(err, LaunchError::PromptPlaceholderCount(PromptCount(0)));
    }

    #[test]
    fn embedded_prompt_occurrence_does_not_count_as_standalone() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["prefix {{ loti_prompt }} suffix".to_string()],
            cwd: None,
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(err, LaunchError::PromptPlaceholderCount(PromptCount(0)));
    }

    #[test]
    fn multiple_standalone_prompts_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec![
                "{{ loti_prompt }}".to_string(),
                "{{loti_prompt}}".to_string(),
            ],
            cwd: None,
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(err, LaunchError::PromptPlaceholderCount(PromptCount(2)));
    }

    // -- cwd default, rendering, and validation --------------------------------

    #[test]
    fn cwd_defaults_to_project_root_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let plan = prepare(
            &epic_target(),
            &minimal_profile(),
            &workflow("review"),
            &context(&root),
        )
        .unwrap();
        assert_eq!(plan.cwd, root);
    }

    #[test]
    fn relative_rendered_cwd_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti_prompt }}".to_string()],
            cwd: Some("relative/path".to_string()),
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(
            err,
            LaunchError::CwdNotAbsolute(PathBuf::from("relative/path"))
        );
    }

    #[test]
    fn nonexistent_rendered_cwd_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti_prompt }}".to_string()],
            cwd: Some(missing.to_str().unwrap().to_string()),
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(err, LaunchError::CwdNotADirectory(missing));
    }

    #[test]
    fn rendered_cwd_that_is_a_file_not_a_directory_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a-file");
        std::fs::write(&file, "x").unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti_prompt }}".to_string()],
            cwd: Some(file.to_str().unwrap().to_string()),
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(err, LaunchError::CwdNotADirectory(file));
    }

    // -- environment precedence and reserved keys ------------------------------

    #[test]
    fn environment_precedence_is_caller_then_markers_then_profile() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = context(dir.path());
        ctx.env
            .insert("FROM_CALLER_ONLY".to_string(), "kept".to_string());
        ctx.env
            .insert("OVERRIDDEN".to_string(), "caller-value".to_string());

        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti_prompt }}".to_string()],
            cwd: None,
            env: Some(BTreeMap::from([(
                "OVERRIDDEN".to_string(),
                "profile-value".to_string(),
            )])),
        };

        let plan = prepare(&ticket_target(), &profile, &workflow("review"), &ctx).unwrap();

        // Caller-only key survives untouched.
        assert_eq!(plan.env.get("FROM_CALLER_ONLY"), Some(&"kept".to_string()));
        // Profile value wins over the caller's inherited value for the same key.
        assert_eq!(
            plan.env.get("OVERRIDDEN"),
            Some(&"profile-value".to_string())
        );
        // Fresh session markers are present with the expected values.
        assert_eq!(
            plan.env.get(SESSION_ENV_VAR),
            Some(&"agent-integration-impl/2".to_string())
        );
        assert_eq!(plan.env.get(WORKFLOW_ENV_VAR), Some(&"review".to_string()));
    }

    #[test]
    fn profile_cannot_define_a_reserved_loti_env_key() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti_prompt }}".to_string()],
            cwd: None,
            env: Some(BTreeMap::from([(
                "LOTI_CUSTOM".to_string(),
                "x".to_string(),
            )])),
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(err, LaunchError::ReservedEnvKey("LOTI_CUSTOM".to_string()));
    }

    #[test]
    fn reserved_prefix_check_is_case_sensitive() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti_prompt }}".to_string()],
            cwd: None,
            env: Some(BTreeMap::from([(
                "loti_lowercase".to_string(),
                "x".to_string(),
            )])),
        };
        // Only the exact-case `LOTI_` prefix is reserved; a lowercase-prefixed
        // key is an ordinary profile value.
        let plan = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap();
        assert_eq!(plan.env.get("loti_lowercase"), Some(&"x".to_string()));
    }

    // -- cooperative session refusal -------------------------------------------

    #[test]
    fn session_marker_with_empty_value_still_counts_as_active() {
        let mut env = BTreeMap::new();
        env.insert(SESSION_ENV_VAR.to_string(), String::new());
        assert!(session_active(&env));
    }

    #[test]
    fn workflow_marker_alone_also_counts_as_active() {
        let mut env = BTreeMap::new();
        env.insert(WORKFLOW_ENV_VAR.to_string(), "some-workflow".to_string());
        assert!(session_active(&env));
    }

    #[test]
    fn no_markers_present_is_not_an_active_session() {
        assert!(!session_active(&BTreeMap::new()));
    }

    #[test]
    fn preparation_is_refused_while_a_session_marker_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = context(dir.path());
        ctx.env
            .insert(SESSION_ENV_VAR.to_string(), "other-target".to_string());
        let err = prepare(
            &epic_target(),
            &minimal_profile(),
            &workflow("review"),
            &ctx,
        )
        .unwrap_err();
        assert_eq!(err, LaunchError::SessionActive);
    }

    #[test]
    fn preparation_is_refused_while_only_a_workflow_marker_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = context(dir.path());
        ctx.env
            .insert(WORKFLOW_ENV_VAR.to_string(), "other-workflow".to_string());
        let err = prepare(
            &epic_target(),
            &minimal_profile(),
            &workflow("review"),
            &ctx,
        )
        .unwrap_err();
        assert_eq!(err, LaunchError::SessionActive);
    }

    // -- first-error ordering ---------------------------------------------------

    #[test]
    fn empty_command_is_reported_before_any_other_problem() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: String::new(),
            args: vec!["no prompt and no LOTI_ issue triggers first".to_string()],
            cwd: None,
            env: Some(BTreeMap::from([(
                "LOTI_ALSO_BROKEN".to_string(),
                "x".to_string(),
            )])),
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(err, LaunchError::EmptyCommand);
    }

    #[test]
    fn reserved_env_key_is_reported_before_template_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            // Malformed template, which would otherwise fail first if the
            // reserved-key check did not run before template parsing.
            args: vec!["{{ unterminated".to_string()],
            cwd: None,
            env: Some(BTreeMap::from([(
                "LOTI_ALSO_BROKEN".to_string(),
                "x".to_string(),
            )])),
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(
            err,
            LaunchError::ReservedEnvKey("LOTI_ALSO_BROKEN".to_string())
        );
    }

    #[test]
    fn template_parsing_is_reported_before_prompt_count() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            // No standalone prompt at all, and a malformed template: parsing
            // must win.
            args: vec!["{{ unterminated".to_string()],
            cwd: None,
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(
            err,
            LaunchError::Template {
                field: TemplateField::Arg(0),
                reason: TemplateError::UnmatchedOpeningDelimiter,
            }
        );
    }

    #[test]
    fn prompt_count_is_reported_before_rendering() {
        #[cfg(unix)]
        {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;

            let dir = tempfile::tempdir().unwrap();
            let mut ctx = context(dir.path());
            // Not valid UTF-8: rendering `current_directory` would fail, but
            // the missing standalone prompt must be reported first.
            ctx.current_directory = PathBuf::from(OsString::from_vec(vec![0x66, 0xff, 0x6f]));

            let profile = Profile {
                command: "pi".to_string(),
                args: vec!["{{ current_directory }}".to_string()],
                cwd: None,
                env: None,
            };
            let err = prepare(&epic_target(), &profile, &workflow("review"), &ctx).unwrap_err();
            assert_eq!(err, LaunchError::PromptPlaceholderCount(PromptCount(0)));
        }
    }

    #[test]
    fn rendering_is_reported_before_the_absolute_cwd_check() {
        #[cfg(unix)]
        {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;

            let dir = tempfile::tempdir().unwrap();
            let mut ctx = context(dir.path());
            ctx.env
                .insert("BROKEN".to_string(), "placeholder".to_string());
            // Force a render failure on an environment value (checked after
            // cwd in the fixed field order), while cwd itself renders fine but
            // to a relative path that would otherwise fail the absolute check.
            ctx.current_directory = PathBuf::from(OsString::from_vec(vec![0x66, 0xff, 0x6f]));

            let profile = Profile {
                command: "pi".to_string(),
                args: vec!["{{ loti_prompt }}".to_string()],
                cwd: Some("relative/cwd".to_string()),
                env: Some(BTreeMap::from([(
                    "BROKEN".to_string(),
                    "{{ current_directory }}".to_string(),
                )])),
            };
            let err = prepare(&epic_target(), &profile, &workflow("review"), &ctx).unwrap_err();
            assert_eq!(
                err,
                LaunchError::Render {
                    field: TemplateField::Env("BROKEN".to_string()),
                    reason: RenderError::NonUtf8Path(Variable::CurrentDirectory),
                }
            );
        }
    }

    #[test]
    fn absolute_cwd_is_reported_before_the_existing_directory_check() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            command: "pi".to_string(),
            args: vec!["{{ loti_prompt }}".to_string()],
            // Neither absolute nor existing; the absolute-path rule must win.
            cwd: Some("relative/and/missing".to_string()),
            env: None,
        };
        let err = prepare(
            &epic_target(),
            &profile,
            &workflow("review"),
            &context(dir.path()),
        )
        .unwrap_err();
        assert_eq!(
            err,
            LaunchError::CwdNotAbsolute(PathBuf::from("relative/and/missing"))
        );
    }
}
