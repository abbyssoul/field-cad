# 0026 — Authored expressions evaluate outside the world, before ticks

Status: **accepted**

## Context

A formula is authored experiment intent, but equation-system plugins require a
small, stable input contract: finite SI quantities in validated component bags.
Putting an AST or an unresolved observation reference in `PropertyValue` would
make every solver, serializer, and sampling consumer understand the expression
language. Evaluating live references after a solver advances would also make a
distance observed at state `n` affect the wrong transition.

## Decision

The runtime owns an `ExpressionDocument` beside the world. Persisted source,
stable constant identities, embedded user definitions, and property targets
live there. A solver-independent crate compiles that authored graph into a
transient deterministic plan. The authoritative world continues to contain only
ordinary finite SI-valued `PropertyValue`s.

Every authored expression edit is compiled and evaluated against a candidate.
Its resolved component bags then cross the existing solver validation boundary,
and the world plus authored graph are adopted together only if every check
succeeds. Removing a target removes its binding in that transaction; removing a
referenced constant or distance probe is rejected with its dependents.

A schema must explicitly opt into live binding. Immediately before `Step` and
each running tick, the compiled graph reads distance probes from the current
authoritative pre-tick world. Changed resolved values are adopted and reported
to solvers before force evaluation and advancement, so state `n` influences
`n → n+1`. Failure pauses/refuses advancement without changing the clock or the
last valid world. Compilation and graph construction never occur in the tick
loop.

Snapshot and retained-run provenance include the authored graph content hash,
so numerically equal worlds with different formulas remain distinguishable.

## Consequences

- Plugins and `fieldcad-core::PropertyValue` remain expression-language agnostic.
- Documents retain reproducible formulas while older documents load with an
  empty expression section.
- Live evaluation is linear in graph nodes, dependency edges, and referenced
  distances; implementations must reuse compiled nodes and tick scratch storage.
- A live feedback cycle is invalid rather than an algebraic system to solve.
- Renaming is presentation work around stable identities; persisted source is
  rewritten, while compiled references are rebuilt.
