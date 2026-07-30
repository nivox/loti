# Reviewer

> Template. The orchestrator fills every `<PLACEHOLDER>` before spawning the
> agent, and removes any section that does not apply.
>
> `<PRIOR_ROUNDS>` is empty on a first review. On a re-review it carries the
> block at the foot of this file, which is what keeps a second round from
> re-proving the first one's table.

---

Review the uncommitted working-tree changes in `/home/nivox/dev/loti` for
`<TICKET>`.

**Read `docs/handbooks/reviewing.md` and follow it.** It defines the three
phases, the sandbox, what to mutate, how to triage a survivor, how to verify a
removals ledger, and the verdict format. It is not optional.

You are read-only on the repository.

## The implementer's report

<REPORT>

## The record to judge against

Read each of these, **including the comments**:

    loti ticket show <TICKET>
    loti epic show <EPIC>
<RECORD_READS>

## Facts for this review

- Baseline at `HEAD`: `<N>` tests passing.

- **Sandbox name: `<SANDBOX>`.** Use exactly this, and `<SANDBOX>-head` for a
  `--from-head` baseline. Do not invent a name and do not mutate outside
  `review-sandbox.sh`: the name is the build cache's key, and the log the script
  writes **is** your mutation table. Finish with `clean`, never `clean --cache`.

- **Before you mutate anything**, build and report the phase-1 cross-check: every
  flagged judgement and every "Done when" bullet against the test that pins it.
  The empty rows are what to mutate. A claim with a test named for it and no
  reason for doubt gets a line saying so, not a sandbox run.

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
For every other row the expected answer is judgement by reading, and a sandbox
run you cannot justify against this table is waste. Say which rows you re-ran
and why.

### The remediation to judge

<REMEDIATION>

Judge that, and whether it broke or weakened anything. Nothing else.
