//! The per-frame uniform block every scene reads. Must stay field-for-field in step with
//! `struct Frame` in `assets/shaders/common.wgsl`.
use onset_core::audio_features::BANDS;
use onset_core::music_state::MusicState;
use onset_core::phrase::PhraseKind;

/// std140 layout: scalars are f32 so packing is predictable; arrays are vec4 groups.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FrameUniforms {
    pub resolution: [f32; 2],
    pub time: f32,
    pub playhead: f32,

    pub beat_phase: f32,
    pub bar_phase: f32,
    pub phrase_phase: f32,
    pub intensity: f32,

    pub bpm: f32,
    pub playing: f32,
    /// 0 none, 1 intro, 2 verse, 3 bridge, 4 chorus, 5 outro, 6 up, 7 down.
    pub phrase_kind: f32,
    pub next_phrase_kind: f32,

    /// Beats until the next phrase; -1 when unknown.
    pub beats_to_next: f32,
    /// Beats until the next drop; -1 when unknown.
    pub drop_countdown: f32,
    pub rms: f32,
    pub onset: f32,

    /// 24 bands packed four per vec4.
    pub bands: [[f32; 4]; 6],
    /// Background, text, accent 1..3 as linear RGB (alpha unused).
    pub theme: [[f32; 4]; 5],
}

const _: () = assert!(std::mem::size_of::<FrameUniforms>().is_multiple_of(16));
const _: () = assert!(BANDS == 24);

pub fn phrase_code(kind: Option<PhraseKind>) -> f32 {
    match kind {
        None => 0.0,
        Some(PhraseKind::Intro) => 1.0,
        Some(PhraseKind::Verse) => 2.0,
        Some(PhraseKind::Bridge) => 3.0,
        Some(PhraseKind::Chorus) => 4.0,
        Some(PhraseKind::Outro) => 5.0,
        Some(PhraseKind::Up) => 6.0,
        Some(PhraseKind::Down) => 7.0,
    }
}

fn optional_count(v: Option<u32>) -> f32 {
    v.map_or(-1.0, |n| n as f32)
}

impl FrameUniforms {
    pub fn from_state(ms: &MusicState, resolution: (u32, u32), time_s: f32) -> Self {
        let mut bands = [[0.0f32; 4]; 6];
        for (i, b) in ms.audio.bands.iter().enumerate() {
            bands[i / 4][i % 4] = *b;
        }
        let mut theme = [[0.0f32; 4]; 5];
        for (dst, src) in theme.iter_mut().zip(ms.theme.iter()) {
            *dst = [src[0], src[1], src[2], 1.0];
        }
        Self {
            resolution: [resolution.0 as f32, resolution.1 as f32],
            time: time_s,
            playhead: ms.playhead_s as f32,
            beat_phase: ms.beat_phase,
            bar_phase: ms.bar_phase,
            phrase_phase: ms.phrase_phase,
            intensity: ms.intensity,
            bpm: ms.bpm,
            playing: if ms.playing { 1.0 } else { 0.0 },
            phrase_kind: phrase_code(ms.phrase),
            next_phrase_kind: phrase_code(ms.next_phrase),
            beats_to_next: optional_count(ms.beats_to_next_phrase),
            drop_countdown: optional_count(ms.drop_countdown_beats),
            rms: ms.audio.rms,
            onset: if ms.audio.onset { 1.0 } else { 0.0 },
            bands,
            theme,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onset_core::audio_features::AudioFeatures;
    use onset_core::music_state::MusicState;
    use onset_core::phrase::PhraseKind;

    #[test]
    fn size_is_a_multiple_of_16_bytes() {
        assert!(std::mem::size_of::<FrameUniforms>().is_multiple_of(16));
    }

    #[test]
    fn maps_state_fields() {
        let mut audio = AudioFeatures::silent();
        audio.bands[23] = 0.75;
        audio.rms = 0.4;
        audio.onset = true;
        let ms = MusicState {
            playhead_s: 12.5,
            playing: true,
            bpm: 126.0,
            beat_phase: 0.25,
            bar_phase: 0.5,
            phrase_phase: 0.1,
            phrase: Some(PhraseKind::Chorus),
            next_phrase: Some(PhraseKind::Down),
            beats_to_next_phrase: Some(32),
            drop_countdown_beats: None,
            intensity: 0.9,
            audio,
            ..MusicState::default()
        };
        let u = FrameUniforms::from_state(&ms, (1920, 1080), 3.0);
        assert!((u.resolution[0] - 1920.0).abs() < f32::EPSILON);
        assert!((u.resolution[1] - 1080.0).abs() < f32::EPSILON);
        assert!((u.time - 3.0).abs() < f32::EPSILON);
        assert!((u.playhead - 12.5).abs() < f32::EPSILON);
        assert!((u.playing - 1.0).abs() < f32::EPSILON);
        assert!((u.phrase_kind - 4.0).abs() < f32::EPSILON, "chorus is 4");
        assert!((u.next_phrase_kind - 7.0).abs() < f32::EPSILON, "down is 7");
        assert!((u.beats_to_next - 32.0).abs() < f32::EPSILON);
        assert!(
            (u.drop_countdown + 1.0).abs() < f32::EPSILON,
            "unknown is -1"
        );
        assert!(
            (u.bands[5][3] - 0.75).abs() < f32::EPSILON,
            "band 23 packs into vec4 5, lane 3"
        );
        assert!((u.onset - 1.0).abs() < f32::EPSILON);
        assert!((u.theme[0][0] - ms.theme[0][0]).abs() < f32::EPSILON);
    }

    #[test]
    fn idle_state_has_no_phrase_and_no_countdown() {
        let u = FrameUniforms::from_state(&MusicState::default(), (100, 100), 0.0);
        assert!((u.phrase_kind).abs() < f32::EPSILON);
        assert!((u.beats_to_next + 1.0).abs() < f32::EPSILON);
        assert!((u.playing).abs() < f32::EPSILON);
    }
}
