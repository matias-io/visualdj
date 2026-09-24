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
  onset-render      wgpu scenes, post FX, text, uniforms, hot reload                                  (planned)
  onset-app         output window, egui control window, config, hotkeys                              (planned)
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
