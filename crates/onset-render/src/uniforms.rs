//! The per-frame uniform block every scene reads. Must stay field-for-field in step with
//! `struct Frame` in `assets/shaders/common.wgsl`.

#[cfg(test)]
mod tests {
    use super::*;
    use onset_core::audio_features::AudioFeatures;
    use onset_core::music_state::MusicState;
    use onset_core::phrase::PhraseKind;

    #[test]
    fn size_is_a_multiple_of_16_bytes() {
        assert_eq!(std::mem::size_of::<FrameUniforms>() % 16, 0);
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
        assert_eq!(u.resolution, [1920.0, 1080.0]);
        assert!((u.time - 3.0).abs() < f32::EPSILON);
        assert!((u.playhead - 12.5).abs() < f32::EPSILON);
        assert!((u.playing - 1.0).abs() < f32::EPSILON);
        assert!((u.phrase_kind - 4.0).abs() < f32::EPSILON, "chorus is 4");
        assert!((u.next_phrase_kind - 7.0).abs() < f32::EPSILON, "down is 7");
        assert!((u.beats_to_next - 32.0).abs() < f32::EPSILON);
        assert!((u.drop_countdown + 1.0).abs() < f32::EPSILON, "unknown is -1");
        assert!((u.bands[5][3] - 0.75).abs() < f32::EPSILON, "band 23 packs into vec4 5, lane 3");
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
