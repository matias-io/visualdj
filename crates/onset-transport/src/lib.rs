//! Transport sources: anything that can tell Onset which track is on the master deck, where
//! the playhead is and how fast it moves. The `sim` adapter plays a file itself; others read
//! rekordbox.
// `memory::process` needs Win32 calls; every other module stays free of unsafe code.
#![deny(unsafe_code)]

pub mod decode;
pub mod memory;
pub mod sim;
pub mod source;
