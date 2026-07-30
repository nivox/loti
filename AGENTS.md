# AGENTS.md

Conventions for agents (and humans) working in this repository.

## Handbooks

Work here is split between an implementer and an independent reviewer, and each
has a handbook under `docs/handbooks/`. Read yours before you start:
`implementing.md` covers shaping code so its boundaries are observable, which
behaviour is pinned at which layer, and what to report; `reviewing.md` covers
running mutations in a sandbox and judging whether the tests could ever have
failed. The rules below bind regardless of which you are.

## Comments & documentation

Comment the **why** and the **invariant**, never the obvious **what**. A comment
should state a rule the code enforces, so it can be checked against reality.

- **Do** encode the rule/invariant in prose:
  `// comment mutations require an actor`
  `// body comes from stdin/--file only, never an inline flag`
- **Don't** narrate the code (`// increment i`) or restate a signature.

## Never reference volatile pointers in source

Source (including doc comments and any user-facing output) **must not** reference:

- **Specs** — e.g. `SPEC §4`, `§7`. Section numbers drift as the
  document changes, leaving stale pointers.
- **Local tracker tickets** — e.g. `loti-impl/2`, ticket ids/names. Tickets are
  transient planning artifacts, not durable code documentation.

Instead, **write the rule or invariant itself**. A stated rule can be matched
against the live spec at any time; a section pointer cannot.

- Bad:  `/// Store format-version (SPEC §9). Bumped by later tickets.`
- Good: `/// Store format-version as (major, minor). A store major newer than`
        `/// the binary is refused; an older major is read-only until migrated.`

Cross-references to the specs/tickets belong in **tracker tickets and their
resolutions**, not in the source tree.

What is acceptable to reference:
- documentation under `docs/`
- external issues on public issue trackers (GitHub issue/JIRA)

## Program output

The same rule applies to anything `loti` prints (help text, errors, messages):
no spec sections, no ticket ids. Describe behaviour and rules in plain terms an
end user understands.
