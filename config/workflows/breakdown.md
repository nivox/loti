# Break down an approved specification

Turn one approved planning-map specification into one reviewed tree of
implementation tickets. This workflow owns validation, targeted research,
proposal, approval, publication, and provenance. It does not implement the
published work, invent requirements, or replace planning.

## Working rules

- Drive the tracker exclusively through `loti`; never inspect or edit store
  files directly.
- Read the target, its comments, and the selected asset before changing
  anything. Read a resource again immediately before replacing its body or
  asset data.
- Use one consistent free-form identity for claims and comments. Publish tickets
  unclaimed.
- Create no draft tickets. A rejection or refinement leaves the tracker
  unchanged until the complete detailed batch is accepted.
- Do not silently alter active, blocked, claimed, or terminal work. Ask the
  human to direct its handling.

## Entry and readiness

Accept an epic or its planning-map ticket. For an epic, locate exactly one
*top-level* ticket labelled `planning-map`; for a ticket target, it must be that
map. Otherwise stop and explain the ambiguity or invalid target without changing
tracker state.

Before analysing a new breakdown, require all of the following:

- the planning map is `done`;
- every planning decision is terminal;
- its **Planning fog** section is empty; and
- approved specification assets are attached to the map.

The map and one explicitly selected attached specification asset are the
authoritative inputs. If several assets are attached, list them and require the
human to select exactly one. One run covers one selected asset only. Ask the
human to confirm any excluded ordinary input or supplemental source, and record
approved exceptions in the root's provenance. A supplement does not override the
map or selected asset unless the human explicitly says so.

## Existing-work preflight

Before costly analysis, read the planning map, its publication comments, and
matching breakdown roots for the selected asset. Treat recorded asset and
map-update context as a staleness hint, not a source diff.

If a matching breakdown exists or might be stale, ask whether to leave it,
update it, replace it, or cancel. Do not do repository-wide divergence analysis
unless the human elects to create or update. An approved revision may add or
update only unclaimed `to-do` tickets; preserving other work is mandatory unless
the human gives explicit handling instructions.

## Research and escalation

For a new or approved update, read the selected specification, relevant planning
decisions, targeted source and configuration, documentation, tests, and existing
implementation tickets that might own a prerequisite contract. Identify affected
boundaries, conventions, integration points, verification layers, and evidence
for the proposal.

If inputs conflict, a requirement is missing, or evidence makes the outcome
infeasible, stop and return the issue to planning. State the conflict and
options, but do not invent a requirement or change scope. Record bounded
technical uncertainty as a ticket risk only when the required outcome remains
clear; name the constraint without mandating a solution.

## Shape the proposed tree

Propose one top-level implementation root for the selected asset. Its direct
children are end-to-end vertical slices or shared foundations:

- A slice owns a user-visible outcome and its final integrated verification.
- A foundation owns the smallest independently verifiable shared contract
  required now by the selected specification; possible future reuse is not
  sufficient.
- Decompose further only when independently verifiable child contracts make
  complexity safer to manage. Every parent retains the outcome it composes and
  the integration verification it alone performs.

Do not split to meet a size quota, mirror code layers, or create checklists.
Tests, documentation, configuration, migrations, and observability normally
belong to the ticket delivering their outcome, not to horizontal tickets.

Use `blocked-by` only for real prerequisite contracts. External dependencies
may point outside the new root only when an existing ticket owns the needed
capability; make each one explicit in review. Do not encode suggested order,
merge-conflict avoidance, or source proximity as a dependency.

Every non-leaf states its owned outcome, child contracts, and integration
verification. Every ticket states scoped requirements, observable verification
criteria at an appropriate layer, hard dependencies, and only the existing
source, configuration, documentation, or tests relevant to its scope. Do not
prescribe test composition, counts, names, or internal solutions. Add **Risks /
open technical questions** only for bounded technical uncertainty.

## Review, approval, and publication

Review the complete batch in this order:

1. **Tree review:** present every root, ticket, and subticket with its outcome,
   brief scope, and dependencies. Obtain agreement on the structure.
2. **Detailed review:** present the accepted tree's requirements, evidence,
   verification, risks, and provenance. Obtain acceptance of the entire batch.

If detailed feedback changes an outcome, boundary, dependency, or tree shape,
return to tree review. Publish nothing until detailed acceptance.

On acceptance, create the root and all descendants together as unclaimed
`to-do` tickets. Use concise outcome-based names, never implementation
activities, source layers, or vague phases. Label the root and slices with
`implementation` and `breakdown:<selected-asset-name>`; label foundations with
`implementation` and `breakdown:foundation`.

The root records the selected asset, planning-map reference, approved input
exceptions, and observed map/asset update context. After publication, add one
concise planning-map comment naming the selected asset, linking the root,
stating whether it was created or revised, and recording that source-update
context. The implementation tree is the actionable decomposition; the planning
map remains the authoritative planning and specification source.
