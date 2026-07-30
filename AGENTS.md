# AGENTS.md

Conventions for agents (and humans) working in this repository.

## Handbooks and prompts

Work here is split between an orchestrator, an implementer and an independent
reviewer.

The two working roles each have a handbook under `docs/handbooks/`. Read yours
before you start: `implementing.md` covers shaping code so its boundaries are
observable, which behaviour is pinned at which layer, and what to report;
`reviewing.md` covers running mutations and judging whether the tests could ever
have failed. The rules below bind regardless of which you are.

Only the orchestrator commits, and it commits the whole tree. Everything else
follows from that: work on the ticket branch it opened, delete every temporary
file you make, and leave behind exactly what should ship.

`docs/prompts/` holds one prompt per role. An orchestrator is started on a loti
reference with `orchestrator.md`, and spawns each subagent from `implementer.md`
or `reviewer.md`, filling in the placeholders. A prompt points at the record and
never restates it — a decision paraphrased from memory is how an agent ends up
building the opposite of what was agreed.

## Run the tests under a one-minute timeout

Every test command gets `timeout 60` in front of it:

    timeout 60 cargo test --workspace

The whole suite runs in about six seconds, and its slowest single binary in under
two, so a minute is ten times the headroom a green run needs. A run that reaches
the ceiling is therefore not a slow machine: it is a test that is stuck, and by
far the likeliest cause is a loop driving an operation until a condition holds
where the operation has gone inert. That failure is silent — no output, no
failing assertion, one core at full tilt — so it costs far more to diagnose than
the timeout costs to type.

Compiling is not testing. If the build is cold, build first and time the tests
separately:

    cargo test --workspace --no-run     # untimed; a cold dependency graph is slow
    timeout 60 cargo test --workspace

**A timeout is an answer, not bad luck.** Never re-run the same command hoping
for a different result. Narrow it instead — one test, one thread, output shown:

    timeout 15 cargo test -p <crate> <test-name> -- --nocapture --test-threads=1

And where a test drives an operation until something becomes true, bound the
loop: a helper that gives up after a bound and says what it was waiting for
turns a hang into a failure that names itself.

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
