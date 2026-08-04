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

Phased plan:

1. **Close the serialization gap** above.
2. **Stand up a headless server**, not an embedded feature of the desktop
   app: a new crate (e.g. `fieldcad-mcp` or `fieldcad-server`) that owns a
   `LocalDataSource`/`AsyncLocalDataSource` from `fieldcad-simulation` and
   runs with no window/GPU dependency — deployable on a machine with no
   display, and exactly the shape ADR 0001 already designed for. Embedding an
   MCP server as an *optional* mode inside `fieldcad-desktop` (so a human
   can watch an agent drive the same live session in real time) is a
   reasonable follow-on once the standalone path works, not a prerequisite.
3. **Map the "Suggested MCP surface" table onto MCP primitives**: world/
   experiment/run mutations become MCP tools (`commit_world`, `play`,
   `pause`, `step`, `set_time_step`, `set_subscription`, `undo`/`redo`, …);
   read-only state (world, simulation status, field systems, latest
   snapshot, probe history, diagnostics) becomes MCP resources, or read
   tools if the chosen SDK's resource model doesn't fit; live updates
   (snapshot publication, queued-command completion, diagnostics) become
   resource-subscription notifications rather than something a client has to
   poll for.
4. **Transport: Streamable HTTP, not stdio.** Stdio MCP is for a client that
   spawns its own local subprocess (e.g. Claude Desktop's typical
   integration); this task explicitly wants remote clients — "a web
   interface or a mobile app or an AI agent" — over a network, which needs
   the HTTP transport.
5. **Security**: bind to localhost by default; require an explicit
   opt-in flag/config plus a bearer token before listening on any
   non-loopback interface, since this is full scene-mutation control, not a
   read-only endpoint.
6. **Crate choice**: `rmcp`, the official Rust MCP SDK, is the likely
   candidate — verify its current maturity and Streamable-HTTP transport
   support at implementation time rather than assuming.
7. **Test it the way ADR 0001 tests locality**: one integration test drives
   a session entirely through the MCP surface and asserts the resulting
   world/snapshots are identical to the same commands submitted directly
   through `CommitWorld`/`FieldDataSource`. That test is what makes "MCP is
   just another transport" a checked property instead of a claim, exactly
   the way ADR 0001's local-vs-loopback test already is for that boundary.

Note explicit scope dependency: user-stories/README.md marks several
stories *Required for API/MCP parity* that aren't implemented yet (stable
scene creation/identifiers, particle-template creation as a command, rename,
domain/config mutation, structured preflight validation, run comparison,
save/export/import). The MCP server doesn't have to wait for all of these —
it can launch covering the stories already marked *Implemented* and grow as
the rest land — but full parity with "everything a person can do" is gated
on that list, not just on the transport work above.