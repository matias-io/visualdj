# Architecture

Onset is a Cargo workspace of small crates with one direction of dependency: everything depends
on `onset-core`, nothing in `onset-core` depends on anything that does I/O.

```
crates/
  onset-core        domain model: tracks, beat grids, phrases, playhead clock, MusicState, director   (v0.0.1)
  onset-rekordbox   master.db (SQLCipher) reader, ANLZ analysis parsing, artwork paths                (v0.0.1)
  onset-transport   TransportSource trait; adapters: sim (v0.0.1), memory + calibrator, osc (planned)
  onset-audio       WASAPI loopback capture, 24-band analysis, onsets, silence detection             (v0.0.1)
  onset-enrich      lyrics (LRCLIB), artwork palette, on-disk cache                                  (planned)
  onset-render      wgpu context, scenes, uniforms, hot reload, text, quads, card, HUD, headless     (v0.0.2)
  onset-app         output window, engine thread, config, hotkeys, settings panel, bench             (v0.0.2)
  onset-cli         developer commands                                                               (v0.0.1)
```

## Data flow

```
rekordbox memory / OSC / sim  --> transport thread (120 Hz) --> TransportSnapshot
master.db + ANLZ files        --> Library (tracks, grids, phrases, cues, artwork)
PC MASTER OUT (loopback)      --> audio thread --> AudioFeatures (bands, rms, onset)
                                        |
                              core: Clock + Structure + Director
                                        |
                                   MusicState (one struct per frame)
                                        |
                       render thread (vsync)  /  enrichment (async)  /  control window
```

Rules that keep it stable:

- The render thread never blocks on disk, network or decoding.
- `MusicState` is assembled once per frame from the latest snapshots.
- The clock extrapolates the playhead between transport reads and snaps on discontinuities,
  so beat phase is smooth at 60 fps even when reads arrive at 60-120 Hz.
- Timing comes from the beat grid; audio features add texture, never timing.

## Threads (v0.0.2)

```
main thread        winit event loop; owns the window, surface, Renderer, settings panel
engine thread      120 Hz: polls the transport, drives Clock/Structure/Director, publishes
                   MusicState through an ArcSwap the render loop reads without locking
simulator thread   cpal output stream decoding the track and reporting an exact playhead
artwork thread     decodes and downsizes album art on request, hands back RGBA
shader watcher     notify callback thread; debounced changes are polled by the render loop
```

Commands flow the other way over a channel: pause, resume, seek, rate, load track.

## Render graph (v0.0.2)

```
FrameUniforms (240 B, std140, mirrored by struct Frame in common.wgsl)
      |
      v
[scene pass]      active FullscreenScene: one fragment shader over a fullscreen triangle
      |           target: the surface, or an offscreen texture at internal_scale < 1.0
      v
[upscale pass]    only with an offscreen target: textured quad, linear filter
      |
      v
[card pass]       scrim gradient quad, artwork quad(s); two textures blend during a crossfade
      |
      v
[text pass]       glyphon: card text and HUD lines prepared together, one atlas, one draw
      |
      v
[overlay pass]    egui settings panel when open
      |
      v
present
```

Every pass after the first loads the previous contents; nothing reads the surface back except
`--screenshot`. Overlays render at full resolution whatever the internal scale.

Shader hot reload: `ShaderWatcher` watches `assets/shaders/`, collapses editor write bursts
(150 ms quiet period) and reports changed stems. The renderer recompiles that scene inside a
validation error scope; on error it keeps the old pipeline and stores the message for the HUD.
A change to `common.wgsl` recompiles every scene against the on-disk prelude.

Headless tests build the same `Renderer` against an offscreen texture and read pixels back,
so scenes, the card, hot reload and the internal-scale upscale are all checked without a
window. `Gpu::new_headless(prefer_software)` asks for the fallback adapter first, which lets
the suite run on machines without a usable GPU.

## Where rekordbox's data lives (Windows)

- `%APPDATA%\Pioneer\rekordbox\master.db`: SQLCipher-encrypted SQLite. Onset decrypts it into
  a plaintext cache with a pure-Rust page decryptor that also replays the write-ahead log.
- `%APPDATA%\Pioneer\rekordbox\share\PIONEER\USBANLZ\...\ANLZ0000.{DAT,EXT,2EX}`: analysis
  files. `.DAT` holds the beat grid and cue list; `.EXT` holds the phrase structure (PSSI),
  extended cues and colour waveforms.
- `%APPDATA%\Pioneer\rekordbox\share\PIONEER\Artwork\...`: artwork JPEGs referenced by
  `djmdContent.ImagePath`.

## Decisions

See `docs/adr/`.
