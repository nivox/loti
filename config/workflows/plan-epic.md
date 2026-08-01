# Plan an epic

Turn one epic's loose work item into an implementation-ready set of approved
specification assets. This workflow owns planning only: it does not create,
refine, claim, or implement delivery slices. A separate breakdown workflow
turns approved specifications into vertical implementation tickets.

## Working rules

- Drive the tracker exclusively through `loti`; never inspect or edit store
  files directly.
- Read the target and its comments before changing anything. Read a ticket or
  epic again immediately before replacing its body.
- Use one human question at a time for every `planning:discussion` decision.
  Do not answer on the human's behalf.
- Choose a clear free-form identity for your own claims and comments, such as
  `agent`, `research-agent`, or `review-agent`. Use it consistently for this
  session. Do not claim work that the human must perform.
- Planning tickets are direct children of the planning map. Express ordering
  with `blocked-by`, never with nested planning tickets.
- Never create delivery or implementation tickets. Planning ends in approved
  specification assets.

## Planning-map model

An epic has exactly one top-level ticket labelled `planning-map`. Its default
name when this workflow creates it is `Plan: <epic name>`. Its body is the
planning index:

```markdown
## Destination

<the outcome this planning phase must make implementation-ready>

## Notes

<domain context, constraints, and planning conventions>

## Decisions so far

<one linked line per resolved planning ticket>

## Planning fog

<in-scope questions not yet precise enough to become tickets>

## Out of scope

<work consciously excluded from this planning effort>
```

The index is not a duplicate decision store. Detailed answers and evidence live
in the resolved planning tickets. Update the index whenever a decision resolves
or fog changes.

Every planning decision ticket has exactly one of these labels:

- `planning:research` — investigate independently and record evidence.
- `planning:prototype` — make a rough artifact to obtain human feedback; do not
  resolve it before that feedback.
- `planning:discussion` — resolve a human decision through one-question-at-a-
  time discussion.
- `planning:task` — prerequisite work. Do it only when you can actually do it;
  otherwise leave it unclaimed for the human and state the required action
  clearly in the ticket.

## Entry and validation

Determine whether your bootstrap target is an epic or ticket.

### Epic without a planning map

1. Run `loti epic show <epic-id>` and inspect its comments.
2. List only its immediate tickets with `loti ticket list <epic-id> --shallow
   --json`. Count top-level tickets carrying `planning-map`.
3. When there is no map, begin a human-led exploration of the loose idea in the
   epic. Establish the planning destination and breadth-first initial questions
   one at a time. Do not turn vague guesses into tickets.
4. Create a top-level ticket named `Plan: <epic name>` with the standard index
   body, add the `planning-map` label, and set it to `in-progress`.
5. Create the initial precise direct-child planning tickets, then add their
   known `blocked-by` dependencies. Keep unresolved but still-vague work in
   Planning fog.
6. Stop after charting this initial frontier. Do not resolve a planning decision
   in the same session.

### Epic with a planning map, or a planning-map target

1. Locate the unique top-level `planning-map` ticket. A ticket target is valid
   only when it is that map or a direct child of it carrying one planning label.
2. If the epic has two or more top-level planning maps, stop and explain the
   ambiguity. Do not choose one.
3. Validate the map's five index sections and the direct-parent/label rule for
   its planning decisions. If anything is inconsistent, explain the exact
   problem and offer a repair; make no repair without the user's confirmation.
4. If the target is a direct planning decision, work only that ticket and stop
   after it resolves or becomes blocked.
5. If the target is the map, follow the frontier rules below.

### Any other ticket target

Stop and tell the user that the target is neither a planning map nor one of its
direct planning decisions. Do not change tracker state.

## Continuing a completed map

A completed planning map is not immutable. First ask the user which intent they
have before changing anything:

1. **Continue planning** — new fog requires more decisions. Set the map to
   `in-progress`, record the new fog, then create precise planning tickets only
   after the corresponding question is sharp.
2. **Prepare or revise specifications** — draft the requested specification
   assets from resolved decisions and relevant planning-map comments.
3. **Recreate specifications** — prepare a replacement when an expected asset
   is absent.
4. **Cancel** — leave the tracker untouched.

Do not reopen a completed map merely to revise an existing asset. Asset-only
revision may leave the map `done`.

## Choosing work from a map

The planning frontier is the map's direct child planning decisions that are
`to-do`, unclaimed, and have only terminal blockers.

Choose the eligible ticket expected to clear the most fog or provide the most
insight into other decisions. If you deliberately choose a ticket other than
the lowest-numbered eligible ticket, add a brief map comment naming it and the
expected insight. If none clearly has greater leverage, choose the lowest-numbered
eligible ticket.

When no ticket is eligible:

- If Planning fog remains, conduct one-question-at-a-time human-led sharpening
  until one item becomes precise enough to create as a direct planning ticket,
  or move it to Out of scope. Stop after creating the new frontier.
- If every decision is terminal and no fog remains, ask whether the user wants
  to draft the specification assets now or in another session.
- If work is blocked, claimed by another worker, or waiting for a human-owned
  task, only tell the user what is waiting and why. Do not add a comment or make
  any tracker mutation.

## Resolving one planning decision

For agent-owned work:

1. Take the ticket claim using your chosen identity and set it to `in-progress`.
2. Resolve the question according to its planning label. Keep human decisions
   interactive and obtain the required prototype feedback before resolving a
   prototype.
3. Add a resolution comment with the answer, rationale, evidence, and any
   resulting constraints.
4. Re-read and update the planning map: append a concise linked entry to
   Decisions so far; remove or reshape cleared fog; create newly precise direct
   planning tickets; and add dependencies after all needed tickets exist.
5. Mark the resolved ticket `done` and release your claim.
6. Stop. Do not select a second decision in the same session.

For human-owned `planning:task` work, leave the ticket unclaimed and `to-do`.
State the concrete human action in its ticket, then stop until the human has
claimed and resolved it.

## Specifications and publication

When planning has no remaining fog and all planning decisions are terminal, ask
whether the user wants to draft specifications now. The user and agent decide
how many specification assets are needed and their names; do not impose a
single-file structure.

Prepare the complete draft batch first. Present the entire batch for human
review. The user has one accept-or-refine decision for the batch; do not add or
update any asset until the whole batch is accepted.

After acceptance, for every specification asset:

1. add it when absent, or update the same-named asset when revising it;
2. add a planning-map comment naming the asset, whether it was added, updated,
   or recreated, a one-line scope statement, and any relevant trigger; and
3. after publishing the batch, add one epic comment pointing to the planning
   map and stating which specification assets were added or updated.

The planning map is the source of truth for specifications. The epic comment is
only a pointer; it does not restate specification content.

When all planning decisions are terminal, Planning fog is empty, and the
required approved specification assets are attached, mark the planning map
`done`. If the user defers drafting, leave it `in-progress` and unclaimed.
