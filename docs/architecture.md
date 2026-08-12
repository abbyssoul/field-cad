# Field CAD architecture overview

Field CAD has one authoritative experiment model with multiple clients. The
desktop client, MCP clients, and future network clients all issue validated
commands to the same session owner and consume the same versioned observations.

```text
desktop UI / MCP client / future client
                 |
                 v
          HeadlessServer
                 |
                 v
 simulation runtime + equation-system plugins
                 |
                 v
 immutable, revisioned observations
                 |
                 v
  desktop rendering, probes, diagnostics, remote readers
```

## Responsibilities

`HeadlessServer` owns the authoritative world, experiment composition,
simulation clock, command queue, and published state. It accepts commands and
exposes reads through the transport-neutral `FieldDataSource` contract.

The desktop application is a client and presentation shell: it maps input to
commands, renders published observations, and keeps camera, selection, layout,
and drawing preferences local. It must not inspect solver-owned memory.

Equation-system plugins own physical equations, their solver state, and the
field channels they publish. The runtime validates and adopts all world changes
and is the only writer of authoritative state. Outputs are immutable, versioned
observations with provenance and sample validity.

## MCP and remote control

MCP is a thin transport over `HeadlessServer`, not a separate model or a UI
automation layer. Its tools translate to the same validated commands and reads
that desktop clients use. This lets an agent construct, run, and inspect the
same experiment without recreating physics rules outside the authority.

Parity means parity of experiment meaning—not parity of mouse gestures. Camera
movement, panel layout, and other presentation preferences are per-client;
world edits, experiment configuration, run control, and observations belong to
the authoritative session.

The current MCP server supports stdio, local IPC, and loopback HTTP. Loopback
and local IPC are intentional current security boundaries; see
[MCP implementation and security notes](mcp-plan.md) for transport details and
[the user-story inventory](user-stories/README.md) for implemented and planned
capabilities.

## Where to go deeper

- [CONTEXT.md](../CONTEXT.md) defines the domain vocabulary, invariants, and
  detailed system model.
- [Architecture decisions](adr/README.md) explain durable boundaries and their
  trade-offs.
- [Contributing](../CONTRIBUTING.md) maps the workspace and verification flow.
