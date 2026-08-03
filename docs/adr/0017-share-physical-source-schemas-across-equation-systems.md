# 0017 — share physical-source schemas across equation systems

## Context

Electrostatics originally owned the charge component schema and the translation
from authored objects to point/sphere sources. Maxwell reused that code by
depending on the electrostatics plugin. That inverted the intended composition:
charge is a property of the world, while electrostatics and time-domain
electromagnetism are independent equation-system Implementations that consume
it. Milestone 6 also needs stable object identity and velocity for deposition
and particle coupling.

The runtime additionally required every component identifier to use the
contributing plugin's namespace. That prevented two equation systems from
declaring the same physical property without either duplicating it or choosing
one solver as the owner.

## Decision

- `fieldcad-electromagnetic-sources` is the shared physical-source Module. It
  owns the stable charge schema, point/sphere source extraction, and a source
  record containing object identity, position, velocity, charge, and
  distribution.
- Electrostatics and electromagnetism depend on this Module, not on one another.
- More than one plugin may contribute an identical component schema. The
  simulation runtime registers that schema once; incompatible definitions with
  the same identifier are rejected before any solver is created.
- Equation-system-specific representation checks remain in each solver. The
  shared Module only validates the common authored charge shape and value.

## Consequences

Electrostatics and Maxwell now compose through one charge property without an
Implementation dependency. Maxwell can be run alone and still author and
consume charged objects. Milestone 6 can retain object identity and velocity
through the source Adapter rather than rediscovering them in solver code.

The existing identifier type still calls its namespace a `PluginId`; the shared
Module therefore uses `fieldcad.electromagnetic-sources` as a schema namespace
even though it is not an active field system. A future serialized plugin
contract may generalize that name, but identity and behaviour are unambiguous
now. Shared schemas increase composition power, so equality is strict: display
metadata and property definitions must all agree.

Status: accepted.
