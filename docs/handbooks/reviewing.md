# Reviewing a change

You are read-only on the repository. Your job is to decide whether the change is
right, and whether its tests could ever have failed. The second half is what
makes the review worth its cost: a test that cannot fail is worse than no test,
because it reports safety that is not there.

Read `AGENTS.md` and `docs/handbooks/implementing.md` first. The second tells you
what the implementer was asked to do, which is what you are judging against.

## Three phases, and only one of them breaks anything

| phase | what happens |
|---|---|
| 1. read and judge | read the record and the diff, run the gates once, judge adherence and practice, and cross-check every claim against the test that pins it |
| 2. break it | mutations, and nothing else |
| 3. write the verdict | cite `file:line` |

The change under review is already committed on a ticket branch, so the working
tree is where a deliberate defect is held — `git reset` puts it back exactly, and
nothing unreviewed lives there to lose. That is also the whole of the safety: it
holds only because **you never commit**, so every mutation is one `git checkout`
from gone. Only the orchestrator commits.

Two duties follow, and both are yours alone:

- **Delete every scratch file you make.** Restoring brings back tracked files; it
  cannot remove a file that was never tracked, and what you leave behind is
  taken for part of the change.
- **Flag anything in the diff that looks committed by accident** — a debug or
  scratch file, an editor artifact, a stray binary, a credential, a change to a
  file the ticket has no business touching. Staging is wholesale, so the diff is
  where an accident becomes visible, and you are the one reading it.

You report; you do not fix.

## Phase 1 — read and judge

Read the ticket **and its comments**, the recorded decisions **and their
comments**, `AGENTS.md`, and the diff:

```
git diff <target branch>...HEAD
```

**Three dots, never two.** Three dots is merge-base to HEAD: only what this
ticket added, correct even if the target branch has moved on. Two dots would
also report the target's own commits, reversed, as though the implementer had
made them — a wrong review, not a confusing one.

Test names diff the same way, which is how a silent removal is caught:
`git diff <target branch>...HEAD -- '*tests*'`.

Then run, once:

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

## Phase 2 — breaking it

Edit the working tree with your normal tools, then:

```
scripts/mutation-check.sh [--in <crate>] [--hold] "<label>"
```

**A mutation edits tracked files under `crates/`, and nothing else.** The script
refuses a tree carrying changes anywhere else rather than restoring it, because
restoring would throw away work that was never a mutation — which is what a
review run against uncommitted work would do.

`--in <crate>` runs only that crate's tests. Most mutations here change one
crate, and the whole-workspace default rebuilds every downstream test binary for
each one, which is the single largest cost in a review. Reach for it by default
and let the script protect you: a `SURVIVED` is never concluded narrowly — it
escalates to the canonical command before reporting — and a narrow `KILLED`
names only that crate's killers, which the log records so a later claim cannot
rest on a partial answer.

The script compares the tree against what the last run left, runs the suite,
classifies the result, logs it, and restores when it has an answer. Its
verdicts:

| verdict | exit | means |
|---|---|---|
| `KILLED` | 0 | a named test caught it — the behaviour is pinned |
| `SURVIVED` | 10 | nothing caught it — triage it below |
| `DID-NOT-COMPILE` | 20 | not a mutant; the tree is **left as-is** so you can repair it |
| nothing mutated | 30 | **your edit did not land** — never read this as a survivor |

The label becomes a row in `.git/loti-mutations.log`, which is the mutation
table for your verdict. Do not reconstruct that table from scrollback — by the
time you write the verdict, the early runs have left your context.

Two rules that matter more than they look:

- **Do not restore by hand and do not stack two mutations.** The script restores
  for you; two mutations at once make every later attribution wrong.
- **Exit 30 is not a result.** It means the edit did not apply. Fix it before
  concluding anything.

### Holding a tree

`--hold` leaves the tree in place instead of restoring it, so the next run is
made against it, and the run after that clears everything. That is how a test is
shown to be redundant: delete the test, hold, then run the mutation it claimed
to catch with the deletion still there — an identical verdict with one fewer
killer means the test is subsumed, and you say which test subsumes it.

Every row logs the full diff against HEAD, so a run made on a held tree shows
both the held change and the mutation, and can never be read as a run against a
pristine one. Hold one thing at a time, and only for the question in front of
you.

**Before recommending any removal, check the test is not load-bearing.** Tests
that look like internals often are: a list-completeness assertion catches a
variant silently dropped from a table, and a helper test can beat a frame test
when the frame's own rendering hides the defect — a marker overrunning its slot
passed every frame test and died only to a width assertion on the helper.

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
- **Pre-existing** — not this change's debt. **Prove it**: switch to the target
  branch (`git switch --detach <target branch>`), apply the same mutation, show
  it survives there too, then restore and switch back. Assertion is not proof.

## Redundancy

The signal is free. A run names **every** failing test, so a mutation killed by
tests at two different layers is a redundancy candidate — you get the candidates
out of the mutation log you were writing anyway. **A row run with `--in` is not
a candidate**: it only ever named one crate's killers, and the log says so.
Re-run it unscoped before calling anything redundant. Confirm a candidate with
`--hold`, above, and only when you intend to recommend a removal.

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
