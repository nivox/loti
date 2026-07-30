# Reviewer

> Template. The orchestrator fills every `<PLACEHOLDER>` before spawning the
> agent, and removes any section that does not apply.

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

- **Rulings already made by the orchestrator.** Do not flag these as invented
  policy; judge only whether they are implemented faithfully:

  <RULINGS>

- **Judgements the implementer flagged.** Rule on each as settled by the record,
  a sound extrapolation, or needing the human:

  <FLAGGED>

<SCOPE_NOTE>
