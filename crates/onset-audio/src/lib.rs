//! Captures the DJ master mix (rekordbox PC MASTER OUT via WASAPI loopback) and turns it into
//! per-frame audio features: log-spaced bands, RMS, onsets and a silence flag.
#![deny(unsafe_code)]

pub mod analyzer;
pub mod capture;
