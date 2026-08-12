# Field CAD desktop user guide

## Start an experiment

Run the desktop application with:

```shell
cargo run -p fieldcad-desktop
```

The Scene panel describes the experiment: its Simulation node holds the domain
and active field systems; objects carry independent physical components; probes
and slice planes sit under Measurement because they observe physics without
changing it. Select an item to edit it in the Inspector.

Start with an object, attach charge or mass as needed, choose the active field
system in the Simulation inspector, and add a probe or slice plane to choose
what to observe. The initial scene provides a useful electrostatic/electromagnetic
starting point, but a saved experiment is the reproducible unit of work.

## Run and observe

- Play, Pause, and Step control the deterministic simulation clock.
- `dt` is the fixed simulation time step. Enter bare seconds or values with
  `s`, `ms`, `us`/`µs`, `ns`, `ps`, `fs`, `min`, or `h`.
- `speed` changes wall-clock playback pacing; it never changes `dt`.
- The Compute/Diagnostics views report simulation state, domain settings,
  precision, boundary conditions, and solver diagnostics.
- A probe records selected field channels at its position. Attach it to an
  object when you want the measurement location to move with that object.
- Open a floating probe plot to retain a time-series view while selecting or
  moving other entities.

The Simulation inspector activates field systems and selects the model for a
shared field. Inactive systems preserve their authored settings and object
properties, but do not solve or publish channels.

## Navigate and edit the viewport

- Middle-button drag orbits; Shift + middle-button drag pans; the wheel dolls.
- `1`, `3`, and `7` select the +X, +Y, and +Z views.
- Click an object, probe, or slice plane to select it; `F` frames it and `Esc`
  clears selection.
- Drag a selected entity directly to move it in the camera-oriented view plane.
- Drag the red, green, or blue gizmo arrows to constrain movement to one axis;
  drag a coloured square to constrain movement to a plane.
- A selected slice plane also has a purple normal handle for reorientation.

Grid, axes, diagnostics, objects, probes, slice planes, and individual field
channels are display controls. They affect presentation, not physics.

## Troubleshooting graphics

Use the headless smoke check when a windowed run fails:

```shell
cargo run -p fieldcad-desktop -- --smoke 120
```

`WGPU_BACKEND`, `FIELDCAD_PRESENT_MODE`, and `FIELDCAD_FORCE_FALLBACK` select
the backend, presentation mode, and fallback adapter. See
[graphics troubleshooting](troubleshooting-graphics.md) for details.

For the scientific model, units, validity, and numerical assumptions, read
[CONTEXT.md](../CONTEXT.md).
