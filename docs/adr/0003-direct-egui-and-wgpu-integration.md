# 0003 — Integrate `egui` and `wgpu` directly rather than through `eframe`

Status: **accepted** (Milestone 0)

## Context

`eframe` is the supported way to build an `egui` application, and it removes real
boilerplate: window creation, surface configuration, the render loop.

It also owns the frame. Field CAD's 3D viewport is not a widget drawn inside a UI
frame; it is a scene pass with its own depth buffer, its own camera, and — from
Milestone 3 — compute dispatches that must be scheduled against the same device
and queue. Making that an `eframe` callback means negotiating with a frame loop
built for a different priority.

We also need adapter selection, fallback-adapter behaviour, and surface-loss
recovery to be ours, because they are user-visible diagnostics in a scientific
tool rather than incidental setup.

## Decision

Own the `winit` event loop and the `wgpu` device, surface, and depth target
directly. Use `egui-winit` for input translation and `egui-wgpu` for painting,
composing the UI as a second render pass over the scene pass.

## Consequences

- We write and maintain surface configuration, resize, and recovery ourselves.
  That is roughly 200 lines, and it is where the adapter diagnostics live.
- Upgrading the `egui`/`wgpu` pair is our problem; the three `egui` crates and
  `wgpu` are pinned together in `[workspace.dependencies]` for that reason.
- The scene pass can add compute, custom depth handling, and multiple viewports
  without asking a framework's permission.
- Input arbitration must be explicit: the UI sees events first and reports
  whether it consumed them, and the viewport only acts on what is left.
