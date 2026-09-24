# Onset

Real-time visuals for DJs who play on rekordbox. Onset runs on the same laptop as rekordbox 7,
renders to a second display, and knows the structure of the track that is playing: beat grid,
phrases, hot cues. Animation is driven from a beat phase that does not lag, and a build-up can
start before the drop because rekordbox already analysed where the drop is.

It also shows what is playing (title, artists, album, year, key, BPM, artwork), themes the
visuals from the artwork's colours, and displays synced lyrics when they exist.

## Status

Pre-alpha. Nothing renders yet. The current milestone is the data layer and the musical time
engine, exercised through a developer CLI:

- `onset-cli library` lists the rekordbox collection (title, artist, BPM, key, artwork).
- `onset-cli anlz <title>` prints a track's beat grid summary, phrases and cues.
- `onset-cli sim <title>` plays a track through the built-in simulator and prints beat, bar,
  phrase and drop-countdown state live.
- `onset-cli listen` captures the master mix and shows a 24-band meter.

## How it reads rekordbox

Onset does not talk to rekordbox through an API, because rekordbox has none. On the user's own
machine it reads:

- the rekordbox library database (`master.db`) and analysis files (beat grids, phrases, cues,
  waveforms) that rekordbox writes to the user's profile;
- rekordbox's process memory, to learn which deck is master and where the playhead is
  (planned; the pointer offsets are specific to each rekordbox version and are calibrated
  locally);
- the master mix, via rekordbox's PC MASTER OUT setting and a WASAPI loopback capture.

Nothing is written to rekordbox's files or memory.

## Requirements

- Windows 11 x64, rekordbox 7 (tested with 7.2.18), a controller in Performance mode.
- A second display for the output window.
- To build: Rust stable (`rustup`), Visual Studio Build Tools with the C++ workload.

## Build

```bash
cargo build --release
```

## Licence

Not decided yet. Until it is, all rights reserved by the author; dependency licences are
recorded in the repository.
