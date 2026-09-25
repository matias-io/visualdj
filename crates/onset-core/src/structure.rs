//! Turns a playhead position into everything the visuals need to know about where we are in
//! the track: beat and bar phase, the current and next phrase, and how many beats remain
//! until the next high-energy section.
use crate::grid::BeatGrid;
use crate::phrase::{PhraseKind, PhraseMap};
use crate::track::HotCue;

/// Everything derivable from the playhead alone, sampled at one instant.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StructureState {
    /// Zero-based index into the beat grid; `None` before the first beat.
    pub beat_index: Option<u32>,
    pub beat_phase: f32,
    pub bar_phase: f32,
    /// Grid tempo at the playhead; 0 when unknown.
    pub bpm: f32,
    pub phrase: Option<PhraseKind>,
    pub phrase_label: Option<String>,
    /// 0..1 progress through the current phrase.
    pub phrase_phase: f32,
    pub next_phrase: Option<PhraseKind>,
    pub beats_to_next_phrase: Option<u32>,
    /// Whether the current phrase is high energy for the track's mood (Chorus, or Up in High
    /// mood). Build-ups count; use `drop_countdown_beats == Some(0..)` for the drop itself.
    pub in_high_energy: bool,
    /// Beats until the next drop (Chorus) starts, when one is ahead.
    pub drop_countdown_beats: Option<u32>,
    /// The next cue ahead of the playhead and the seconds until it.
    pub next_cue: Option<(HotCue, f32)>,
    /// rekordbox's mood analysis for the whole track, when phrases exist.
    pub mood: Option<crate::phrase::Mood>,
}

pub struct Structure<'a> {
    grid: &'a BeatGrid,
    phrases: Option<&'a PhraseMap>,
    cues: &'a [HotCue],
}

impl<'a> Structure<'a> {
    pub fn new(grid: &'a BeatGrid, phrases: Option<&'a PhraseMap>, cues: &'a [HotCue]) -> Self {
        Self {
            grid,
            phrases,
            cues,
        }
    }

    pub fn at(&self, playhead_ms: f64) -> StructureState {
        let mut st = StructureState {
            bpm: self.grid.bpm_at(playhead_ms).unwrap_or(0.0),
            ..StructureState::default()
        };

        if let Some((i, ph)) = self.grid.beat_phase(playhead_ms) {
            st.beat_index = Some(u32::try_from(i).unwrap_or(u32::MAX));
            st.beat_phase = ph;
            st.bar_phase = self.grid.bar_phase(playhead_ms).unwrap_or(0.0);

            if let Some(pm) = self.phrases {
                // PSSI phrase boundaries are 1-based beat numbers.
                let beat1 = u32::try_from(i + 1).unwrap_or(u32::MAX);
                if let Some(pi) = pm.at_beat(beat1) {
                    let p = &pm.phrases[pi];
                    st.phrase = Some(p.kind);
                    st.phrase_label = Some(p.label.clone());
                    let len = p.end_beat.saturating_sub(p.start_beat).max(1) as f32;
                    st.phrase_phase = ((beat1 - p.start_beat) as f32 + ph) / len;
                    st.beats_to_next_phrase = Some(p.end_beat.saturating_sub(beat1));
                    st.next_phrase = pm.phrases.get(pi + 1).map(|n| n.kind);
                    st.in_high_energy = pm.is_high_energy(p.kind);
                }
                st.drop_countdown_beats = pm.beats_until_drop(beat1);
            }
        }
        if let Some(pm) = self.phrases {
            st.mood = Some(pm.mood);
        }

        st.next_cue = self
            .cues
            .iter()
            .filter(|c| f64::from(c.time_ms) > playhead_ms)
            .min_by_key(|c| c.time_ms)
            .map(|c| {
                (
                    c.clone(),
                    ((f64::from(c.time_ms) - playhead_ms) / 1000.0) as f32,
                )
            });

        st
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Beat, BeatGrid};
    use crate::phrase::{Mood, Phrase, PhraseKind, PhraseMap};
    use crate::track::HotCue;

    /// 120 BPM, 256 beats from t=0.
    fn grid() -> BeatGrid {
        BeatGrid::new(
            (0..256u32)
                .map(|i| Beat {
                    time_ms: i * 500,
                    beat_in_bar: u8::try_from(i % 4 + 1).unwrap(),
                    bpm: 120.0,
                })
                .collect(),
        )
    }

    fn phrases() -> PhraseMap {
        let p = |kind, label: &str, start, end| Phrase {
            kind,
            label: label.into(),
            start_beat: start,
            end_beat: end,
            fill_from_beat: None,
        };
        PhraseMap {
            mood: Mood::High,
            end_beat: 257,
            phrases: vec![
                p(PhraseKind::Intro, "Intro 1", 1, 65),
                p(PhraseKind::Up, "Up 1", 65, 129),
                p(PhraseKind::Chorus, "Chorus 1", 129, 257),
            ],
        }
    }

    #[test]
    fn reports_phrase_and_countdown() {
        let g = grid();
        let p = phrases();
        let s = Structure::new(&g, Some(&p), &[]);
        // 10 s => grid index 20 => rekordbox beat 21 (1-based)
        let st = s.at(10_000.0);
        assert_eq!(st.phrase, Some(PhraseKind::Intro));
        assert_eq!(st.phrase_label.as_deref(), Some("Intro 1"));
        assert_eq!(st.beats_to_next_phrase, Some(65 - 21));
        assert_eq!(st.next_phrase, Some(PhraseKind::Up));
        // The drop is the Chorus at beat 129; the Up in between is the build-up.
        assert_eq!(st.drop_countdown_beats, Some(129 - 21));
        assert!(!st.in_high_energy);
        assert!(
            (st.phrase_phase - 20.0 / 64.0).abs() < 0.01,
            "{}",
            st.phrase_phase
        );
    }

    #[test]
    fn build_up_is_high_energy_but_not_the_drop() {
        let g = grid();
        let p = phrases();
        let st = Structure::new(&g, Some(&p), &[]).at(40_000.0); // grid index 80 => beat 81, inside Up
        assert_eq!(st.phrase, Some(PhraseKind::Up));
        assert!(st.in_high_energy);
        assert_eq!(st.drop_countdown_beats, Some(129 - 81));
    }

    #[test]
    fn without_phrases_still_has_beats() {
        let g = grid();
        let s = Structure::new(&g, None, &[]);
        let st = s.at(1250.0);
        assert_eq!(st.beat_index, Some(2));
        assert!((st.beat_phase - 0.5).abs() < 1e-6);
        assert!((st.bpm - 120.0).abs() < f32::EPSILON);
        assert_eq!(st.phrase, None);
        assert_eq!(st.drop_countdown_beats, None);
    }

    #[test]
    fn next_cue_is_the_first_after_playhead() {
        let g = grid();
        let cues = vec![
            HotCue {
                slot: 1,
                time_ms: 10_000,
                name: "IN".into(),
                color: None,
            },
            HotCue {
                slot: 4,
                time_ms: 30_000,
                name: "DROP".into(),
                color: None,
            },
        ];
        let st = Structure::new(&g, None, &cues).at(29_000.0);
        let (c, secs) = st.next_cue.unwrap();
        assert_eq!(c.name, "DROP");
        assert!((secs - 1.0).abs() < 1e-6);
    }

    #[test]
    fn before_the_grid_starts_state_is_empty() {
        let g = BeatGrid::new(vec![Beat {
            time_ms: 5000,
            beat_in_bar: 1,
            bpm: 128.0,
        }]);
        let st = Structure::new(&g, None, &[]).at(100.0);
        assert_eq!(st.beat_index, None);
        assert!(st.bpm.abs() < f32::EPSILON);
    }
}
