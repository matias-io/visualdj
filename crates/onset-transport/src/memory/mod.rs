//! Reading rekordbox's live state from its process memory (Windows only). `process` opens
//! and reads; `chain` describes pointer paths; `offsets` stores them per version; `scan`
//! discovers them; `reader` turns calibrated chains into `TransportSnapshot`s.
//!
//! Nothing here writes to the process.
#[cfg(windows)]
pub mod process;

pub mod chain;
pub mod offsets;
pub mod reader;

#[cfg(windows)]
pub mod scan;

/// The executable this reader targets.
pub const REKORDBOX_EXE: &str = "rekordbox.exe";
