//! The one struct the renderer reads every frame. Everything time-critical is derived from
//! the clock and beat grid; audio features add texture; the director sets intensity.
use crate::audio_features::AudioFeatures;
use crate::phrase::{Mood, PhraseKind};
use crate::structure::StructureState;
use crate::track::TrackMeta;

/// Neutral theme used before artwork has been analysed: background, text, three accents.
pub const DEFAULT_THEME: [[f32; 3]; 5] = [
    [0.05, 0.05, 0.07],
    [0.90, 0.90, 0.95],
    [0.20, 0.60, 1.00],
    [1.00, 0.40, 0.20],
    [0.60, 0.20, 0.90],
];

/// The next cue ahead of the playhead, as the show director needs it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CueAhead {
    /// 0 = memory cue, 1..=8 = hot cue A..H.
    pub slot: u8,
    pub seconds: f32,
    pub color: Option<[u8; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MusicState {
    /// Wall-clock seconds since the app started.
    pub time_s: f64,
    pub playhead_s: f64,
    pub playing: bool,
    /// Tempo being played: grid tempo × playback rate (follows the pitch fader).
    pub bpm: f32,
    /// The analysed tempo at the playhead.
    pub bpm_grid: f32,
    pub beat_phase: f32,
    pub bar_phase: f32,
    pub phrase_phase: f32,
    pub phrase: Option<PhraseKind>,
    pub next_phrase: Option<PhraseKind>,
    pub beats_to_next_phrase: Option<u32>,
    pub drop_countdown_beats: Option<u32>,
    /// Director output, 0..1.
    pub intensity: f32,
    pub audio: AudioFeatures,
    pub track: Option<TrackMeta>,
    /// Beat number in the grid (0-based), for sequencing; `None` before the first beat.
    pub beat_index: Option<u32>,
    pub mood: Option<Mood>,
    pub next_cue: Option<CueAhead>,
    /// Background, text, accent 1..3 as linear RGB.
    pub theme: [[f32; 3]; 5],
}

impl Default for MusicState {
    fn default() -> Self {
        Self {
            time_s: 0.0,
            playhead_s: 0.0,
            playing: false,
            bpm: 0.0,
            bpm_grid: 0.0,
            beat_phase: 0.0,
            bar_phase: 0.0,
            phrase_phase: 0.0,
            phrase: None,
            next_phrase: None,
            beats_to_next_phrase: None,
            drop_countdown_beats: None,
            intensity: 0.5,
            audio: AudioFeatures::silent(),
            track: None,
            beat_index: None,
            mood: None,
            next_cue: None,
            theme: DEFAULT_THEME,
        }
    }
}

impl MusicState {
    /// `rate` is the playback rate from the clock (`bpm_now / bpm_original`).
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        time_s: f64,
        playhead_s: f64,
        playing: bool,
        rate: f64,
        st: &StructureState,
        audio: AudioFeatures,
        intensity: f32,
        track: Option<TrackMeta>,
    ) -> Self {
        Self {
            time_s,
            playhead_s,
            playing,
            bpm: st.bpm * rate as f32,
            bpm_grid: st.bpm,
            beat_phase: st.beat_phase,
            bar_phase: st.bar_phase,
            phrase_phase: st.phrase_phase,
            phrase: st.phrase,
            next_phrase: st.next_phrase,
            beats_to_next_phrase: st.beats_to_next_phrase,
            drop_countdown_beats: st.drop_countdown_beats,
            intensity,
            audio,
            track,
            beat_index: st.beat_index,
            mood: st.mood,
            next_cue: st.next_cue.as_ref().map(|(c, s)| CueAhead {
                slot: c.slot,
                seconds: *s,
                color: c.color,
            }),
            theme: DEFAULT_THEME,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_features::AudioFeatures;
    use crate::phrase::PhraseKind;
    use crate::structure::StructureState;

    #[test]
    fn assemble_copies_structure_and_inputs() {
        let st = StructureState {
            beat_index: Some(20),
            beat_phase: 0.25,
            bar_phase: 0.0625,
            bpm: 120.0,
            phrase: Some(PhraseKind::Up),
            phrase_label: Some("Up 1".into()),
            phrase_phase: 0.5,
            next_phrase: Some(PhraseKind::Chorus),
            beats_to_next_phrase: Some(8),
            in_high_energy: true,
            drop_countdown_beats: Some(8),
            next_cue: None,
            mood: None,
        };
        let mut audio = AudioFeatures::silent();
        audio.rms = 0.3;
        audio.silent = false;

        let ms = MusicState::assemble(12.5, 10.0, true, 1.05, &st, audio, 0.7, None);

        assert!((ms.time_s - 12.5).abs() < f64::EPSILON);
        assert!((ms.playhead_s - 10.0).abs() < f64::EPSILON);
        assert!(ms.playing);
        // Displayed tempo follows the pitch fader; the grid tempo stays available.
        assert!((ms.bpm - 126.0).abs() < 1e-3, "{}", ms.bpm);
        assert!((ms.bpm_grid - 120.0).abs() < f32::EPSILON);
        assert!((ms.beat_phase - 0.25).abs() < f32::EPSILON);
        assert_eq!(ms.phrase, Some(PhraseKind::Up));
        assert_eq!(ms.next_phrase, Some(PhraseKind::Chorus));
        assert_eq!(ms.drop_countdown_beats, Some(8));
        assert!((ms.intensity - 0.7).abs() < f32::EPSILON);
        assert!((ms.audio.rms - 0.3).abs() < f32::EPSILON);
        assert!(ms.track.is_none());
    }

    #[test]
    fn default_theme_has_five_distinct_colours_with_readable_text() {
        let ms = MusicState::default();
        assert_eq!(ms.theme.len(), 5);
        let bg = ms.theme[0];
        let fg = ms.theme[1];
        let lum = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        assert!(
            lum(fg) - lum(bg) > 0.5,
            "text must contrast with background"
        );
    }

    #[test]
    fn idle_state_is_silent_and_stopped() {
        let ms = MusicState::default();
        assert!(!ms.playing);
        assert!(ms.audio.silent);
        assert_eq!(ms.phrase, None);
        assert!((ms.intensity - 0.5).abs() < f32::EPSILON);
    }
}
