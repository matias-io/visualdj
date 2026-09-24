# ADR 0002: How Onset learns rekordbox's live state

Date: 2026-09-24. Status: accepted.

## Context

rekordbox has no API, plugin system or IPC. Options surveyed: process memory (pointer chains,
version-specific), rekordbox's Ableton Link (deck follows Link, not the reverse; disables the
tempo fader with a controller attached), MIDI LEARN output to a virtual port (LED events only),
Pro DJ Link (not emitted in Performance mode; ports bound exclusively by rekordbox), History
database (written about a minute late), FLX-10 jog-display HID (not reverse engineered), DMX
(needs hardware), audio analysis (identity via fingerprinting, playhead via envelope alignment).

## Decision

1. Live state comes through a `TransportSource` trait so sources can be swapped and fused.
2. The primary source is a memory reader using the pointer-chain model proven by rkbx_link
   (master deck index; per deck BPM, sample position, track-info text, ANLZ path). Beats, bars
   and phrases are computed from the ANLZ files, not read from memory.
3. Offsets are derived locally by a calibrator (known chain tails first, then value and pointer
   scanning) and stored per rekordbox version. Onset does not depend on a third-party offset
   feed.
4. An OSC adapter accepts rkbx_link's output for users who already run it.
5. A built-in simulator source plays a file with an exact playhead for development and demos.
6. Audio alignment against rekordbox's waveform data is the planned version-proof fallback.

## Consequences

- Every rekordbox point release may require recalibration; the calibrator makes that a
  minutes-long, user-runnable step and its results can be shared.
- Reading another program's memory sits in the same grey area as every existing rekordbox
  tool; the README states plainly what is read and that nothing is written.
- The renderer and structure engine are testable without rekordbox at all.
