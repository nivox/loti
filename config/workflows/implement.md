# Implement an approved delivery slice

Deliver one concrete slice from an approved implementation tree while preserving
human control of scope, decisions, review, and commits. This workflow selects
one ready leaf, coordinates repository-governed implementation and independent
review, and returns requirement uncertainty to its planning source.

This workflow does not create or refine implementation trees, invent product or
design requirements, or prescribe a repository-specific delivery process. Those
concerns belong respectively to breakdown, planning and the human, and the
repository's applicable guidance.

## Working rules

- Drive the tracker exclusively through `loti`; never inspect or edit store
  files directly. Read the target and its comments before changing it, and read
  a resource again immediately before replacing its body, comment, or asset.
- Implementation work is identified exclusively by the `implementation` label.
  Do not use `breakdown:*` labels to select work.
- Use a clear, consistent free-form identity for claims and comments. Claim only
  the selected leaf; never claim its ancestors.
- Only the orchestrator manages tracker state, creates tracker evidence, commits
  or merges, or presents decisions to the human. Delegated agents may report
  findings and evidence only.
- Every delegated agent must locate and follow applicable repository guidance,
  including local agent instructions, contribution guidance, test conventions,
  and nested instructions relevant to its work. When no such guidance exists,
  use the ticket's requirements and verification criteria without inventing a
  repository-specific process.
- In supervised operation, do not commit or merge autonomously. Stop after one
  completed leaf unless the human explicitly directs continuation.

## Entry and target validation

Accept an epic, an implementation root, or an implementation subtree.

1. Read the selected target, its comments, and the relevant tree. A target in
   `done` or `closed` is not selectable. A closed target must be explicitly
   reopened by the human before it can be worked.
2. For an epic, consider only top-level tickets labelled `implementation` that
   are `to-do` or `in-progress`. Exclude terminal roots and roots claimed by
   another worker. If multiple candidates remain, ask the human to choose one
   and recommend the lowest-numbered eligible root.
3. For a ticket, require the `implementation` label and confirm that it belongs
   to an implementation root. The named ticket is the execution scope. If that
   relationship is absent or ambiguous, stop and explain the problem without
   changing tracker state.
4. Before any code-changing delegation, inspect the working tree. If unrelated
   or uncommitted changes are present, ask the human how to isolate or handle
   them. Never assume a safe baseline.

## Provenance and source changes

Before delegation, locate the selected root's recorded planning-map reference,
selected specification, and relevant planning context. Read that material and
its publication and decision comments.

If the provenance is absent or invalid, ask the human how to proceed and
recommend repairing the root before implementation. If the human explicitly
chooses to proceed without a planning-feedback route, record that durable
exception on both the root and selected leaf. Do not invent a planning
destination.

Treat a later planning-map comment as a source-change warning only when it
records a specification publication or update, a resolved planning decision, or
new planning fog. Show the human the change and ask whether to proceed or
return to earlier work. The warning alone does not stop execution.

## Select a ready leaf

Within the selected scope, descend hierarchically to one concrete ready leaf.
A direct child with no descendants is a leaf. A non-leaf is not
implementation-ready while any descendant is non-terminal: it retains its
responsibility to compose child contracts and to perform its own integration
verification after those children are resolved.

A ready leaf is non-terminal and:

- has no unresolved descendants;
- has no non-terminal `blocked-by` dependency;
- has no explicit block on itself or an ancestor in its path; and
- is not claimed by another worker.

At every branch with multiple eligible paths, ask the human which path to take
and recommend the lowest-numbered eligible option. A direct invocation on a
ready leaf already expresses that choice.

An unclaimed `in-progress` leaf may be interrupted work. Before claiming or
resuming it, show the human its recorded context and ask how to proceed. If all
otherwise eligible work is claimed, report the active claims and stop. If the
only available path is explicitly blocked or has unmet dependencies, show the
stored reason and dependency state and ask the human how to proceed. Never
silently clear a block, remove a dependency, or override an ancestor's block.

After selecting a leaf, claim it, set it and every non-terminal ancestor to
`in-progress`, and record any durable state transition that needs explanation.
Do not claim ancestors.

## Delegate implementation and review

Delegate implementation to an implementation agent. Use additional supporting
agents only where repository guidance or a concrete delivery need warrants it.
The implementation brief must identify the selected leaf, its requirements,
verification criteria, provenance context, applicable repository guidance, and
the fact that subagents do not manage tracker state, commit, merge, or make
product or design decisions.

Then obtain an independent review. The reviewer must assess the ticket
requirements, repository state, implementation result, and applicable
verification evidence. Instruct the reviewer to report defects and evidence,
not to repair defects; repository guidance determines the practical mechanism
that preserves this boundary. An implementation conclusion that no code change
is needed still requires independent review.

A review pass requires applicable verification evidence. Unavailable required
verification is not a conditional pass: ask the human whether to provide the
prerequisite, block the leaf with a reason, or choose another path.

Tracker comments record durable, inspectable state only. When repository
practice creates a durable artifact or transition, add concise evidence pointing
to it, for example a worktree or branch, provisional or review commit, merge,
preserved report, block, or escalation. Do not record transient agent activity
or an unpreserved failed-review report.

## Correct review findings

A review finding returns the leaf to implementation. After correction, obtain a
fresh independent review; a reviewer never validates its own corrective work.

Perform at most two automatic correction-and-re-review cycles. If the second
re-review still finds defects, leave the leaf claimed and `in-progress`, retain
any durable evidence, and ask the human for direction. Do not hide the failed
convergence by clearing state or starting another automatic cycle.

## Human review and delivery

After independent review succeeds, record durable evidence that final human
review is awaiting, including a pointer to the reviewed artifact. Leave the leaf
claimed and `in-progress` while awaiting the decision.

The human's approval applies only to that reviewed durable state. Before
committing or merging, verify that it has not changed. If it has changed, obtain
fresh independent review and fresh human confirmation.

A human rejection must include feedback. Record the feedback on the leaf, retain
its claim and `in-progress` state, and perform another implementation and fresh
review cycle. Human-directed revision cycles do not consume the automatic
review-correction limit.

After approval, commit or merge according to applicable repository guidance,
record the durable result, mark only the selected leaf `done`, and release its
claim. Do not complete ancestors, sibling roots, or the epic. Parent integration
work remains selectable only after its descendants are terminal.

## Exceptional autonomous authorization

Autonomous continuation is exceptional. It must be independently and explicitly
given by the human, never solicited, inferred, or broadened by the orchestrator.
It permits selecting, reviewing, and committing ready leaves only within the
implementation root selected for that run.

That authorization does not extend to sibling roots, new scope, explicit
blocks, unmet dependencies, failed convergence, planning decisions, or any
other point requiring human direction. It ends when the selected root is
complete or human guidance is required.

## Requirement and planning escalation

When implementation evidence exposes a required product or design decision,
present the human with the evidence, options, and impact on the selected leaf.

The human may provide a bounded direct answer. Record the answer and rationale
on both the leaf and its source planning map, then continue. If the answer
materially changes the approved specification, required outcome, or
implementation-root boundary, recommend returning to planning and breakdown
rather than continuing with an invalidated decomposition.

The human may instead graduate the issue to planning fog. Append the new fog to
the source planning map, set that map to `in-progress` when it was complete,
and block the implementation leaf with the reason. New planning decisions and
their resolution belong to the planning workflow.

When no authoritative planning destination exists, do not invent one. Ask the
human how to proceed and recommend provenance repair before continuing.
