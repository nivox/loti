# Implementing a ticket

You implement one ticket and prove it. Shape the code so its boundaries are
observable without back doors, then pin each behaviour once, at the boundary
that owns it. Breaking your tests is the reviewer's job — you do not mutate.

Read `AGENTS.md` first; it binds. This handbook is how to satisfy it.

## Read the record before the prompt

Design decisions live in the planning epic's tickets **and in their comments** —
the resolution is usually a comment, not the body. Read both.

**The recorded decision beats the task prompt.** A prompt is a paraphrase written
by someone who is not looking at the record; the record is the decision. Where
they conflict, stop and say so rather than picking one. Where the record is
silent on something you must choose, choose, and report it as a judgement so the
reviewer can rule on it. Where you cannot choose without inventing product
policy, stop and report `DECISION NEEDED`.

## Designing for observable boundaries

A behaviour you cannot observe from outside is a behaviour you cannot pin, and no
amount of test-writing fixes it. Shape the code first.

- **One module owns each external dependency.** The seam is where the outside
  world enters, so a test substitutes the world in one place instead of in every
  caller.
- **A side effect you cannot observe cannot be pinned.** Make two things visible,
  because the first alone is not enough: the *sequence* of an effect (an injected
  recorder proves order and completeness) and its *payload* (a sink proves what
  was actually emitted). A guarantee whose steps are observable but whose real
  implementation is not can be gutted with every test still green.
- **Prefer a compile error to a test.** Exhaustive matches rather than wildcards;
  derive one direction of a mapping from the other rather than writing both;
  enforce an invariant with visibility rather than with a comment. A rule that
  can be a type should not be a test.
- **One decision point, not two that must agree.** If a key's effect and its hint
  come from one function they cannot drift. If they are two functions plus a test
  that they match, they will drift and the test will be what tells you.
- **Push effects to the edge; keep the decision pure.** A lookup that reads the
  environment inside itself is untestable. The same lookup as a function of its
  inputs is trivially testable, and the effect shrinks to one call site.
- **Do not cache derived state.** Recompute from the source of truth so two
  readers cannot disagree. A value captured when a surface opens is a divergence
  waiting for the thing it derives from to change.
- **Seam or back door?** A seam has a production reason and the implementation
  that ships is the one it names. A test-only setter, or a `cfg(test)` mutator on
  a production type, is a back door: if you need one, the design is wrong, and
  that is worth reporting rather than working around.

If a behaviour is hard to test at its boundary, fix the boundary, not the test.

## Pin each behaviour at exactly one layer

The layer is chosen by what the claim is about:

| the claim is about | pin it in | drive it with |
|---|---|---|
| what the store holds | the core-seam tests | the shared fixture, against a real store |
| what the reader sees or types | the drawn-frame tests | public intents, asserted on terminal cells |
| which intent a key carries | the key-table tests | key plus mode, asserted on the intent |
| the state machine's own transitions | the intent-level tests | public intents, asserted on state |

**An intent-level test must not re-assert store contents or rendered output.** It
is the right layer only for a claim no boundary can express: a frozen selection,
mode layering, a sticky flag, a lifetime, or a sequence spanning several intents.

### The trap: the write-outcome triple

A write invites three tests, and two of them are the same test:

- the seam test — *the named entry went, and no other* ✅ keep
- the frame test — *the notice names it and the mode ended* ✅ keep
- the intent test — *the store changed* (the seam already said so) *and the mode
  ended* (the frame already said so, on real cells, which is stronger) ❌ drop

If you find yourself asserting the store from the intent layer, or the notice
text from two layers, you are writing this triple. Pick the layer that owns the
claim and delete the other assertion rather than weakening it.

## Vacuity smells

Each of these passes while pinning nothing. All have shipped here at least once.

- **`is_some()` where the value is the claim.** A stale message from an earlier
  action satisfies it. Assert the message.
- **A substring that appears elsewhere on the line.** A frame line spans both
  panes, so an author's name in the preview satisfies an assertion about the row.
  Bound the search to the pane.
- **Comparing against the function under test.** Tautological. Derive the
  expectation from the layer below — the operation, not the wrapper.
- **A fixture-shape constant.** `assert_eq!(len, 2)` breaks when the fixture
  grows and proves nothing when it does not. Derive the count from the store.
- **A claim the fixture cannot distinguish.** One epic cannot prove that a
  cross-epic reference reaches another epic; one required field cannot prove the
  warning names the *first* one. Give the fixture the case that separates them.
- **A helper that normalises the defect away.** Trimming trailing space hides a
  misaligned marker. Read the raw buffer when the claim is about position.
- **Asserting a value is carried when the claim is that it is applied.** Follow
  through: dismiss, then type, then check where the text landed.

## Removals ledger

You may delete or rename a test. **Every removal is declared**: the claim it
carried, where that claim is pinned now, and why the new home is at least as
strong. An undeclared removal is a defect even with a green suite.

A removal is justified by subsumption only, and the ledger must name the
subsuming test. Never remove a test to make a failing suite pass.

Report the test count as an observation with an explanation — "211 → 204, seven
removals, all in the ledger" — never as a floor to defend.

## Do not mutate

Mutation testing belongs to the reviewer. Do not set up scratch trees, do not
run mutation campaigns, do not report mutation tables. Duplicating it wastes the
run and has never once changed a verdict. What you owe instead is the claims
table below, which tells the reviewer exactly what to attack.

## Leave behind exactly what should be committed

You do not commit — the orchestrator does, and it stages **everything** in the
tree, tracked and new alike. So what you leave is what ships.

- **New files are welcome.** A new module or a new test file is committed like
  any other change; you do not have to work around anything.
- **Delete every temporary file you made.** A scratch fixture, a debug binary, a
  notes file, a copy of something you were comparing against. Nothing
  distinguishes them from work you meant to keep, so anything left behind is
  committed as though you intended it.
- The tree was clean when you started, so everything in it at the end is
  attributable to you. Check with `git status --porcelain` before you report,
  and list every path in your report so the orchestrator can cross-check.

## What to report

- files changed
- **claims table**: each new test, the one behaviour it pins, and its layer
- **removals ledger**, if anything went
- judgements you made where the record was silent
- anything in `docs/` your change makes stale (do not fix it unless the ticket
  says so; name it so it is not lost)
- **followups**, each marked *blocks* or *does not block*, with one line of why
- `DECISION NEEDED` for anything you could not settle from the record

## Before you hand over

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Draw real frames through the headless backend and **read them**. Assertions
encode what you expected; the frame shows what you built. Every slice that has
done this found something the assertions missed.

Do not commit, and do not change ticket status. That is the orchestrator's.
