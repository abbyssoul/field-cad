# Field CAD user stories

Status: living product and API inventory.  
Last reviewed: 2026-08-04.

## Purpose and scope

These stories describe interactions with Field CAD independently of the desktop
UI. They are the contract that a future UI, HTTP API, or MCP server should be
able to express. “User” includes a scientist using the desktop application and
an automation or AI agent acting through a remote client.

The goal is parity of *meaning*, not parity of mouse gestures: a remote client
must be able to construct and inspect the same experiment, submit the same
validated changes, run it with the same timing rules, and obtain the same
provenanced observations. Viewport camera and layout actions are deliberately
per-client conveniences, not shared experiment mutations.

### Reading the stories

- **Implemented** means the capability exists in the current model/runtime;
  a desktop control may be the only current client.
- **Required for API/MCP parity** identifies a necessary public capability that
  is not yet exposed or complete. It is a product requirement, not a claim of
  implementation.
- A **world edit** changes the authored scene and produces a new world
  revision. It is submitted as one validated, atomic transaction.
- A **session command** controls the authoritative data source (local or
  remote) and is acknowledged with a command identity and outcome.
- A **subscription/view preference** changes what a client receives or draws;
  it never changes the physics.
- An **observation** is immutable output with snapshot, world, simulation,
  solver, domain, precision, and validity provenance.

## Product model: what is authoritative

| State class | Owned by | Examples | API/MCP consequence |
| --- | --- | --- | --- |
| Authored world | experiment/session | objects, components, planes, probes, their visibility and probe channels | Read and mutate through revisioned world transactions. |
| Experiment composition | authoritative data source | active field systems, chosen model for each shared field, time step | Read and mutate through correlated session commands. |
| Simulation state | authoritative data source | paused/running mode, tick, time, dynamically integrated pose/velocity | Read as status/snapshots; advance only through run commands. |
| Observations | data source | field samples, probe histories, trajectories, diagnostics | Return immutable, versioned results; do not make clients reconstruct them. |
| Client presentation | individual client | selection, camera, panel layout, layer style, floating plots | Keep local unless explicitly saved as a named view. |
| Transport subscription | individual client plus source acknowledgement | requested channels, planes, grid stride, level of detail | Set independently of the world; re-establish after reconnect. |

## User stories

### 1. Start, discover, and connect

- **US-01 — Create a new scene** *(Required for API/MCP parity)*  
  As a modeller, I want to create an empty or explicitly templated scene so
  that I can begin a reproducible experiment.  
  Acceptance: the result has a stable experiment/session identifier, initial
  world revision, declared schemas/catalog versions, field-system composition,
  and initial run configuration. Creating a scene must use the same validation
  path as later edits and must not leave unwanted undo entries.

- **US-02 — Open an existing scene/experiment** *(Required for API/MCP parity)*  
  As a modeller, I want to load a saved experiment so that I can continue or
  reproduce prior work.  
  Acceptance: all authored state and experiment configuration are restored with
  their schemas, units, plugin/model versions, domain, numerical settings, and
  provenance; incompatibility is reported, never silently adapted.

- **US-03 — Inspect the current scene** *(Implemented)*  
  As a modeller, I want to retrieve the complete current world, including its
  revision and schemas, so that I can understand what is in the experiment
  before changing it.

- **US-04 — Discover capabilities and constraints** *(Implemented in model;
  Required for API/MCP exposure)*  
  As an automation client, I want to list available field systems, component
  schemas, particle catalog templates, channels, configuration schemas, and
  validation limits so that I can make valid choices without hard-coded UI
  knowledge.

- **US-05 — Connect to and monitor a compute source** *(Implemented)*  
  As a remote client, I want to know whether a source is connecting, ready,
  disconnected, or failed, and its pending command count, so that I can act on
  a trustworthy session rather than infer state from frames.

### 2. Construct and manage simulated objects

An object is a named entity with transform, velocity, optional shape,
visibility, pinning, and independent physical components. Objects become
simulated only when the active systems consume their components; an object with
no such components is still a valid, non-simulated scene object.

- **US-10 — List simulated and non-simulated objects** *(Implemented)*  
  As a modeller, I want to list every object and inspect its ID, name, pose,
  velocity, shape, visible state, pinned state, components, and whether the
  active experiment treats it as a source and/or dynamic body, so that I can
  distinguish what is merely drawn from what participates in physics.

- **US-11 — Add an object** *(Implemented)*  
  As a modeller, I want to create an object with a name, transform, velocity,
  optional point/sphere/box shape, visibility, pinning, and optional initial
  components so that I can construct a scene in one transaction.

- **US-12 — Remove an object** *(Implemented)*  
  As a modeller, I want to remove an object so that it no longer appears or
  participates in the experiment.  
  Acceptance: references such as attached probes are validated/rejected or
  handled explicitly; no dangling relationship is silently retained.

- **US-13 — Edit an object’s placement and motion** *(Implemented)*  
  As a modeller, I want to set an object’s position, orientation, linear and
  angular velocity, shape and dimensions so that I can define its initial or
  authored state.  
  Acceptance: transforms, velocities, and shapes are finite and valid; values
  are stored in SI units even if a client formats another unit.

- **US-14 — Choose who controls an object’s motion** *(Implemented)*  
  As a modeller, I want to pin or unpin an object so that it follows my
  authored transform/velocity or is eligible for solver integration.

- **US-15 — Show or hide an object without changing physics** *(Implemented)*  
  As a modeller, I want to control object visibility so that I can declutter a
  view without accidentally removing a source or dynamic body.

- **US-16 — Add, edit, and remove physical components** *(Implemented)*  
  As a modeller, I want to attach independently declared components (for
  example charge, inertial mass, or a plugin-defined property), edit every
  schema-defined property, and detach a component so that I can compose the
  physical role of an object without hidden species behaviour.

- **US-17 — Create an object from a particle template** *(Required for API/MCP
  parity)*  
  As a modeller, I want to instantiate a versioned catalogue template such as
  electron, proton, positron, or neutron so that published mass/charge values
  and their provenance are applied consistently.  
  Acceptance: template creation is equivalent to a normal object plus standard
  components; later edits retain the object but record that it no longer claims
  the template’s unchanged values.

- **US-18 — Rename objects and all authored instruments** *(Required for API/MCP
  parity)*  
  As a modeller, I want to rename objects, planes, and probes so that I can
  keep a meaningful, automation-friendly scene inventory.  
  Note: names exist in the model but dedicated rename commands are not yet
  present; they should not require delete-and-recreate because IDs and
  attachments must survive.

### 3. Configure the physical experiment

- **US-20 — List field systems and their declared fields** *(Implemented)*  
  As a modeller, I want to inspect every available equation system, its version,
  description, channels, component schemas, configuration schema, current
  configuration, realtime setting, and enabled state so that I can understand
  the experiment’s physical choices.

- **US-21 — Activate or deactivate a field system** *(Implemented)*  
  As a modeller, I want to enable or disable a field system so that I can choose
  which physics participates and publishes observations.  
  Acceptance: incompatible combinations and invalid worlds are rejected at the
  authority; component schemas remain available when a system is inactive.

- **US-22 — Choose a model for each shared field** *(Implemented)*  
  As a modeller, I want to select which active equation system computes a
  shared physical field (or choose not to compute it) so that, for example,
  electrostatics and Maxwell are alternatives for one electric field rather
  than contradictory duplicate fields.

- **US-23 — Configure a field system and numerical domain** *(Required for
  API/MCP parity)*  
  As a modeller, I want to create or change the domain, resolution, boundaries,
  precision, solver configuration, initial conditions, and coupling parameters
  that a selected model exposes so that I can define a reproducible numerical
  experiment.  
  Acceptance: schemas and domain constraints are discoverable; changes are
  validated before adoption; an accepted snapshot reports the values that
  produced it. The present desktop reports these settings but does not yet
  expose a general editing command.

- **US-24 — Control recomputation during an interactive edit** *(Implemented)*  
  As a modeller, I want to choose per field system whether it recomputes for
  intermediate drag/text values or only when I commit so that I can trade
  immediate feedback for responsiveness without changing the final physics.

- **US-25 — Validate a proposed change before committing it** *(Implemented in
  authority; Required for API/MCP exposure)*  
  As an automation client, I want a structured validation result for a proposed
  world or experiment transaction so that I can repair invalid input before
  attempting a run.  
  The authority remains the final validator; preflight is advisory and must use
  the same schema/domain/plugin rules.

### 4. Build and manage non-simulated measurement objects

Planes and probes are world entities used to observe a field. They never act as
physical sources or alter a solver result.

- **US-30 — List measurement instruments** *(Implemented)*  
  As a modeller, I want to list slice planes and probes with their IDs, names,
  geometry/attachment, channels, visibility, and current status so that I can
  understand how the experiment is observed.

- **US-31 — Add, edit, duplicate, hide, and remove a slice plane** *(Implemented,
  except rename)*  
  As a modeller, I want to create a bounded, orientable slice plane; set its
  origin, normal, in-plane orientation and extents; choose standard XY/XZ/YZ
  orientations; duplicate it; hide it; and remove it so that I can inspect a
  field across relevant surfaces.

- **US-32 — Add, position, attach, show/hide, and remove a probe** *(Implemented,
  except rename)*  
  As a modeller, I want to create a point probe, place it at a world coordinate
  or attach it with a local offset to an object, detach it at its current world
  position, hide it, and remove it so that I can measure a fixed or moving
  location.

- **US-33 — Choose what a probe records** *(Implemented)*  
  As a modeller, I want to select the declared field channels a probe records
  so that its bounded history contains the quantities relevant to my question.
  Channels remain selectable while their field system is inactive, allowing a
  recorder configuration to survive a model change.

- **US-34 — Inspect and plot probe history** *(Implemented locally; Required for
  API/MCP exposure)*  
  As a modeller, I want to retrieve a probe’s time series, component selection,
  sample validity, and snapshot/time provenance so that I can compare values
  over an experiment run.  
  A floating plot is a client view; the recorder data is an observation.

### 5. Run and control simulation

- **US-40 — Inspect run state and progress** *(Implemented)*  
  As a modeller, I want the authoritative mode, tick, reconstructed simulation
  time, time step, playback speed, world revision, snapshot sequence/freshness,
  queued command count, and source state so that I know exactly what has run.

- **US-41 — Start and pause a run** *(Implemented)*  
  As a modeller, I want to play or pause simulation so that I can control when
  fixed simulation ticks advance. The acknowledgement must identify whether the
  command has been applied or queued.

- **US-42 — Advance exactly one tick** *(Implemented)*  
  As a modeller, I want to step one fixed time interval while paused so that I
  can inspect deterministic state transitions.

- **US-43 — Set numerical time step** *(Implemented)*  
  As a modeller, I want to set a finite positive `dt` so that I can control
  numerical resolution.  
  Acceptance: the authority rejects invalid or model-unstable steps (including
  a Maxwell Courant violation) before adopting the new clock configuration.

- **US-44 — Set playback rate without changing physics** *(Implemented)*  
  As a modeller, I want to set a positive wall-clock speed multiplier so that I
  can watch a run faster or slower without altering fixed `dt` or results.

- **US-45 — Edit safely while simulation is running** *(Implemented)*  
  As a modeller, I want an authored edit submitted during a run to be applied
  atomically immediately before a fixed tick, in submission order, so that the
  result does not depend on GUI frame cadence. The client must receive an
  initial applied/queued acknowledgement and, for queued work, a final
  applied/rejected outcome at the tick boundary.

- **US-46 — Bracket an interactive edit** *(Implemented)*  
  As a UI client, I want to mark the beginning and end of a multi-frame gesture
  so that the run suspends while I author intermediate values, recommences only
  if it was previously running, and treats the gesture as one history entry.
  MCP/API clients normally submit a final transaction instead; they may use this
  capability only when deliberately streaming an edit.

### 6. Inspect results and assess validity

- **US-50 — Retrieve the latest complete field snapshot** *(Implemented)*  
  As a modeller, I want to retrieve immutable field snapshot metadata and
  channel batches so that I can inspect a complete result even while the next
  solve is in flight.  
  Acceptance: snapshot identity includes session, sequence, world revision and
  simulation time; freshness against the current world is explicit.

- **US-51 — Request field sampling appropriate to the question** *(Implemented)*  
  As a client, I want to subscribe to channels and request probe points, visible
  slice-plane lattices, and a decimated whole-domain grid so that I receive the
  observations I need without changing the computed field.  
  Acceptance: the source validates per-axis and total sample budgets and
  acknowledges the adopted subscription; clients renew it after reconnect.

- **US-52 — Inspect scalar/vector field values and validity** *(Implemented)*  
  As a modeller, I want every returned value to include its channel, unit,
  geometry, representation, and validity (exact, interpolated, or a specific
  undefined reason) so that I do not mistake a singularity, domain boundary,
  periodic seam, overflow, or unconverged value for a measurement.

- **US-53 — Inspect solver diagnostics** *(Implemented)*  
  As a modeller, I want to retrieve structured diagnostics with severity,
  stable code, message, and producing plugin so that I can assess stability,
  residuals, divergence, energy, conservation, source/session errors, and
  deferred work.

- **US-54 — Inspect dynamic-object outcomes** *(Implemented in snapshots/world;
  Required for API/MCP exposure)*  
  As a modeller, I want to list object transforms and velocities at an explicit
  tick/world revision and obtain trajectories over a selected interval so that
  I can analyse solver motion rather than infer it from a viewport.

- **US-55 — Compare observations across runs** *(Required for API/MCP parity)*  
  As a researcher, I want to name, retain, and compare snapshots, probe series,
  and diagnostics from multiple runs/configurations so that I can attribute a
  difference to a stated model or parameter change.

### 7. History, recording, reproducibility, and exchange

- **US-60 — Undo and redo authored edits** *(Implemented)*  
  As a modeller, I want to restore the preceding or following captured authored
  scene while paused so that I can correct construction mistakes.  
  Acceptance: a restore creates a new world revision, preserves stable IDs,
  validates against active systems, and does not rewind simulation time.

- **US-61 — Inspect edit history** *(Implemented status; Required for API/MCP
  exposure)*  
  As an automation client, I want to see whether undo/redo is available and the
  label of the change it would restore so that I can make intentional history
  operations. Dynamic solver motion clears incompatible authored history.

- **US-62 — Record and replay a semantic session** *(Implemented in runtime;
  Required for API/MCP exposure)*  
  As a researcher, I want to record semantic commands and elapsed intervals,
  then replay them against a fresh session so that I can reproduce a result
  independently of rendered frames and UI timing.

- **US-63 — Save, export, import, and share an experiment** *(Required for
  API/MCP parity)*  
  As a researcher, I want to persist and exchange a documented experiment,
  optional named views, recordings, and selected observations so that another
  person or agent can reproduce, audit, and extend it. Export must include
  identifiers, schemas, units, catalogue/model/plugin versions, numerical
  configuration, provenance, and any compatibility warnings.

### 8. Navigate and present the scene

These are intentionally client-local stories. An API/MCP client may expose
them for a visualizer, but they are not needed to construct or run an
experiment.

- **US-70 — Select and focus a scene item** *(Implemented)*  
  As a desktop user, I want to select an object, probe, plane, or simulation
  node from the scene tree or 3D view and focus the camera on it so that I can
  inspect and edit the intended subject.

- **US-71 — Navigate the 3D view** *(Implemented)*  
  As a desktop user, I want to orbit, pan, zoom, reset, choose an axis view,
  and switch perspective/orthographic projection so that I can inspect spatial
  relationships accurately.

- **US-72 — Manipulate selected geometry in the viewport** *(Implemented)*  
  As a desktop user, I want to drag axis, plane, free-translation, and plane
  normal gizmos so that I can reposition an object, probe, or slice plane
  directly. These gestures resolve to the same world commands as inspector
  edits.

- **US-73 — Choose what is drawn** *(Implemented)*  
  As a desktop user, I want to show/hide scene classes and field layers, choose
  per-plane magnitude colour/vector arrows and vector mode, and choose
  whole-domain vector density/scale so that I can make a readable view without
  changing observation values or physics.

- **US-74 — Open help and client diagnostics** *(Implemented)*  
  As a desktop user, I want to open usage help and a diagnostics window so that
  I can discover workflows and investigate current source/solver issues.

## API/MCP design rules derived from the stories

1. **Make the model the core.** UI actions and MCP/API calls translate to the
   same typed `WorldCommand` transaction or `CommandPayload`; neither gets a
   privileged mutation path.
2. **Separate reads, mutations, and streams.** Provide snapshot/world/status
   reads; correlated commands with receipts; and subscriptions/events for
   snapshot publication, queued-command completion, source status, and
   diagnostics. Do not use polling alone to infer command success.
3. **Address all durable entities by stable IDs, not names.** Names are human
   labels and may collide or change. Creation responses must return allocated
   object/plane/probe IDs and the committed world revision.
4. **Use explicit optimistic concurrency.** A world transaction should carry an
   expected revision (or an explicit “apply to latest” policy) and return the
   committed revision or a structured conflict. A batch is all-or-nothing.
5. **Keep authored edits distinct from run controls and observation requests.**
   Sampling density, visual layers, camera, and plot layout must never mutate
   the physical world or numerical result.
6. **Expose schemas and structured errors.** Automation needs component,
   configuration, field, unit, domain, and budget schemas before it can safely
   construct valid commands. Rejections need a machine-readable code, affected
   path/entity, and human explanation.
7. **Preserve provenance and validity end-to-end.** Every observation needs
   enough metadata to reproduce or reject it; undefined and stale are valid
   outcomes, not missing fields to be silently filled.
8. **Treat remote and local sources equivalently.** The same command ordering,
   tick-boundary queuing, acknowledgement, snapshot completeness, and history
   rules apply regardless of where computation runs.

## Suggested MCP surface (capability-oriented, not a final wire protocol)

| Capability | Read/action examples |
| --- | --- |
| Scene lifecycle | `create_scene`, `open_scene`, `save_scene`, `get_scene`, `list_capabilities` |
| World inventory | `list_objects`, `get_object`, `list_planes`, `list_probes`, `get_world_revision` |
| World mutation | `commit_world(expected_revision, commands)`, `validate_world_transaction` |
| Experiment configuration | `list_field_systems`, `set_field_system_enabled`, `set_field_model`, `set_field_system_configuration`, `set_domain` |
| Simulation control | `get_simulation_status`, `play`, `pause`, `step`, `set_time_step`, `set_playback_speed` |
| Observation | `set_subscription`, `get_latest_snapshot`, `sample_field`, `get_probe_history`, `get_trajectory`, `get_diagnostics` |
| Events | `watch_session` for snapshot/status/diagnostic/queued-command-completion events |
| Reproducibility | `get_history`, `undo`, `redo`, `record_session`, `replay_session`, `export_experiment` |

The existing `CommitWorld`, field-system, transport, and snapshot abstractions
are a useful starting point for this surface. The missing work is chiefly a
transport-neutral serialization contract, capability/schema discovery, durable
scene lifecycle, configuration mutation, and structured asynchronous events —
not a second model for remote clients.
