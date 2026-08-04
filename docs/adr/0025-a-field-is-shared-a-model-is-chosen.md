# 0025 — a field is shared; the model that computes it is chosen

Status: **accepted** (generalises
[0017](0017-share-physical-source-schemas-across-equation-systems.md), revisits
[0014](0014-scene-level-field-system-activation.md))

## Context

`ChannelId` was plugin-namespaced, so the electrostatics plugin published
`fieldcad.electrostatics.electric-field` and the electromagnetism plugin
published `fieldcad.electromagnetism.electric-field`. The inspector duly listed
three fields for a scene that has two: "Electric field E", "Magnetic flux
density B", and "Electric field".

That was not a labelling problem. With both systems active the scene really did
contain two electric fields: two solvers each publishing values under their own
identity, and — since [0022](0022-dynamics-is-a-first-party-system.md) — each
contributing the force *its* field exerts on a charge. A charged body was
accelerated by `qE` twice, from two disagreeing models of one interaction. A
probe recording "the" electric field had to be told which plugin's, and a
recorded series stopped meaning anything if the model was later changed.

[0017](0017-share-physical-source-schemas-across-equation-systems.md) already
settled this for the *input* side: charge is a property of a world object, not
of a solver, so its schema lives in a shared module and several systems consume
it. The output side had never been given the same treatment, and there is no
reason it should differ. The electric field is a property of the scene. That an
electrostatic evaluator or a time-domain lattice computes it is a statement
about method, not about how many fields exist.

The same shape is already visible in the next system: Newtonian gravity and a
relativistic model would compute one gravitational field, not two.

## Decision

**A field channel names a physical quantity, and its identity is shared.** The
canonical schemas for `E`, `B`, and electric potential live in
`fieldcad-electromagnetic-sources` alongside the charge schema — that module is
now the shared electromagnetic model, both its inputs and its fields. Any
equation system may declare them. Channels that only make sense for one method —
an FDTD divergence residual, an energy density defined on a Yee lattice — stay
in that plugin's namespace, because they are diagnostics of a discretization
rather than quantities the world has.

**Declarations compose or are rejected, exactly as component schemas are.** The
runtime accepts several systems declaring one channel when the schemas are
identical, and rejects incompatible ones before any solver is created. The
`ForeignChannel` rule — a plugin may only name channels in its own namespace —
is gone, because a shared field has no owning plugin; the composition check is a
stronger guard in its place.

**At most one active system computes any field.** Refused at construction and at
activation, never silently resolved: which model computes a field is the user's
choice, not a consequence of registration order. This is the rule that makes
double-solving structurally impossible, and with it the double force — a system
that models a field publishes it, so it cannot contribute that field's force
while another system owns the field.

**A snapshot channel carries its provider.** The identity can no longer say who
produced a value, so `ChannelSnapshot` records the plugin that did. Provenance
was previously derivable from the identifier by accident; now it is stated.

**Choosing a model is one command.** `SetFieldModel { channel, provider }`
stands the old model down and brings the new one up as a unit. A deactivation
followed by an activation would pass through a state in which nothing computes
the field the user is asking about, and strand them there if the second half
were refused.

The field system remains the activation unit
([0014](0014-scene-level-field-system-activation.md)). A solver computes all of
its fields or none of them — Maxwell cannot advance `E` without `B` — so
choosing it as the model of one field chooses it for the rest, and every system
it overlaps stands down. Refusing instead would leave a field whose only model
overlaps an active one unreachable from its own control: with electrostatics
computing `E`, there would be no way to ask for `B`. The consequence is visible
rather than hidden, because the inspector's field list shows both rows change.

## Consequences

The inspector's Simulation node now leads with **Fields** — one row per physical
field, each naming the model computing it and offering the alternatives where
there are any — with **Field systems** below as what those models are made of. A
field no active system computes is still listed, so the control that would turn
it on is reachable. A system whose fields are already taken shows why it cannot
be activated and points at the choice that works.

A probe records a field, not a plugin's output, so its series survives a change
of model — which is what makes "run this scene under electrostatics, then under
Maxwell, and compare" a thing this application can express at all. The recorded
values change, and the snapshot says which model produced each.

**What this trade costs.** The desktop's default scene now composes Maxwell
inactive and solves its single stationary charge analytically, where it
previously ran both. That is a change in default behaviour, and it is the honest
one: the scene never wanted two electric fields.

Two systems can also no longer be compared side by side in one session, which
was never a valid physical composition but was a convenient way to eyeball
agreement between an analytic oracle and a numerical scheme. That comparison
belongs to the harness and to tests, where both solvers can be driven directly
over the same world without pretending the scene has two fields — which is where
the electrostatic benchmarks already do it.
