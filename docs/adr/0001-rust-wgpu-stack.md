# ADR 0001: Rust with wgpu, winit and egui

Date: 2026-09-24. Status: accepted.

## Context

Onset needs a borderless fullscreen output window on a chosen monitor, shader-based scenes with
hot reload, a control window, good text (Unicode, CJK), image textures with crossfades, and a
stable 60 fps while rekordbox shares the GPU. Candidates: raw wgpu + winit + egui, Bevy, nannou,
Tauri with a native output window, Godot, web/Electron.

## Decision

Raw `wgpu` + `winit` for the output window and render graph, `egui` for the control window,
`glyphon` (cosmic-text + rustybuzz) for text. Rust throughout.

## Consequences

- Full control over present mode and frame pacing, which matters when another GPU-heavy app
  (rekordbox) runs alongside; smallest binary; no engine tax.
- We write the render graph, post chain, hot-reload watcher and uniform plumbing ourselves.
- wgpu, winit and egui release breaking changes on their own cadences; versions are pinned and
  upgraded deliberately.
- Bevy remains a legitimate alternative if ECS, asset hot reload and multi-window scaffolding
  become worth a heavier binary and slower compiles. A Tauri web UI can replace the egui
  control window later if settings outgrow it; the output window stays native.
