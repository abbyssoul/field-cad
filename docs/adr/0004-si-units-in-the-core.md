# 0004 — SI in the core, conversion only at display

Status: **accepted** (Milestone 0)

## Context

Scientific software that stores "2" and remembers elsewhere that it means
nanocoulombs eventually gets the elsewhere wrong. Mixed-unit storage is a
recurring, expensive source of silent error, and silent error is precisely what
this project claims not to produce.

## Decision

Every stored physical quantity is SI, and carries its dimension as data:

- `Dimension` is a seven-exponent vector over the SI base units.
- `Quantity` and `VectorQuantity` pair an SI magnitude with a `Dimension` and
  reject non-finite values at construction.
- Property and channel schemas declare a dimension. `charge = 2` without a unit
  is not a valid value, and `validate_properties` rejects it.
- Display-unit conversion happens in the UI layer, on the way out.

## Consequences

- A dimension check is a cheap equality test on seven `i8`s, done at edit time
  and at snapshot publication — not per value in a hot loop.
- Field values in bulk are stored as bare `f64`/`DVec3` columns, with the
  dimension carried once by the channel schema
  ([0006](0006-columnar-batched-field-sampling.md)). The type system's guarantee
  is at the boundary, not on every element.
- No dimensional *arithmetic* is provided. Multiplying a charge by a field to get
  a force is a plugin's job today. If that starts producing bugs, add checked
  operators — until then it would be unused machinery.
- The UI must always show units. A number with no unit is a defect.
