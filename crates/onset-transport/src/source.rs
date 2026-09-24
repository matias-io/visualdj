//! The interface every live-state source implements.
use onset_core::transport::TransportSnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceStatus {
    /// Delivering snapshots.
    Connected,
    /// Waiting for its target (rekordbox not running, no OSC yet).
    Searching,
    /// The target exists but this source cannot read it (e.g. no offsets for this version).
    Unsupported(String),
    Error(String),
}

pub trait TransportSource: Send {
    fn name(&self) -> &'static str;
    fn status(&self) -> SourceStatus;
    /// Called at up to 120 Hz. `None` when nothing new can be reported.
    fn poll(&mut self) -> Option<TransportSnapshot>;

    /// The sample rate of the track the engine believes is loaded, for sources whose
    /// position counts in samples of the file (rekordbox). Default: ignored.
    fn set_track_sample_rate(&mut self, _hz: Option<u32>) {}
}
