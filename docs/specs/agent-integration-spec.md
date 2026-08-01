# Agent integration specification

This document is **normative** (MUST/SHOULD/MAY per RFC 2119). It specifies
configured external-agent profiles, Markdown workflows, cooperative launched
agent sessions, and foreground launch behaviour. The command grammar and
output-format conventions are in [cli-spec.md](cli-spec.md); the browser
contract is in [tui-spec.md](tui-spec.md).

## 1. Scope

Loti launches one selected external agent harness in the foreground for one
existing epic or ticket. It is a terminal handoff, not job management:

- no agent session is detached, supervised, retained, reattached, or logged by
  loti;
- a launch MUST NOT create or change a claim, status, comment, or any other
  tracker data; and
- a workflow MAY instruct an agent or human to make tracker changes separately.

Profiles describe how to invoke a harness. Workflows are the human-authored
instructions for the work. Loti does not define harness-specific concepts such
as a model, sandbox, or approval policy; a profile may express harness-specific
arguments directly.

## 2. Effective resources

### Roots and discovery

Agent profiles and workflows each have an optional project-local root and a
user-global root:

| Resource | Project configuration key | Global root |
|---|---|---|
| agent profile | `agent-root` | `$XDG_CONFIG_HOME/loti/agents` |
| workflow | `workflow-root` | `$XDG_CONFIG_HOME/loti/workflows` |

When `XDG_CONFIG_HOME` is not set, the normal XDG configuration-home equivalent
is used. Loti finds project configuration by walking upward from the store and
uses the nearest `.loti.conf`. A configured local root is absolute or relative
to that config file and MUST resolve to an existing directory. An absent key
means that resource kind has no local root; a malformed configuration or broken
configured root is an error.

Discovery is shallow. Only direct children with an exact lower-case `.toml`
extension are profile candidates, and only direct children with an exact
lower-case `.md` extension are workflow candidates. Nested paths and unrelated
or mixed-case-extension entries are ignored.

A resource ID is its filename stem. IDs MUST be non-empty and contain only ASCII
letters, digits, hyphens, and underscores. IDs are case-sensitive; definitions
whose IDs differ only by case are distinct, though they are non-portable on
case-insensitive filesystems.

A local candidate with the same raw filename stem as a global candidate shadows
it **before** validation. Thus an unreadable, malformed, or invalid local file
is the effective resource and MUST be reported as such; loti MUST NOT fall back
to the global definition. Effective resource lists are sorted by bytewise
lexical ID.

### Diagnostics and read commands

`loti agent list` and `loti workflow list` list every effective candidate,
including invalid ones. Each resource row contains exactly:

- `id`;
- `origin`, either `local` or `global`; and
- any diagnostics.

A diagnostic is either a warning, which leaves a resource usable, or an error,
which makes it unusable. The lists expose only those three fields; their plain,
JSON, NDJSON, and raw formats follow the CLI's ordinary resource-list rules.

`loti agent show <id>` shows one usable effective profile. Its default is
Markdown; JSON is the canonical parsed value; raw output permits the normal
unambiguous leaf projections. An invalid selected profile reports its
diagnostic, while an absent profile reports not found.

`loti workflow show <id>` writes one usable effective workflow's valid UTF-8
Markdown source exactly as loaded: no front matter interpretation, wrapper,
interpolation, normalization, or added trailing bytes. An invalid selected
workflow reports its diagnostic, while an absent workflow reports not found.

## 3. Profile format

A profile is a TOML document with this recognized shape:

```toml
# Required: executable name or path. Loti executes it directly.
command = "pi"

# Required: ordered argument templates. One complete element must be the
# bootstrap placeholder exactly once.
args = ["{{ loti_prompt }}"]

# Optional: defaults to {{ project_root }}. Its rendered value must be an
# absolute existing directory.
cwd = "{{ current_directory }}"

# Optional: rendered values added after the inherited environment and loti's
# own session markers. Keys beginning with LOTI_ are forbidden.
[env]
PI_SESSION_NAME = "loti-{{ loti_ref }}-{{ loti_workflow }}"
```

`command` is a required literal string. `args` is a required ordered array of
literal/template strings. `cwd` is an optional string. `env` is an optional map
of string values. A missing recognized field or a recognized field of the wrong
type makes the profile invalid. Unknown top-level fields are ignored with a
warning, not an error.

Loti executes `command` and the rendered argument vector directly; it MUST NOT
invoke a shell or perform shell quoting, expansion, evaluation, conditionals, or
environment lookups.

### Templates

Only `args`, `cwd`, and environment **values** are templates. `command` and
environment keys are literal. A template may interpolate these six placeholders,
with optional whitespace inside the braces:

| Placeholder | Rendered value |
|---|---|
| `{{ loti_prompt }}` | generated target-specific bootstrap instruction |
| `{{ project_root }}` | store root |
| `{{ current_directory }}` | directory from which loti was invoked |
| `{{ loti_ref }}` | epic ID or ticket reference |
| `{{ loti_ref_name }}` | target display name |
| `{{ loti_workflow }}` | selected workflow ID |

An unknown or empty placeholder, or an opening `{{` without a closing `}}`, is a
launch-preparation error. A single `{` or `}` is ordinary literal text.

Exactly one `args` element MUST consist solely of `{{ loti_prompt }}` (allowing
interior placeholder whitespace). An occurrence embedded in a longer argument
does not meet this rule. This ensures the complete bootstrap instruction reaches
the harness as one argument, while profiles remain free to put it where that
harness expects an initial prompt.

After rendering, `cwd` defaults to `project_root` when absent and MUST be an
absolute directory that exists. Profile environment keys beginning with `LOTI_`
are forbidden. The child environment is composed in this order:

1. the caller environment;
2. loti's cooperative session markers; then
3. rendered profile environment values.

Later stages overwrite earlier ordinary values. The reserved-key rule prevents a
profile from overwriting a session marker.

## 4. Workflows and launched sessions

A workflow is opaque Markdown. It is not a profile and it has no loti-defined
front matter, schema, or template expansion.

For a selected epic, the bootstrap instruction MUST be:

```text
You are working as an agent in the loti workflow "<workflow-id>" on epic "<epic-id>" (<epic-name>).

Before acting:
1. Run `loti skill` to learn how to operate loti.
2. Run `loti workflow show <workflow-id>` to read your instructions.
3. Run `loti epic show <epic-id>` to fill your context.

Follow the instructions in the named workflow; this is your main goal.

IMPORTANT: If the `loti` command is not available, stop IMMEDIATELY and notify the user. Do not try to circumvent or fix the issue.
```

For a selected ticket, `epic "<epic-id>" (<epic-name>)` becomes `ticket
"<ticket-ref>" (<ticket-name>)`, and the third command is `loti ticket show
<ticket-ref>`.

A child receives these markers:

```text
LOTI_AGENT_SESSION=<target reference>
LOTI_AGENT_WORKFLOW=<selected workflow ID>
```

The markers are cooperative guardrails, not an adversarial security boundary; a
child can change its own environment. Their presence, including an empty value,
means the process is in a cooperative agent session:

- the operator-facing `agent` namespace is unavailable, including recursive
  `agent run`;
- if `LOTI_AGENT_WORKFLOW` is present, `workflow list` returns only that named
  workflow and `workflow show` treats every other ID as not found; and
- `LOTI_AGENT_SESSION` by itself does not narrow workflow access.

`loti skill` remains general. It directs a launched agent to follow its named
workflow and not to use operator-facing profile commands.

## 5. Foreground launch

The explicit CLI form is:

```text
loti agent run <epic-id> --agent <profile-id> --workflow <workflow-id>
loti agent run <epic-id>/<ticket-number> --agent <profile-id> --workflow <workflow-id>
```

Both selections are required and have no defaults. A target containing `/` is a
ticket reference; another target is an epic ID. The target and both effective
resources MUST exist and be usable.

Before handing over the terminal, the CLI MUST, without mutating tracker state:

1. refuse a cooperative agent-session caller;
2. require stdin, stdout, and stderr to all be terminals;
3. open and resolve the target;
4. resolve the selected effective profile and workflow; and
5. render and validate the launch plan.

A preflight failure launches no child and changes neither tracker data nor the
current CLI/TUI surface. On Unix, a successful CLI launch replaces the loti
process with the prepared command. The replacement inherits the terminal
streams and its exit status becomes the command's exit status. Non-Unix builds
refuse rather than emulate process replacement with a wrapper child.

## 6. TUI handoff

In `loti tui`, the editing-mode `w` action is offered only for a selected epic
or ticket. It opens the existing centred editing surface titled for the frozen
target. The surface has two pickers in this order: `workflow`, then `agent`.
Each choice shows its effective resource ID and origin.

Ordinary picker movement, field focus, cancellation, and acceptance apply. An
ordinary accept launches immediately; there is no confirmation dialog. The TUI
validates a selected launch before releasing the terminal. A validation failure
appears in a dialog while preserving the picker.

For a prepared launch, the browser releases mouse capture, the alternate screen,
and raw mode; runs the child with inherited standard streams; then restores raw
mode, the alternate screen, and mouse capture and repaints. Restoration and
repaint MUST happen after any child exit or spawn failure. A non-zero exit or
spawn failure is reported only after the browser has been restored, in a
dismissible notice naming the selected profile and the failure. A successful
agent exit changes no tracker state.
