# Onset

Real-time visuals for DJs who play on rekordbox. Onset runs on the same laptop as rekordbox 7,
renders to a second display, and knows the structure of the track that is playing: beat grid,
phrases, hot cues. Animation is driven from a beat phase that does not lag, and a build-up can
start before the drop because rekordbox already analysed where the drop is.

It also shows what is playing (title, artists, album, year, key, BPM, artwork), themes the
visuals from the artwork's colours, and displays synced lyrics when they exist.

## Status

Pre-alpha, milestone 1 (v0.0.1). Nothing renders yet. What works, all verified against a
real rekordbox 7.2.18 library:

- Reads `master.db` (SQLCipher, decrypted to a plaintext cache in about 150 ms, WAL included)
  and loads the collection: title, artists, album, year, key, BPM, duration, file, artwork,
  hot cues and memory cues.
- Parses the analysis files: beat grid, phrase structure (Intro, Up, Chorus, Down, Outro and
  the Low/Mid vocabularies) and cue lists.
- Models musical time: a jitter-filtered playhead clock, beat/bar/phrase phase, beats to the
  next phrase, a drop countdown to the next high-energy phrase, upcoming cues, and a director
  that turns all of that into an intensity envelope with anticipation.
- Plays a track through a built-in simulator with an exact playhead, and captures the live
  mix through WASAPI loopback into a 24-band analyzer with onset and silence detection.

Developer CLI (`cargo run -p onset-cli -- <command>`):

- `library [--grep text]` lists the collection.
- `anlz <title>` prints a track's grid summary, phrases and cues.
- `sim <title> [--seek s] [--analyze]` plays a track and prints beat, bar, phrase, drop
  countdown, next cue and intensity live, with an optional band meter.
- `devices` and `listen [--device name]` list loopback-capable endpoints and meter one.

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
