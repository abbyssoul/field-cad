@AGENTS.md

Everything below is supplementary session knowledge accumulated across prior
work on this repo — not duplicated in AGENTS.md, CONTEXT.md, CONTRIBUTING.md,
or the ADRs. Read those first; this is the operational layer on top.

## Verification reality

`apps/fieldcad-desktop` has no headless/driven GUI test harness — no Xvfb, no
project run-skill, no input-injection driver (drag simulation, screenshot
capture) for this native wgpu/winit/egui app. For UI or rendering changes,
automated verification tops out at `cargo build`/`test`/`clippy` plus the
`--smoke N` headless graphics check. State that limit plainly and ask the
user to manually exercise the change in the running app rather than
fabricating GUI verification.

## wgpu/winit rules (apps/fieldcad-desktop)

- **Struct field order is GPU drop order.** Declaring fields in "intuitive"
  order (instance, surface, adapter, device, queue, …, pipelines) destroys
  the stack inside-out and segfaults on exit — invisible to every test since
  it only manifests during drop. Working order, top to bottom: scene
  pipelines/buffers/bind groups/textures → depth/other attachments → `queue`
  → `device` → `surface` → `adapter` → `instance`. The window must outlive
  everything referencing it, so declare the renderer *before* the window in
  the owning struct. Tear the graphics stack down in
  `ApplicationHandler::exiting` (event loop still alive), not during process
  teardown, and drain in-flight work first (`device.poll(PollType::Wait)`).
- **Never spin the event loop.** Use `ControlFlow::WaitUntil(deadline)` driven
  by egui's `repaint_delay`, not unconditional `Poll` + `request_redraw` —
  the latter is sustained pressure on the compositor that never idles.
- **`Occluded` means stop asking, not retry.** A minimized/covered window
  will never present; skip GPU work and back off (~200ms) rather than
  retrying at frame rate.
- **Recreate a surface by dropping first.** Hold it as `Option`, drain the
  queue, `take()` and drop the old surface, then create the replacement —
  two live surfaces for one window isn't valid on every backend.
- **Diagnose before blaming the driver.** `journalctl -k -b0 | grep -iE "GPU
  HANG|i915|xe |reset|drm"` — no hang logged means something above the driver
  blocked. An offscreen windowless render path (real pipelines, no surface)
  isolates GPU/shader bugs from presentation bugs; pair with `--exit-after`
  so a windowed repro can't wedge the compositor.

## Working style with this user

- Expect iterative rounds driven by real usage — a screenshot, a specific
  interaction tried — rather than an exhaustive upfront spec. The first fix
  is often a correct but narrower slice of the real problem; a follow-up
  report revealing a related edge case is the normal next step, not a sign
  the earlier fix was wrong. Keep chasing root cause in engine/architecture
  code (read the relevant ADR, follow the runtime) rather than patching at
  the UI layer.
- When investigation turns up a legitimate, scoped follow-up that's out of
  scope for the current task (API redesign, cross-cutting refactor,
  follow-on feature), write it to `docs/tasks/*.md` — matching the existing
  shape (`## Goal` / `## Current limitation` / `## Required behavior` /
  `## Tests and acceptance` / `## Relevant code`) — before moving on. Don't
  just mention it in chat and let it evaporate. Re-verify file:line
  references in older task docs before acting on them; refactors shift them.
- `docs/perf/*.md` performance audits are a standing backlog worked through
  incrementally across sessions, one finding at a time — not a one-shot
  task. Findings go stale as unrelated commits land; re-verify a claim
  against current code before acting on it rather than trusting the report.

## Domain gotcha: simulated time vs. wall-clock windows

Any feature that filters `fieldcad_simulation::BodySample` history by a
"how many seconds back" cutoff (e.g. trajectory trail length) must clamp the
trim to keep at least the 2 most recent samples. `BodySample.time_seconds`
is *simulated* time driven by the session's `TimeStep`, which is commonly
tens of seconds or more per tick for orbital/astronomical scenes — nothing
about the UI's numeric range implies sub-second ticks. A naive
`skip_while(time < cutoff)` can silently collapse to 1 sample and draw
nothing even while the sim is visibly running. Prefer "keep at least N most
recent samples" as a floor under any time-based cutoff on simulated-time
data.
