# Implementation roadmap

**Status updated: 2026-10-10.** This file is Spool's single live summary of
project maturity, the next milestone, and contributor-sized work. The README
introduces Spool; it is not the status log. Use GitHub issues or assigned tasks
for owners, discussion, and detailed progress, then link the active tasks here.

## Current state

Spool has a native GPUI editor, `.spool` project creation and opening, a
source-backed document model, and a bounded set of visual edits that write back
to authored HTML/CSS and `lamine.yaml`. Supported object creation and
duplication persist through save/reopen. Deleting a container deletes its
descendants in one semantic history operation; undo restores the subtree.

The governing boundaries remain in [Product and Boundaries](01-product-and-boundaries.md),
[Document and Source Model](02-document-and-source-model.md), and
[Editor Runtime and History](03-editor-runtime-history.md). In brief: HTML,
CSS, and SVG remain authored truth; `lamine.yaml` holds Spool identity and
structure; editor runtime state is disposable; persistent changes use semantic
operations and history.

## Completed slices

- **Project lifecycle and source-backed editing:** open and create `.spool`
  projects; preserve authored source outside supported edit spans; save and
  reopen supported text, geometry, style, rename, and created-object changes.
- **Core interaction and persistence:** click/drag creation, tool handoff,
  canvas keyboard focus, verified GPUI window interactions, and persistent
  deletion/undo behavior, including container cascades.
- **Verification foundations:** focused tests and mutation harnesses cover key
  interaction, operation, project, and created-object invariants.

The latest implementation report is commit `f6453c1` (following `2587aed`). It
reports 730 passing tests, clean formatting and Clippy, successful mutation
runs, and a release bundle launch. Interactive GUI verification was not done.
These are agent-reported results and were not rerun while updating this roadmap.

## Next milestone — reliable first editing workflow

**Status: proposed working focus; team owners and target date are not assigned.**
This follows the team's stated goal of completing the first usable application
surface and the latest agent report's remaining risks. Confirm the scope and
assign owners at the next team planning point.

The milestone is complete when a contributor can demonstrate a supported
project workflow from open through edit, save, close, and reopen, and can see
what happened when an operation is refused.

### Acceptance checks

1. A supported `.spool` project opens into its authored document. A project
   deliberately emptied of all managed nodes does not silently become the
   starter scene on reopen.
2. Create, duplicate, edit, and delete (including container cascade) each
   update the runtime, persistent structure, and authored source consistently.
3. Each committed action is one undo step; undo and redo restore the expected
   objects and source. Cancelled actions add no history entry.
4. A refused semantic operation is visible to the user and does not leave
   runtime, document, source, or history in conflicting states.
5. Unsupported or ambiguous source edits are reported without rewriting
   unrelated authored bytes.
6. Focused model and GPUI window tests cover the workflow. Relevant mutation
   harnesses must detect removed behavior and restore files even when a mutant
   fails.

### Candidate tasks (unassigned)

- **Operation refusal behavior:** trace `commit_operation` and its callers;
  define the smallest user-visible failure message and ensure a refusal cannot
  leave eager runtime changes behind.
- **Empty-project reopen:** reproduce the zero-node case and specify whether an
  empty document is a valid saved project; make open/reopen behavior match that
  decision.
- **Surface consistency pass:** choose one reported Canvas/Layers/Inspector
  behavior at a time, establish expected behavior, and add a reproducible
  acceptance test before implementing a fix.
- **Mutation harness maintenance:** resolve the four stale anchors reported in
  `mutate_ops.sh` as separate test-tooling work.

No contributor owns these tasks yet. Assign one task per person with an
acceptance check and review date; avoid parallel edits to high-coupling canvas
or shell behavior without coordination.

## Later, when a concrete workflow needs it

- Expand the supported CSS/layout subset based on real project examples and
  explicit source-ownership rules.
- Improve design-editor interaction consistency in focused slices.
- Measure renderer or document-scale bottlenecks before adding optimization
  infrastructure.
- Explore local AI operating on the same structured, source-backed project.

These are directions, not committed milestone dates or feature promises.

## Keep out of the current milestone

- A general browser-compatible CSS engine or complete cascade inspector.
- A duplicate store of HTML/CSS visual properties in `lamine.yaml`.
- A browser DOM as the editor's runtime scene graph.
- Collaboration, plugin/MCP infrastructure, or an AI agent runtime.
- Large-scale optimization without a measured bottleneck.
- A parallel demonstration app or wholesale rewrite of existing GPUI tools.

## Contributor verification

Read [CONTRIBUTING.md](../CONTRIBUTING.md) for setup and checks. Run focused
tests for changed behavior and the app crate's test suite. Report exact commands
and outcomes; label agent-reported or unrun checks accurately. If platform or
dependency limits prevent a check, include the command and failure details.
