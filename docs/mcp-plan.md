Original task:
MCP server: The app should expose a REST API to allow external clients to design and control the simulation. The API should support creating, modifying, and deleting objects in the scene, as well as starting and stopping the simulation. Additionally, it should provide endpoints for retrieving the current state of the simulation, including the positions and properties of all objects.
So as a user I want to be able to control the simulation from an external client, such as a web interface or a mobile app or an AI agent. This will allow for more flexible and remote control of the authoring and simulation environment.

---

Expanded plan (research done 2026-08-05, not implemented):

Most of the hard design work for this already exists and should not be
redone: `docs/user-stories/README.md` is the authoritative capability
contract — it has a "Suggested MCP surface" table mapping every user story to
a capability, 8 API/MCP design rules (model is the core, reads/mutations/
streams are separate, stable IDs, optimistic concurrency on world revision,
schemas + structured errors, provenance end-to-end, remote and local sources
behave identically), and it already marks which stories are *Implemented*
vs. *Required for API/MCP parity*. Treat that document as the spec; this
entry is the implementation plan on top of it.

The architecture is also already prepared for this, per ADR 0001: the
desktop app talks to a `FieldDataSource` trait (commands in, versioned
immutable snapshots out), with `LocalDataSource` (in-process) and
`LoopbackDataSource` (remote stand-in) required to be interchangeable —
`LoopbackDataSource` is *literally* the placeholder this MCP server fills in
for real. Nothing about the world/command model is MCP-specific; this is a
new transport on an existing boundary, not a new API surface to design from
scratch.

**MCP vs. REST**: the task title says MCP, the description says REST — these
are different things (MCP is JSON-RPC 2.0 with tool/resource primitives
built for LLM agents; REST is plain HTTP for any client). Recommendation:
build the MCP server first, since that's what's named as the actual near-term
goal and what an AI agent client needs natively. Because the underlying
`FieldDataSource`/`WorldCommand` surface is transport-neutral, a REST/HTTP
layer later is mostly new routing over the same domain calls, not a second
design — defer it rather than building both at once.

Concrete gap, before any MCP-specific work: `CommandPayload`, `Command`,
`CommandReceipt`, `DataSourceStatus`, `SimulationStatus`,
`EditHistoryStatus`, and `FieldSystemStatus` (all in
`crates/fieldcad-simulation`) do not derive `Serialize`/`Deserialize` yet —
only `WorldCommand` and the snapshot types do. Nothing can cross a real
process boundary until that's closed; this is a small, mechanical, low-risk
first step, worth doing on its own.

**Done (2026-08-05):** added `Serialize`/`Deserialize` derives to all seven
types above plus their non-conforming field types (`CommandId`,
`PlaybackSpeed`, `CommandDisposition`, `Subscription`, all in
`crates/fieldcad-simulation`). All their nested field types already derived
serde, including `Domain` — brought in since this closed the gap, by the
`ReconfigureDomain` command added meanwhile for "configurable local
numerical domains." Purely mechanical: no custom `Serialize`/`Deserialize`
impls were needed, confirmed by `cargo build --workspace` and
`cargo test -p fieldcad-simulation` (80 passed).

Phased plan:

1. **Close the serialization gap** above. Done.
2. **Stand up a headless server**, not an embedded feature of the desktop
   app: a new crate (e.g. `fieldcad-mcp` or `fieldcad-server`) that owns a
   `LocalDataSource`/`AsyncLocalDataSource` from `fieldcad-simulation` and
   runs with no window/GPU dependency — deployable on a machine with no
   display, and exactly the shape ADR 0001 already designed for. Embedding an
   MCP server as an *optional* mode inside `fieldcad-desktop` (so a human
   can watch an agent drive the same live session in real time) is a
   reasonable follow-on once the standalone path works, not a prerequisite.

   **Done (2026-08-05):** new crate `crates/fieldcad-server`, named for the
   internal architecture it owns rather than for MCP specifically — MCP is
   one interface onto it, not the reason it exists. It follows the app's Elm
   architecture: one authoritative model, and commands as the only way to
   change it. `HeadlessServer` owns that model — a `SimulationRuntime` behind
   an `AsyncLocalDataSource` — and exposes `submit`/`execute`/`advance`/
   `drain_events`/`status`/`latest_snapshot`, the same shape any command
   source uses; the desktop UI is one such source today, a future MCP/network
   layer will be another, equal one, driving the same `HeadlessServer`
   through the same `FieldDataSource` contract ADR 0001 defines. No transport
   is wired up yet — `src/main.rs` runs the model on a wall-clock poll loop
   with nothing attached, proving the crate builds and runs with zero
   wgpu/winit/egui in its dependency graph (`cargo tree -p fieldcad-server`
   confirms). `default_session()` starts from an *empty* world, deliberately:
   the desktop's demo scene (default charge/probe/plane) is a UI convenience,
   not part of the server's contract, and a remote client must build up a
   scene the same way a local one would. An integration test
   (`tests/headless_session.rs`) authors a scene and steps it entirely
   through commands, polling for the asynchronous completion events
   `AsyncLocalDataSource` produces (ADR 0011) — the smaller precursor to the
   phase 7 parity test, since there is no second transport to compare against
   yet. `cargo build --workspace` and `cargo test -p fieldcad-server` pass.
3. **Map the "Suggested MCP surface" table onto MCP primitives**: world/
   experiment/run mutations become MCP tools (`commit_world`, `play`,
   `pause`, `step`, `set_time_step`, `set_subscription`, `undo`/`redo`, …);
   read-only state (world, simulation status, field systems, latest
   snapshot, probe history, diagnostics) becomes MCP resources, or read
   tools if the chosen SDK's resource model doesn't fit; live updates
   (snapshot publication, queued-command completion, diagnostics) become
   resource-subscription notifications rather than something a client has to
   poll for.

   **Done (2026-08-05):** new crate `crates/fieldcad-mcp`, depending on
   `fieldcad-server` and `rmcp` 3.1.0 (the official Rust SDK — its maturity
   and API were verified against the vendored crate source and its own
   integration tests, not assumed; see below). `McpServer` wraps
   `Arc<tokio::sync::Mutex<HeadlessServer>>` — the shared-state pattern
   `StreamableHttpService`'s per-session factory requires — and exposes 18
   tools via `#[tool_router]`/`#[tool_handler]`, each a direct call into one
   `CommandPayload` variant or one `FieldDataSource` read: `get_world`,
   `get_simulation_status`, `get_source_status`, `list_field_systems`,
   `get_edit_history`, `get_subscription`, `get_latest_snapshot`,
   `commit_world`, `play`, `pause`, `step`, `set_time_step`,
   `set_playback_speed`, `set_subscription`, `set_field_system_enabled`,
   `set_field_model`, `undo`, `redo`. Every non-blocking submission
   (`AsyncLocalDataSource`/ADR 0011) is awaited inside the tool call via a
   poll loop, so a client gets one request/response instead of having to
   poll separately for completion. `commit_world`'s `WorldCommand`s and
   `set_subscription`'s density fields are the only inputs that don't map
   to a native MCP JSON Schema in this slice: `WorldCommand` and its nested
   types aren't `schemars::JsonSchema`, and deriving that across all of
   `fieldcad-core` is bigger than this slice. `commit_world` now accepts a
   native JSON array of command objects rather than a JSON-encoded string;
   plugin-defined component-property values remain dynamically validated
   against the schemas the server reports. A fully generated typed command
   DSL remains a follow-up for richer agent-side authoring guidance.
   Deliberately deferred, because the underlying capability doesn't exist in
   the model yet or needs its own design: scene lifecycle (create/open/
   save), particle templates, rename, probe history/trajectories as
   server-retained series, a dedicated diagnostics read (today folded into
   the snapshot), run comparison, record/replay, export, and
   `watch_session`/resource-subscription push events (every read here is a
   pull, via a tool call).

   Building this surfaced one real, generic serialization bug, now fixed:
   `QualifiedName` (backing `ChannelId`/`ComponentTypeId`) derived
   field-by-field `Serialize`, so it encoded as a JSON *object* — which
   `serde_json` rejects as a map key with "key must be a string". Both
   `ChannelId` and `ComponentTypeId` are used as `BTreeMap` keys in
   `FieldSnapshot`/`WorldObject`/`WorldState`, so `get_world` and
   `get_latest_snapshot` failed on literally the first call, before any
   scene was even authored (plugins register component schemas at startup).
   Fixed with a hand-written `Serialize`/`Deserialize` for `QualifiedName`
   using a `"plugin:name"` string — `:` cannot appear in either field
   (`validate_identifier` only allows alphanumerics, `-`, `_`, `.`), so it's
   unambiguous, unlike `.`, which both fields may already contain and which
   `Display` keeps using for its unrelated human-readable form. Also added
   `Serialize`/`Deserialize` to `WorldSnapshot` itself (missed in phase 1 —
   `WorldState` had it, its `Arc<WorldState>` wrapper didn't).

   Verified two ways: an in-process test module in `fieldcad-mcp` (three
   `#[tokio::test]`s — author a scene and step it through tools only,
   confirm an invalid `commit_world` payload comes back as a tool-level
   error rather than a protocol error, read field systems/edit history) that
   calls the `#[tool]` methods directly (the router still calls them by
   their original name, confirmed by reading `rmcp-macros`' codegen rather
   than assumed); and a manual smoke test of the real stdio JSON-RPC wire
   protocol against the built `fieldcad-mcp` binary (`initialize` →
   `notifications/initialized` → `tools/list` → `tools/call`), which is what
   actually caught the `QualifiedName` bug, since the in-process tests were
   written before it and only the real `serde_json`-backed wire path
   triggers `serialize_map`'s key check. `cargo build --workspace`,
   `cargo test --workspace` (373 passed), and `cargo clippy` on the touched
   crates are clean. `src/main.rs` served over stdio only at the end of this
   phase — phase 4 below replaced that with a CLI that selects transports,
   stdio included.

   **P0 completed (2026-08-05): numerical-domain reconfiguration is now an
   MCP tool.** `reconfigure_domain` accepts typed, schema-discoverable bounds
   in metres, cell counts, a boundary condition per axis, and precision — not
   a JSON-encoded domain blob. It sends the existing authoritative
   `CommandPayload::ReconfigureDomain`, so local and remote calls share the
   same validation, tick-boundary queuing, solver rebuild, reset-to-paused
   `t=0`, run-generation, and automatic safe-`dt` semantics. Its response
   returns the command receipt plus the adopted domain and simulation status.
   A focused MCP test verifies the reset result.

   **P0 completed (2026-08-05): realtime/deferred field-system control is now
   an MCP tool.** `set_field_system_realtime(plugin, realtime)` maps directly
   to the existing authoritative command. It makes the desktop's performance
   choice available to an automation client without changing the result of the
   committed scene.

   **P1 completed (2026-08-05): dedicated inspector reads are now MCP tools.**
   `get_diagnostics` returns the latest snapshot's structured diagnostics with
   that snapshot's provenance; `get_body_forces` returns the dynamics system's
   most recent per-object force vectors in SI newtons. Neither requires an
   agent to reconstruct inspector data from field batches.

   **P2 completed (2026-08-05): streamed interactive edit lifecycle is now
   available through MCP.** `begin_interactive_edit` and
   `end_interactive_edit` directly bracket the authoritative gesture state;
   the end command triggers deferred-system recomputation. Agent clients
   should normally prefer one final atomic world transaction.
4. **Transport: Streamable HTTP, not stdio.** Stdio MCP is for a client that
   spawns its own local subprocess (e.g. Claude Desktop's typical
   integration); this task explicitly wants remote clients — "a web
   interface or a mobile app or an AI agent" — over a network, which needs
   the HTTP transport.

   **Done (2026-08-05):** `fieldcad-mcp`'s `main.rs` now takes `--stdio`,
   `--http [ADDR]`, and `--unix PATH`, additively — any combination runs
   concurrently against *one* shared session (`Arc<Mutex<HeadlessServer>>`
   behind cloned `McpServer` handles), so e.g. an agent on stdio and a web
   client on HTTP see and mutate the same model, not independent sessions.
   Defaults to `--stdio` alone if nothing is given, preserving phase 3's
   behavior. Unix domain socket support was added in the same pass, per
   request: a plain, unauthenticated local-IPC transport, useful for a
   trusted same-host client that shouldn't need a bearer token once phase 5
   adds one for HTTP. Implementation notes:
   - HTTP and Unix both reuse `StreamableHttpService` (the transport-neutral
     Tower service `rmcp` provides — not axum-specific despite every
     example mounting it on an axum `Router`, confirmed by reading
     `tower.rs`'s `Service<Request<RequestBody>>` impl rather than assumed).
     `axum::serve` handles the HTTP listener; the Unix listener does not
     — `axum::serve(UnixListener, …)` uses `spawn_local` on Linux, which
     needs a `LocalSet` this binary doesn't set up. Unix instead runs the
     same manual `hyper::server::conn::http1` accept loop `rmcp`'s own
     `test_unix_socket_transport.rs` uses on the server side of its Unix
     socket test — a proven pattern, not a new one.
   - `axum`/`hyper`/`hyper-util` versions are pinned to match what `rmcp`
     3.1.0 itself uses in that test's `[dev-dependencies]`
     (`axum = "0.8"`, `hyper = "1"`, `hyper-util = "0.1"`), so the
     `http`/`http-body`/`tower-service` trait implementations both sides
     rely on are the same ones, not merely compatible-looking ones.
   - `--http` rejects any non-loopback address outright — phase 5's
     "opt-in flag plus bearer token before any non-loopback bind" doesn't
     exist yet, so there is currently no way to ask for a non-loopback bind
     at all, rather than a default that could be overridden unsafely.
   - The Unix socket gets `0600` permissions set explicitly after `bind`
     (its ambient `umask`-derived permissions are not a security boundary
     to depend on for a full scene-mutation control surface), and a stale
     socket file from a killed previous run is detected by attempting a
     connect before removing it — only replaced once nothing answers, never
     blindly.
   - Shutdown: one root `CancellationToken`, cancelled by Ctrl+C, handed to
     every transport as a child token — `RunningService::serve_with_ct` for
     stdio, `StreamableHttpServerConfig::with_cancellation_token` plus
     `axum::serve(..).with_graceful_shutdown(..)` for HTTP, and a
     `tokio::select!` against `ct.cancelled()` in the Unix accept loop.
   - Verified end-to-end with real clients, not just compilation: the stdio
     smoke test from phase 3 still passes after the rewrite; `curl` against
     `--http` completes a full `initialize` → `notifications/initialized` →
     `tools/call` handshake; `curl --unix-socket` does the same against
     `--unix` (both required a `Host: localhost` — `rmcp` rejects other
     `Host` values as DNS-rebinding protection, discovered by trying
     `mcp.local` first and reading the resulting "Forbidden: Host header is
     not allowed"); `--http 0.0.0.0:8642` is rejected before any bind is
     attempted; a killed-and-restarted `--unix` server correctly reclaims
     its stale socket file. `cargo build --workspace`, `cargo test
     --workspace`, and `cargo clippy --workspace --all-targets` stay clean
     (one pre-existing, unrelated warning in `fieldcad-simulation`).
   - Not done: bearer-token auth itself (still phase 5 — the loopback-only
     restriction above is the stand-in until then); the two-process parity
     test from phase 7 (no second independent transport implementation to
     compare against — HTTP and Unix both go through the same
     `StreamableHttpService`, so they are not independent evidence of
     anything phase 7 is meant to check).
5. **Security**: bind to localhost by default; require an explicit
   opt-in flag/config plus a bearer token before listening on any
   non-loopback interface, since this is full scene-mutation control, not a
   read-only endpoint.

   **Done (2026-08-05):** bearer-token auth for HTTP, and — the actual
   motivating use case — the MCP server embedded *inside* `fieldcad-desktop`
   itself, so an agent can drive the exact live session a user has open
   rather than a separate, empty standalone one. An "MCP" checkbox in the
   top bar opens a panel (`apps/fieldcad-desktop/src/ui/panels.rs`,
   `mcp_window`, mirroring the existing Diagnostics panel) with an "Enable
   MCP" button; enabling generates a fresh UUID v4 token (never persisted —
   confirmed with the user; keychain persistence is an explicit, separate
   follow-up), spawns a dedicated OS thread with its own minimal
   single-threaded tokio runtime, binds `127.0.0.1:8642`, and shows the
   token plus connection URL (masked by default, `egui::TextEdit`'s
   built-in password mode, with a copy button using
   `egui::Context::copy_text`). Tightened past what's written above: the
   bearer token is required for HTTP *even on loopback*, not only for a
   non-loopback bind — another local process or user could otherwise hit an
   unauthenticated port and mutate the scene, which matters once "loopback"
   can mean "this port, which the desktop app just started for exactly this
   purpose." The standalone CLI's `--http` gained matching `--token`/
   auto-generate-and-print behavior for the same reason, so both paths carry
   the same guarantee; its loopback-only restriction itself is *not* relaxed
   in this change (still deliberately deferred — a non-loopback bind needs
   its own explicit opt-in design, not just "a token now exists"). The Unix
   socket transport stays unauthenticated by design either way, relying on
   its 0600 file permissions as the trust boundary instead of a token.

   This surfaced a real concurrency bug, not just a wiring task: once the
   desktop UI's per-frame pump and an MCP tool call's own wait loop could
   share one `HeadlessServer`, both independently minting `CommandId`s from
   separate `CommandSequencer`s and both draining
   `AsyncLocalDataSource`'s single destructively-drained event queue, a
   command's completion could be silently stolen by whichever side drained
   first — a hang (no timeout existed) or, worse, one side observing a
   *different* in-flight command's receipt because both could mint the same
   numeric id. Fixed in `crates/fieldcad-server`: `HeadlessServer` is now
   the sole minter (desktop no longer keeps its own `CommandSequencer`) and
   the sole place that calls the inner drain, with a per-command
   `tokio::sync::oneshot` waiter (`submit_and_await`) registered under the
   same lock as submission, fulfilled by whichever caller's `drain_events()`
   call actually contains the matching event. Proved with a test
   (`crates/fieldcad-server/tests/concurrent_transports.rs`) that pits two
   independent pumping threads against 50 concurrent submissions — checked
   directly against the old scan-the-returned-`Vec` approach during
   development, which reliably hung/cross-delivered under the same load
   (0/50 completed) where the new approach gets a clean 50/50. Also: the
   shared model moved from `tokio::sync::Mutex` to `std::sync::Mutex`
   throughout `fieldcad-mcp`/`fieldcad-desktop` (needed so the synchronous
   winit frame loop can lock it without its own tokio runtime; sound because
   no lock site holds the guard across an `.await`), which meant switching
   every `.lock().unwrap()` to
   `.lock().unwrap_or_else(PoisonError::into_inner)` — `std::sync::Mutex`,
   unlike `tokio::sync::Mutex`, poisons on a panic-while-held, and a panic
   reachable from one MCP tool call must not crash the desktop app on its
   own next frame.

   `fieldcad-mcp`'s `run_http`/`run_unix`/`run_stdio` moved from `main.rs`
   into the library as `pub` `bind_*`/`serve_*` pairs so the desktop
   (`apps/fieldcad-desktop/src/mcp.rs`) can learn "did the bind succeed,
   what address did it get" — via a bounded 2-second
   `std::sync::mpsc::Receiver::recv_timeout` on the calling UI thread —
   without duplicating the transport setup or waiting on the whole
   long-running serve loop. `cargo build --workspace`, `cargo test
   --workspace` (376 passed), `cargo clippy --workspace --all-targets`, and
   `fieldcad --smoke` (confirms the app still boots/renders after
   `WindowState.data_source` changed from `Box<dyn FieldDataSource>` to
   `Arc<Mutex<HeadlessServer>>`) are all clean. Confirmed by the user in the
   actual running desktop app (this environment cannot drive an interactive
   GUI window itself): clicking Enable MCP, copying the token, and
   connecting a real agent all work end-to-end against the live session.

   **Follow-up done (2026-08-05): connection indicator.** The panel had no
   way to tell whether anything was actually connected — relevant both for
   deciding it's safe to disable the server and for validating a new client
   config. `fieldcad-mcp` gained `McpConnections`, a queryable handle onto
   the same `LocalSessionManager` session table `serve_http`/`serve_unix`
   use (not a snapshot of it) — constructed by the caller *before* the
   server starts, so a UI can hold it immediately with no round trip.
   `McpRunning` carries one; the panel shows a colored dot plus "N clients
   connected" / "No client connected" / "Checking…" (the last only for the
   instant a request happens to be touching the session table — a
   non-blocking `try_read`, never awaited from the UI thread), with a
   tooltip noting the honest caveat: a session persists until explicitly
   closed, so this can lag behind a client that vanished uncleanly. Verified
   with a real client, not just type-checking: a new test
   (`crates/fieldcad-mcp/tests/connections.rs`) drives an actual `initialize`
   handshake over a real TCP connection against `serve_http` and asserts the
   count goes from 0 to 1. `cargo test --workspace` (382 passed) and `cargo
   clippy --workspace --all-targets` stay clean.

   **Phase 5 is complete** for what's actually needed today: everything in
   its original scope (localhost by default, mandatory bearer token even on
   loopback) is done, with the embedding work as its actual motivating
   deliverable. Not built, deliberately: an opt-in **non-loopback** bind —
   nothing has needed off-machine access yet, and that needs its own
   explicit design (an opt-in flag plus a defined threat model for a token
   leaving the local machine), not just "a token now exists."
6. **Crate choice**: `rmcp`, the official Rust MCP SDK, is the likely
   candidate — verify its current maturity and Streamable-HTTP transport
   support at implementation time rather than assuming.

   **Done, retroactively — decided in phase 3, not a separate step.** `rmcp`
   3.1.0's maturity and Streamable-HTTP support were verified against its
   vendored source and its own integration tests before any code was
   written on top of it (see phase 3's notes), and every phase since has
   built on that choice without friction. Recorded here only to close out
   the numbered list; no new work.
7. **Test it the way ADR 0001 tests locality**: one integration test drives
   a session entirely through the MCP surface and asserts the resulting
   world/snapshots are identical to the same commands submitted directly
   through `CommitWorld`/`FieldDataSource`. That test is what makes "MCP is
   just another transport" a checked property instead of a claim, exactly
   the way ADR 0001's local-vs-loopback test already is for that boundary.

   **The only phase from the original plan still open.** Next up.

   **Research notes (paused 2026-08-05, resume here):**

   - **The exact ADR 0001 test to mirror**: `local_and_loopback_sources_are_interchangeable_for_consumers`
     (`crates/fieldcad-simulation/src/lib.rs:938`), which asserts
     `observed_script(&mut local) == observed_script(&mut remote)`.
     `observed_script(source: &mut dyn FieldDataSource) -> Vec<String>`
     (`crates/fieldcad-simulation/src/lib.rs:857`) drives a source through a
     fixed sequence — `Step`, `CommitWorld([CreateObject])`, `Play` +
     `poll(250ms)`, `Pause` — settling with 4× `poll(Duration::ZERO)` after
     each command, and after each settle appends one formatted line to the
     log: snapshot sequence/world_revision/tick, simulation mode, sample
     count, freshness label, object count, world revision. It ends with one
     line summarizing probe-history length/first-reading via
     `ProbeHistory`/`TestFieldPlugin`'s `scalar_channel_id`. **Key finding:
     "identical" here means string equality on this hand-picked semantic
     projection, not a bit-for-bit struct `Eq`** — deliberately: wall-clock
     timing and other incidental fields aren't expected to match, only the
     observable, meaningful state. (There's a separate, *stricter*
     bit-for-bit comparison — `ReplayObservation`, derives `PartialEq`,
     `crates/fieldcad-simulation/src/recording.rs:87`, used by
     `SessionRecording::replay` at `recording.rs:55` — but that's used only
     to check *one* source type's replay determinism against itself
     (`a_recorded_command_sequence_replays_bit_identically`,
     `loopback_replay_is_deterministic_despite_deferred_snapshots`, both in
     `lib.rs`), not for cross-transport comparison. The semantic-projection
     approach is the right template for this phase, not the strict one.)

   - **`SessionRecording::replay` cannot be reused directly for the MCP
     side.** It's generic over `&mut dyn FieldDataSource`, so it works for
     `LocalDataSource`/`LoopbackDataSource`/`AsyncLocalDataSource`/
     `HeadlessServer` (once wrapped) alike — but `McpServer`
     (`crates/fieldcad-mcp/src/lib.rs:327`) wraps
     `Arc<std::sync::Mutex<HeadlessServer>>` and exposes `rmcp`-typed async
     tool methods returning `CallToolResult` (a JSON text block), not the
     `FieldDataSource` trait itself. The new test needs its own driver for
     the MCP side that issues the equivalent tool calls and parses their
     JSON results into the same log-line format `observed_script` uses.

   - **Existing pattern to copy for that driver**: `fieldcad-mcp`'s own
     unit-test module (`crates/fieldcad-mcp/src/lib.rs:662`, `mod tests`)
     already shows the shape — `server()` (line 671) builds
     `McpServer::new(Arc::new(Mutex::new(HeadlessServer::new(fieldcad_server::default_session().unwrap()))))`,
     and `json_of(&CallToolResult) -> serde_json::Value` (line 679) extracts
     the single text content block every tool returns and parses it. These
     are private to that module (not `pub`), so the new test — which should
     live in `crates/fieldcad-mcp/tests/` as an integration test, since it
     needs both `fieldcad_simulation` directly and `fieldcad_mcp`'s
     `McpServer` — will need to duplicate this ~10-line helper rather than
     import it (small, not worth plumbing a shared test-util module for).

   - **Command/tool coverage is already sufficient** for a script like
     `observed_script`'s: `McpServer` has `play`, `pause`, `step`,
     `commit_world`, `get_world`, `get_simulation_status`,
     `get_latest_snapshot` (plus `set_time_step`, `set_playback_speed`,
     `set_subscription`, `set_field_system_enabled`, `set_field_model`,
     `set_field_system_realtime`, `reconfigure_domain`, `undo`, `redo`,
     `get_diagnostics`, `get_body_forces` — all added since phase 3, see
     that phase's P0/P1 notes above). No gap here.

   - **One real gap to design around**: `observed_script`'s last line reads
     probe history via `ProbeHistory`/`TestFieldPlugin`'s
     `scalar_channel_id` — there is no MCP tool for probe-history retrieval
     yet (explicitly deferred in phase 3: "probe history/trajectories as
     server-retained series"). The new script for this phase should either
     drop that line and compare only what both sides can observe (snapshot
     identity, simulation status, world state, channel batches from
     `get_latest_snapshot`), or read the same information a different way
     via `get_latest_snapshot`'s channel batches instead of `ProbeHistory`.
     Lean toward dropping/adapting rather than adding a probe-history MCP
     tool as a prerequisite — that's its own scoped feature, not blocking
     for this parity check.

   - **Seeding**: don't reuse `fieldcad-simulation`'s private `#[cfg(test)]`
     helpers (`runtime()`, `seeded_world()`, `domain()`, `time_step()`,
     `TestFieldPlugin`) — they're not exported. Simplest approach: call
     `fieldcad_server::default_session()` independently for each side (it's
     deterministic — same domain, time step, `SessionId::from_u128(1)`,
     empty world, electrostatics active + electromagnetism composed
     inactive) and apply the *same* script of commands to each, once via
     direct `FieldDataSource` calls (`HeadlessServer::execute`/`submit`),
     once via the equivalent `McpServer` tool calls. This sidesteps needing
     a shared seeded-runtime builder across crates.

   - **One asymmetry to handle explicitly, not paper over**: the direct
     `FieldDataSource` side needs manual settling between commands (a
     `settle()` helper calling `poll(Duration::ZERO)` a few times, exactly
     like `observed_script` does) because `AsyncLocalDataSource` resolves
     non-blocking submissions on a background worker thread. The MCP side
     does **not** need this — every tool call already goes through
     `submit_and_wait` (`crates/fieldcad-mcp/src/lib.rs`, `COMMAND_TIMEOUT`
     = 30s), which awaits the command's own completion via the
     `HeadlessServer::submit_and_await` waiter (see phase 5's concurrency
     fix) before the tool call returns. So the MCP-side script is just a
     sequence of `.await`ed tool calls with no explicit settling — don't
     add a redundant settle step there, and don't be surprised the two
     drivers look structurally different for this reason.

   - **Suggested shape for the new test** (not yet written): new file
     `crates/fieldcad-mcp/tests/parity.rs`. Two independent
     `fieldcad_server::default_session()`-backed sessions; one driven
     directly through `HeadlessServer`/`FieldDataSource` with explicit
     `settle()` calls between commands (mirroring `observed_script`); the
     other driven through `McpServer` tool calls with no settling needed;
     both projected into the same `Vec<String>` log format (adapted to drop
     or replace the probe-history line per the gap above); `assert_eq!` the
     two logs. That equality is what turns "MCP is just another transport"
     from a claim into a checked property, exactly as the docstring above
     says.

Note explicit scope dependency: user-stories/README.md marks several
stories *Required for API/MCP parity* that aren't implemented yet (stable
scene creation/identifiers, particle-template creation as a command, rename,
domain/config mutation, structured preflight validation, run comparison,
save/export/import). The MCP server doesn't have to wait for all of these —
it can launch covering the stories already marked *Implemented* and grow as
the rest land — but full parity with "everything a person can do" is gated
on that list, not just on the transport work above.
