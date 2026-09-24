//! Onset domain model: tracks, beat grids, phrases, the playhead clock, the per-frame
//! `MusicState` and the director that turns structure into intensity. No I/O lives here.
#![forbid(unsafe_code)]

pub mod clock;
pub mod director;
pub mod grid;
pub mod phrase;
pub mod structure;
pub mod track;
pub mod transport;
