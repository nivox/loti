# Implementer

> Template. The orchestrator fills every `<PLACEHOLDER>` before spawning the
> agent, and removes any section that does not apply.

---

Implement `<TICKET>` in `/home/nivox/dev/loti`.

**Read `docs/handbooks/implementing.md` and follow it.** It defines how to shape
code so its boundaries are observable, which behaviour is pinned at which layer,
what to report, and what not to do. It is not optional.

## The ticket, and the record it implements

Read each of these, **including the comments** — a decision's resolution usually
lives in a comment rather than in the body:

    loti ticket show <TICKET>
    loti epic show <EPIC>              # standing rules for every ticket here
<RECORD_READS>

Where the record and this prompt disagree, **the record wins**. Say so rather
than choosing.

## Already built — extend rather than reinvent

<EXISTING_MECHANISMS>

## Not in scope

<OUT_OF_SCOPE>

## When you are done

Report as the handbook specifies: files changed, the claims table, the removals
ledger if anything went, judgements you made where the record was silent, stale
documentation you noticed, and followups each marked *blocks* or *does not
block*.

Do not commit. Do not change ticket status. Do not run mutation testing — that
is the reviewer's, and duplicating it has never changed a verdict.

**Delete every temporary file you made.** The orchestrator commits the whole
tree, so anything left behind ships as though you meant it. New source and test
files are welcome; scratch is not. Check `git status --porcelain` before you
report, and list every path you touched.

If the record does not settle something you must choose, stop and report
`DECISION NEEDED` rather than guessing, or ask the supervisor.
