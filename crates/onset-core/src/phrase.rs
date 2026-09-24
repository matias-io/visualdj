//! Phrase structure: rekordbox's song-structure analysis (intro, verse, chorus, ...) mapped to
//! beat ranges, plus the queries the director needs (what is playing, what comes next, how
//! many beats until the next high-energy section).

/// rekordbox analyses each track in one of three "moods", which decide the phrase vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mood {
    Low,
    Mid,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PhraseKind {
    Intro,
    Verse,
    Bridge,
    Chorus,
    Outro,
    /// High-mood only: the build-up before a chorus.
    Up,
    /// High-mood only: the breakdown.
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Phrase {
    pub kind: PhraseKind,
    /// rekordbox's display label, e.g. `Chorus 1`.
    pub label: String,
    /// First beat of the phrase (1-based beat index into the grid, as rekordbox stores it).
    pub start_beat: u32,
    /// One past the last beat.
    pub end_beat: u32,
    /// Beat where a fill starts, when rekordbox marked one.
    pub fill_from_beat: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PhraseMap {
    pub mood: Mood,
    pub phrases: Vec<Phrase>,
    /// Beat at which the analysed structure ends.
    pub end_beat: u32,
}

impl PhraseMap {
    /// Index of the phrase containing `beat`.
    pub fn at_beat(&self, beat: u32) -> Option<usize> {
        self.phrases
            .iter()
            .position(|p| beat >= p.start_beat && beat < p.end_beat)
    }

    /// Index of the first phrase that starts after `beat`.
    pub fn next_after(&self, beat: u32) -> Option<usize> {
        self.phrases.iter().position(|p| p.start_beat > beat)
    }

    /// Chorus is high energy in every mood; in High mood the build-up ("Up") counts too.
    pub fn is_high_energy(&self, kind: PhraseKind) -> bool {
        matches!(kind, PhraseKind::Chorus) || (self.mood == Mood::High && kind == PhraseKind::Up)
    }

    /// Beats from `beat` to the start of the next high-energy phrase beginning after it.
    pub fn beats_until_high_energy(&self, beat: u32) -> Option<u32> {
        self.phrases
            .iter()
            .filter(|p| p.start_beat > beat && self.is_high_energy(p.kind))
            .map(|p| p.start_beat - beat)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> PhraseMap {
        PhraseMap {
            mood: Mood::High,
            end_beat: 128,
            phrases: vec![
                Phrase {
                    kind: PhraseKind::Intro,
                    label: "Intro 1".into(),
                    start_beat: 0,
                    end_beat: 32,
                    fill_from_beat: None,
                },
                Phrase {
                    kind: PhraseKind::Up,
                    label: "Up 1".into(),
                    start_beat: 32,
                    end_beat: 64,
                    fill_from_beat: None,
                },
                Phrase {
                    kind: PhraseKind::Chorus,
                    label: "Chorus 1".into(),
                    start_beat: 64,
                    end_beat: 128,
                    fill_from_beat: None,
                },
            ],
        }
    }

    #[test]
    fn at_beat_finds_phrase() {
        assert_eq!(map().at_beat(40), Some(1));
        assert_eq!(map().at_beat(200), None);
    }

    #[test]
    fn next_after_finds_following_phrase() {
        assert_eq!(map().next_after(40), Some(2));
        assert_eq!(map().next_after(100), None);
    }

    #[test]
    fn beats_until_high_energy_counts_to_up_in_high_mood() {
        assert_eq!(map().beats_until_high_energy(10), Some(22));
    }

    #[test]
    fn inside_high_energy_counts_to_next_one() {
        assert_eq!(map().beats_until_high_energy(40), Some(24));
    }

    #[test]
    fn none_after_last_high_energy() {
        assert_eq!(map().beats_until_high_energy(100), None);
    }

    #[test]
    fn up_is_only_high_energy_in_high_mood() {
        let mut m = map();
        assert!(m.is_high_energy(PhraseKind::Up));
        m.mood = Mood::Mid;
        assert!(!m.is_high_energy(PhraseKind::Up));
        assert!(m.is_high_energy(PhraseKind::Chorus));
    }
}
