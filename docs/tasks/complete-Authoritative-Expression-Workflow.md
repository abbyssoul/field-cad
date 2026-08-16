# Complete the Authoritative Expression Workflow

## Summary

Finish the expression milestone around six gaps: one atomic scene-edit transaction, observable dependency health, complete
local draft validation, validation before Play, an enforceable allocation boundary, and acceptance/performance evidence.

The allocation guarantee applies to compiled dependency lookup and expression evaluation after initialization. Creating
immutable world revisions and snapshots may still allocate when a resolved value changes; ADR 0026 will state that boundary
explicitly.

## Public Interfaces and State

- Add SceneEdit { world_commands, expression_commands } and CommandPayload::CommitSceneEdit.
    - SimulationRuntime::commit_scene_edit applies both parts as one candidate, one solver-validation pass, one history
    entry, one queue item, and at most one world revision.

    - Apply world commands provisionally, remove bindings whose targets disappeared, apply expression commands, compile/
    evaluate against the provisional world, add resolved property changes, then validate and adopt everything together.

    - Removing a probe and clearing its final references in the same transaction succeeds; removing it while references
    remain fails atomically.

    - A world property write that changes a still-bound property is rejected as a conflicting writer unless that binding is
    cleared in the same transaction.

    - Keep CommitWorld, CommitExpressions, and their runtime methods as compatibility wrappers. Persist newly queued edits
    as the unified variant while continuing to deserialize legacy queue variants.

    - Add an MCP edit_scene tool accepting typed world edits plus expression commands; preserve existing tools as wrappers.

- Add transport-serializable expression state:
    - ExpressionSubject: constant ID or property target.
    - ExpressionDependency: constant ID or distance-probe ID.
    - ExpressionNodeStatus: Resolved, Faulted, or Blocked { by }.
    - ExpressionNodeState: subject, direct dependencies, last valid value, and status.
    - ExpressionDiagnostic: subject plus existing error kind, message, span, and dependents.
    - ExpressionState: authored document, graph hash, resolved world revision, node states, and current diagnostics.

- Add FieldDataSource::expression_state() and propagate it through async, server, desktop, and MCP surfaces. Keep the
existing definition/value accessors as compatibility projections.

- Authoritative state contains accepted graph health and the current live-evaluation fault only. Rejected edits remain on
their command receipt; UI drafts diagnose locally.

- Embedded-library update availability remains a desktop overlay because headless authorities do not read the user library.

## Runtime, UI, and Performance

- Refactor EvaluationPlan into a transactional, dense evaluation plan:
    - Compile constant references to stable vector slots and precompute direct/reverse dependency edges.
    - Allocate active and candidate value buffers during compilation/initialization.
    - Evaluate into candidate buffers without insertion, cloning, graph rebuilding, or allocation.
    - Expose candidate property iteration, explicit candidate adoption, and discard-on-error behavior.
    - Commit candidate values only after world and solver validation, preserving all last-valid values after failures.
    - Propagate transitive liveness through constants; user constants must reject distance references, and any property
    depending indirectly on a distance must require live_binding.

- Make SimulationRuntime::play() return Result and run the same preparation path used by Step and ticks before changing
mode.
    - On success, adopt changed values and publish the resulting paused snapshot before entering Running.
    - On failure, remain paused with clock, world, last valid values, and latest snapshot unchanged; update ExpressionState
    with the owning fault and blocked dependents.

    - Clear the fault after the next successful evaluation.
    - Notify only solvers declaring consumption of components whose resolved values changed.

- Use the shared scene-edit path for authored edits, undo/redo, and queued adoption so validation behavior cannot diverge.
- Replace ad hoc editor previews with one candidate-preview helper that applies draft commands to the whole document and
compiles/evaluates against the current world.
    - Existing constant rows, new constants, property formulas, and user-library constants show evaluated values or owning
    diagnostics with byte spans while typing.

    - Invalid drafts disable Apply and Enter submission; Escape/Cancel restores authoritative source.
    - Downstream bindings affected by a draft are listed.
    - User-library drafts cannot be saved while invalid.
    - Draft state resynchronizes when the authoritative graph changes externally unless the user has an active dirty draft.
    - Merge expression diagnostics into the desktop diagnostics view and expose the same state through MCP.

- Amend ADR 0026 to define the zero-allocation boundary and unified pre-tick preparation semantics.
- Extend benchmarks:
    - Keep expressions/evaluate-graph, but include a terminal property binding rather than constants only.
    - Add expressions/evaluate-live-bindings, sweeping live-binding/reference count with preallocated provider and output
    buffers.

    - Declare both workloads linear in graph nodes, dependency edges, and evaluated references.
    - Use a dedicated integration-test executable with a counting global allocator: warm the plan, vary distance values for
    at least 1,000 evaluations, and assert zero allocations inside the measured evaluation region.

    - Run fieldcad-bench --filter expressions and retain the report as verification evidence; timing is measured, while
    allocation count is a hard test assertion.

## Test and Acceptance Plan

- Expression engine:
    - Direct and transitive dependency metadata is deterministic.
    - Transitive distance use marks a binding live; user constants cannot reference distances.
    - Faults identify their constant/property and source span; dependents become blocked.
    - Failed candidate evaluation leaves active constants and property outputs unchanged.
    - Warmed evaluation performs zero allocations.

- Runtime and command queue:
    - Mixed world/expression edits adopt atomically with one revision and one undo/redo step.
    - Any compile, evaluation, schema, or solver failure rolls back both halves.
    - Probe removal plus reference removal succeeds together; probe removal alone lists dependents.
    - Unified edits queue and replay in order while running and round-trip through queue persistence; legacy queued variants
    still load.

    - Play, Step, and each tick use pre-tick distance state.
    - Failed Play remains paused; failed ticks do not advance clock/world or replace the latest snapshot.
    - Successful retry clears diagnostics.
    - Unchanged values skip world adoption and solver notification; changed values notify only consuming solvers.
    - Repeated runs and recording replay produce identical resolved worlds and expression graph hashes.

- State, persistence, and transport:
    - Scene documents still persist authored expressions only; dependency state is rebuilt on load.
    - Expression state is equivalent through local, async, server, and MCP sources.
    - MCP unified edits have the same receipt, rollback, diagnostics, and queue behavior as desktop edits.
    - Snapshot and run-record provenance continue distinguishing numerically equal worlds with different formulas.

- UI and verification:
    - Unit-test the pure draft-preview/state-transition helpers for syntax, dimensions, cycles, downstream failures,
    cancellation, and authoritative resynchronization.

    - Run formatting, workspace tests, Clippy, expression benchmarks, and the 120-frame desktop smoke check.
    - Manually verify property and variable drafts, span diagnostics, downstream lists, invalid-save prevention, failed
    Play, save/reload, and MCP parity because there is no driven GUI harness.

## Assumptions

- Existing wire and scene compatibility is preserved; old command variants are not removed.
- Newly created object IDs cannot be referenced by expressions in the same transaction because current commands allocate IDs
during commit; unified transactions otherwise support all existing targets.

- Expression evaluation is the enforced zero-allocation hot path. Immutable world revision and snapshot publication
allocations are measured separately but are not part of that assertion.

- Rejected draft/edit history is not added to authoritative expression state; command history remains its source of truth.
- Catalog/object-property expression references and production live-bindable schemas remain outside this implementation.