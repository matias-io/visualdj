//! Transport sources: anything that can tell Onset which track is on the master deck, where
//! the playhead is and how fast it moves. The `sim` adapter plays a file itself; others read
//! rekordbox.
#![deny(unsafe_code)]
