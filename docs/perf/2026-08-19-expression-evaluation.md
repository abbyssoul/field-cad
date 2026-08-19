# Expression evaluation — 2026-08-19

## Hardware and command

- 13th Gen Intel Core i7-1370P, 14 cores / 20 logical CPUs
- Linux 6.17.0-41-generic x86_64
- `cargo run --release -p fieldcad-bench -- --filter expressions --save-baseline docs/perf/2026-08-19-expressions.json`
- Allocation assertion: `cargo test -p fieldcad-expressions --test steady_state_allocations`

The machine-readable report is
[`2026-08-19-expressions.json`](2026-08-19-expressions.json).

## Interpretation

Both full sweeps retained their declared linear complexity. The compiled
constant graph measured `O(nodes^1.13)` with 0.057 log-space scatter, from a
456 ns median at 16 nodes to 23.50 µs at 512 nodes. Live distance bindings
measured `O(bindings^1.05)` with 0.021 scatter, from 406 ns at 16 bindings to
15.41 µs at 512 bindings. The harness reported no benchmark growing faster
than declared.

The warmed candidate-evaluation integration test varied live distance values
for 1,000 evaluations and observed zero allocations inside the ADR 0026
evaluation boundary. Immutable world/snapshot publication after an adopted
change remains outside that boundary.
