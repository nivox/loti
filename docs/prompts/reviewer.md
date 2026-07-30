# Reviewer

> Template. The orchestrator fills every `<PLACEHOLDER>` before spawning the
> agent, and removes any section that does not apply.
>
> `<PRIOR_ROUNDS>` is empty on a first review. On a re-review it carries the
> block at the foot of this file, which is what keeps a second round from
> re-proving the first one's table.

---

Review `<TICKET>` in `/home/nivox/dev/loti`. The change is committed on branch
`ticket/<EPIC>-<N>`; the change under review is:

    git diff <TARGET_BRANCH>...HEAD

**Read `docs/handbooks/reviewing.md` and follow it.** It defines the three
phases, how to run a mutation, what to mutate, how to triage a survivor, how to
verify a removals ledger, and the verdict format. It is not optional.

**You never commit, and you never push.** Only the orchestrator does. You mutate
the working tree through `scripts/mutation-check.sh`, which restores it for you.

**Delete every scratch file you make before you report.** The orchestrator
restores tracked files, but nothing removes a file that was never tracked, and
what is left behind is taken for part of the change.

**Flag anything in the diff that looks committed by accident** — a scratch or
debug file, an editor artifact, a stray binary, a credential, a change to a file
the ticket has no business touching. Staging is wholesale, so the diff is where
an accident becomes visible, and you are the one reading it.

## The implementer's report

<REPORT>

## The record to judge against

Read each of these, **including the comments**:

    loti ticket show <TICKET>
    loti epic show <EPIC>
<RECORD_READS>

## Facts for this review

- Baseline at `HEAD`: `<N>` tests passing.

- **Target branch: `<TARGET_BRANCH>`.** Use the three-dot form
  `git diff <TARGET_BRANCH>...HEAD` for the change under review, never the
  two-dot form: if the target has moved on, two dots also reports *its* commits,
  reversed, as though the implementer had made them.

- **Do not mutate by hand.** `scripts/mutation-check.sh` refuses an unchanged
  tree, refuses a tree carrying anything outside `crates/`, restores after every
  answer, and writes the log that **is** your mutation table.

- **Before you mutate anything**, build and report the phase-1 cross-check: every
  flagged judgement and every "Done when" bullet against the test that pins it.
  The empty rows are what to mutate. A claim with a test named for it and no
  reason for doubt gets a line saying so, not a mutation run.

- **Rulings already made by the orchestrator.** Do not flag these as invented
  policy; judge only whether they are implemented faithfully:

  <RULINGS>

- **Judgements the implementer flagged.** Rule on each as settled by the record,
  a sound extrapolation, or needing the human. Each one also gets a row in the
  cross-check, and a judgement with no test naming it is a mutation target:

  <FLAGGED>

<SCOPE_NOTE>

<PRIOR_ROUNDS>

---

## `<PRIOR_ROUNDS>` — the re-review block

> Used only when a previous round has already passed on part of this change.
> The orchestrator pastes this in place of `<PRIOR_ROUNDS>` and fills it.

### Already accepted — do not re-open

<WHAT_PASSED>

### Already proven — do not re-run

<PRIOR_MUTATION_TABLE>

Re-run a row **only** if the remediation touched the code or the test it names.
For every other row the expected answer is judgement by reading, and a mutation
run you cannot justify against this table is waste. Say which rows you re-ran
and why.

### The remediation to judge

<REMEDIATION>

Judge that, and whether it broke or weakened anything. Nothing else.
