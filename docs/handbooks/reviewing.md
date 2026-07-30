# Reviewing a change

You are read-only on the repository. Your job is to decide whether the change is
right, and whether its tests could ever have failed. The second half is what
makes the review worth its cost: a test that cannot fail is worse than no test,
because it reports safety that is not there.

Read `AGENTS.md` and `docs/handbooks/implementing.md` first. The second tells you
what the implementer was asked to do, which is what you are judging against.

## Three phases, and only one of them uses a sandbox

| phase | where | what happens |
|---|---|---|
| 1. read and judge | working directory | read the record and the diff, run the gates once, judge adherence and practice, and cross-check every claim against the test that pins it |
| 2. break it | sandbox | mutations, and nothing else |
| 3. write the verdict | working directory | cite `file:line` from the working tree |

**The sandbox exists to hold a deliberate defect. If you are not currently
holding one, you should be in the working directory.** Phase 1 runs against the
repository's warm build cache, so the gates take seconds; a review that never
reaches a claim worth attacking never creates a sandbox at all.

Never modify the working tree. Confirm at the end that `git status` is unchanged
and nothing is staged. You report; you do not fix.

## Phase 1 — read and judge

Read the ticket **and its comments**, the recorded decisions **and their
comments**, `AGENTS.md`, and the diff. Then run, once:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Judge:

- **Every "Done when" bullet**, individually, with evidence.
- **Fidelity to the record.** Flag anything that invents policy the decision did
  not grant. Rule explicitly on each judgement the implementer flagged: settled
  by the record, a sound extrapolation, or **needs the human**.
- **Standing rules** — the seam module, the key-naming module, whichever the
  ticket names.
- **`AGENTS.md` on the changed lines.** Comments must state a rule that can be
  checked against reality; a comment asserting an invariant the code does not
  enforce is a defect, not a nicety. No spec sections or ticket ids in source or
  in anything the program prints.
- **Partial failure.** If the store refuses, is anything already changed on
  screen or in memory? Is anything left half-done?
- **The removals ledger** (below).
- **Test names and bodies against `HEAD`.** Every disappearance must appear in
  the ledger.

### The cross-check that decides phase 2

Before you leave phase 1, build one table: **every flagged judgement and every
"Done when" bullet, against the claims-table row that pins it.** Report it.

The rows that come back empty are the review. A judgement the implementer had to
make, with no test naming it, is where a defect survives — that is where the last
must-fix was found, and it was found by reading this table, not by mutating.
Mutation then proved it in one run.

Come out of phase 1 with that table. The empty rows are phase 2.

## Phase 2 — the sandbox

```
scripts/review-sandbox.sh init <the name you were given>
cd /tmp/loti-sandbox-<that name>
```

**Use the name you were given, and do not invent one.** The name is the build
cache's key: one name serves every review round on a body of work, so a later
round starts warm instead of paying a cold build. A `--from-head` baseline goes
in `<that name>-head`, so the same rule covers it. If you were given no name,
ask — do not pick.

Then, for each mutation: edit the sandbox with your normal editing tools, and

```
/path/to/repo/scripts/review-sandbox.sh check [--in <crate>] "<label>"
```

`--in <crate>` runs only that crate's tests. Most mutations here change one
crate, and the whole-workspace default rebuilds every downstream test binary for
each one, which is the single largest cost in a review. Reach for it by default
and let the script protect you: a `SURVIVED` is never concluded narrowly — it
escalates to the canonical command before reporting — and a narrow `KILLED`
names only that crate's killers, which the log records so a later claim cannot
rest on a partial answer.

`check` compares against the baseline, runs the suite, classifies the result,
logs it, and restores the baseline when it has an answer. Its verdicts:

| verdict | exit | means |
|---|---|---|
| `KILLED` | 0 | a named test caught it — the behaviour is pinned |
| `SURVIVED` | 10 | nothing caught it — triage it below |
| `DID-NOT-COMPILE` | 20 | not a mutant; the tree is **left as-is** so you can repair it |
| nothing mutated | 30 | **your edit did not land** — never read this as a survivor |

The label becomes a row in `mutations.log`, which is the mutation table for your
verdict. Do not reconstruct that table from scrollback.

Two rules that matter more than they look:

- **Do not reset by hand and do not stack two mutations.** `check` restores for
  you; two mutations at once make every later attribution wrong.
- **Exit 30 is not a result.** It means the edit did not apply. Fix it before
  concluding anything.

Finish with `clean`. It keeps the build directory so the next round under this
name starts warm; the orchestrator drops it with `discard <name>` once the
ticket is done. Do not `clean --cache` yourself — you cannot know whether
another round is coming.

## What to mutate

**Mutation confirms; reading discovers.** Spend the budget in this order, and
stop once the first three are exhausted:

1. **Claims with no named test** — every empty row of the phase-1 cross-check,
   plus any invariant a comment asserts that no test mentions. Only these can
   produce a must-fix.
2. **Every removals-ledger entry.** Required without exception: a claim may be
   moved, but never lost silently.
3. **The change's own thesis** — the mutation that puts back the behaviour the
   ticket exists to remove. Usually one or two. They are the reason a `PASS`
   means anything.
4. **Anything you doubt** — a claim whose named test you cannot convince
   yourself would actually fail, and the **failure paths**: the refusal, the
   missing file, the editor that is not installed, where guarantees are stated
   and rarely exercised.

**Do not mutate a claim that has a test named for it and no reason for doubt.**
Write one line — "pinned by `<test>`, not mutated" — and move on. A run of
consecutive `KILLED` verdicts in category 4 means you are measuring the suite
rather than the change: stop there.

The implementer runs no mutations, so there are none of theirs to repeat. If a
report contains any, treat them as claims to check by reading, not as
experiments to redo.

### Six shapes that find things

1. **Delete a call or a guard.** Does anything notice the line is gone?
2. **Boundary and operator swaps.** `<` → `<=`, `max` → `min`, first → last.
3. **Polarity swaps.** Two symmetric operations transposed: enter for leave,
   enable for disable, forwards for backwards.
4. **Stub a return value.** Make a function answer trivially. If the suite is
   green, nothing depended on the real answer.
5. **Redirect a write.** Aim it at the neighbouring entity or container. The
   failure mode is silent data loss, not a crash.
6. **Reword user-facing text, or shorten a data list.** Prose is behaviour when
   it teaches the reader something, and a list a surface walks is behaviour when
   dropping an entry silently drops a case.

## Triaging a survivor

A survivor is not automatically a defect. Decide which of three it is, and say:

- **Real gap** — reachable in production. Name the reachable path. This is a
  must-fix.
- **Unreachable** — defence in depth that nothing can currently reach. Say why,
  and leave it.
- **Pre-existing** — not this change's debt. **Prove it**: `init <name>-head
  --from-head`, apply the same mutation there, and show it survives at `HEAD`
  too. Assertion is not proof.

## Redundancy

The signal is free. `check` names **every** failing test, so a mutation killed by
tests at two different layers is a redundancy candidate — you get the candidates
out of the mutation log you were writing anyway. **A row run with `--in` is not
a candidate**: it only ever named one crate's killers, and the log says so.
Re-run it unscoped before calling anything redundant.

Only confirm a candidate when you intend to recommend a removal:

```
# delete the suspect test from the sandbox
scripts/review-sandbox.sh hold          # this tree becomes the baseline
scripts/review-sandbox.sh check "<the mutation it claimed to catch>"
```

Identical verdict with one fewer killer means the test is subsumed; say which
test subsumes it. Every later result prints `baseline: pristine + N held
changes`, so a held baseline can never be read out of context. `hold` refuses a
tree that does not build, and is only ever for removing a test — for a different
starting point, `init` a new sandbox.

**Before recommending any removal, check it is not load-bearing.** Tests that
look like internals often are: a list-completeness assertion catches a variant
silently dropped from a table, and a helper test can beat a frame test when the
frame's own rendering hides the defect — a marker overrunning its slot passed
every frame test and died only to a width assertion on the helper.

## The removals ledger

The implementer may delete or rename a test if the claim it carried is pinned
elsewhere. Each removal is declared with its claim and its new home. Your job:

- **For each entry**, mutate the behaviour the removed test claimed to pin and
  require the **named** new home to kill it. An entry whose mutation survives is
  a must-fix.
- **Diff test names against `HEAD`.** Any disappearance not in the ledger is a
  must-fix even if coverage happens to be intact — the rule is that no claim is
  lost *silently*.
- A removal justified by "covered elsewhere" without naming the test is not a
  ledger entry.

## Phase 3 — the verdict

```
VERDICT: PASS
```
or
```
VERDICT: FAIL
```

Then, in order:

- **The phase-1 cross-check**: every flagged judgement and "Done when" bullet
  against the test that pins it, empty rows marked.
- **Must-fix items**, each with `file:line` in the working tree and the mutation
  that proves it. Nothing is a must-fix without evidence.
- **The mutation table**, from `mutations.log`, including every survivor and its
  triage — and the claims you deliberately did not mutate, one line each.
- **Followups**, each marked *blocks* or *does not block*, and for the ones that
  block, which tickets and why.

A `PASS` says the tests would have caught the defects you could invent. Say what
you tried, so the next reviewer knows what has already been ruled out.
