# 0024 — undo restores a captured scene, forwards

Status: **accepted** (builds on
[0007](0007-validate-before-adopting-a-world-edit.md),
[0011](0011-queue-running-edits-at-fixed-tick-boundaries.md),
[0023](0023-an-interactive-edit-suspends-the-run.md))

## Context

Every scene change was already a validated `WorldCommand` batch committed
atomically at one revision, and the conceptual architecture has had an arrow
labelled "undo/history (later)" hanging off that path since the beginning. This
is later.

The obvious implementation — store the inverse of each command — does not work
here. Undoing `RemoveObject` has to put the object back *with the identifier it
had*, and `CreateObject` mints a fresh one. Every consumer keyed by identifier
would break: a probe attached to the object, a recorded series, the current
selection. Writing an inverse for each of the nineteen command variants would
also mean writing a new one for every command a future plugin adds, and getting
one wrong produces a scene that never existed rather than an error.

Two things also make undo harder here than in a drawing program. A drag submits
an edit *every frame*, so a naive stack steps back one mouse position at a time.
And the world is not only authored: a solver tick writes to it too.

## Decision

**An entry is a captured scene, not an inverse.** `World` is already
`Arc<WorldState>` plus three identifier counters, so `World::checkpoint` is a
pointer and an entry costs a pointer plus a label. Scenes that an edit did not
change are shared between entries, and field data is not in the world, so
nothing large is retained. The stack is bounded anyway
(`DEFAULT_UNDO_DEPTH = 128`).

**Restoring moves the revision forward.** A `WorldRevision` is a point in this
world's history, not a place to return to; restoring contents that once existed
produces a revision that never existed before. Identifier counters are not
rewound either, so undoing a creation frees nothing and no later object can
inherit a predecessor's identity.

**Restoring is validated like any other edit.** A scene that was representable
when it was captured may not be now — a field system enabled since can reject it
— so every active solver sees the candidate before the world moves
([0007](0007-validate-before-adopting-a-world-edit.md)). A refusal leaves the
history exactly as it was.

**One interactive edit is one step.** [0023](0023-an-interactive-edit-suspends-the-run.md)
already brackets a drag or a held control; the first commit inside the gesture
records the scene it started from and the rest join it. This is the whole reason
undo is usable on a viewport drag.

**A solver tick that moves a body clears the history.** The world is then no
longer the authored scene the entries describe, and restoring one would drag
every integrated body back without rewinding the clock. Ticks that change
nothing — an analytic scene, or one where nothing is free to move — leave the
history alone, because there is nothing to be inconsistent with.

**Undo and redo are refused while running**, mirroring
`CannotStepWhileRunning`. An undo names a scene, and a running clock is
replacing that scene underneath it.

**The history lives with the world.** Undo is `CommandPayload::Undo` on the
data-source boundary, like play and pause. Only the authoritative side can say
what the scene was or validate that it may be restored; a client keeping its own
stack would be guessing, and would be wrong the moment compute is remote.

Session setup — the default scene, later a loaded file — authors through the
same command path, which is what keeps validation and provenance uniform, and
then calls `clear_edit_history`. The opening undo of a session emptying the
workspace is not a feature.

## Consequences

Complex edits need no special handling. A transaction of any size is one entry
because an entry is a scene, so "move three objects and change two properties"
undoes as one step if it was committed as one batch, and each command names the
batch through `WorldCommand::batch_label` so the button can say what it will
reverse rather than offering an unlabelled arrow.

**What this trade costs.** Memory is proportional to the number of *distinct*
scenes reachable through the stack, not to the size of the edits, so a session
that repeatedly edits one large scene retains one copy of it per entry. Nothing
in the current world model is large enough for that to matter; if a scene ever
becomes large, the entry type is the single place to change.

The history is also all-or-nothing across ticks. A user who runs a simulation
and then wants their last authoring step back cannot have it: the scene it named
is gone. The alternative — rewinding the clock along with the scene — was
rejected because simulation time advances only through accepted fixed ticks, and
an undo that moved it backwards would make snapshot provenance a fiction. Full
state rewind is a different feature, and belongs with recording and replay.
