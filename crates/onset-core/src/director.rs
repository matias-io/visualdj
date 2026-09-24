//! The director turns structure into an intensity envelope the scenes can lean on: calm in
//! breakdowns, ramping through build-ups, full at the drop, and rising ahead of a drop the
//! phrase analysis says is coming.
use crate::phrase::PhraseKind;
use crate::structure::StructureState;

/// Beats ahead of a high-energy phrase at which the anticipation ramp starts.
pub const ANTICIPATION_BEATS: u32 = 16;
/// Time constant (seconds) when intensity rises. Short enough that a drop lands within one
/// frame at 60 fps.
pub const ATTACK_S: f32 = 0.01;
/// Time constant (seconds) when intensity falls.
pub const RELEASE_S: f32 = 1.5;
/// Everything before a drop stays at or below this, so the drop itself always has a step.
pub const PRE_DROP_CEILING: f32 = 0.8;
/// Maximum lift added while a drop is announced within `ANTICIPATION_BEATS`.
pub const ANTICIPATION_LIFT: f32 = 0.15;

#[derive(Debug, Default)]
pub struct Director {
    value: f32,
    manual: Option<f32>,
}

impl Director {
    pub fn new() -> Self {
        Self {
            value: 0.5,
            manual: None,
        }
    }

    /// Where the intensity should be right now, before smoothing.
    pub fn target_intensity(st: &StructureState) -> f32 {
        let is_drop = st.phrase == Some(PhraseKind::Chorus);
        if is_drop {
            return 1.0;
        }
        let base = match st.phrase {
            Some(PhraseKind::Up) => 0.45 + 0.30 * st.phrase_phase.clamp(0.0, 1.0),
            Some(PhraseKind::Verse) | None => 0.5,
            Some(PhraseKind::Intro) => 0.35,
            Some(PhraseKind::Bridge | PhraseKind::Down | PhraseKind::Outro) => 0.3,
            Some(PhraseKind::Chorus) => 1.0,
        };
        // Anticipation: lift as the announced drop approaches, but never up to the drop's
        // own level, so the drop keeps a visible attack.
        let lift = match st.drop_countdown_beats {
            Some(beats) if beats <= ANTICIPATION_BEATS => {
                ANTICIPATION_LIFT * (1.0 - beats as f32 / ANTICIPATION_BEATS as f32)
            }
            _ => 0.0,
        };
        (base + lift).clamp(0.0, PRE_DROP_CEILING)
    }

    /// Force a value (0..1) from the control panel; `None` returns control to the structure.
    pub fn set_override(&mut self, value: Option<f32>) {
        self.manual = value.map(|v| v.clamp(0.0, 1.0));
    }

    /// Advance by `dt_s` seconds towards the target and return the smoothed intensity.
    pub fn update(&mut self, st: &StructureState, dt_s: f32) -> f32 {
        if let Some(m) = self.manual {
            self.value = m;
            return m;
        }
        let target = Self::target_intensity(st);
        let tau = if target > self.value {
            ATTACK_S
        } else {
            RELEASE_S
        };
        let a = 1.0 - (-dt_s / tau).exp();
        self.value += a * (target - self.value);
        self.value
    }

    pub fn value(&self) -> f32 {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phrase::PhraseKind;
    use crate::structure::StructureState;

    fn st(kind: Option<PhraseKind>, phase: f32, countdown: Option<u32>) -> StructureState {
        StructureState {
            phrase: kind,
            phrase_phase: phase,
            drop_countdown_beats: countdown,
            ..StructureState::default()
        }
    }

    #[test]
    fn chorus_is_full() {
        let t = Director::target_intensity(&st(Some(PhraseKind::Chorus), 0.5, None));
        assert!((t - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn up_ramps_with_phrase_progress() {
        let a = Director::target_intensity(&st(Some(PhraseKind::Up), 0.0, Some(64)));
        let b = Director::target_intensity(&st(Some(PhraseKind::Up), 1.0, Some(1)));
        assert!(a < b, "{a} < {b}");
        assert!(a >= 0.45 && b <= 1.0, "{a} {b}");
    }

    #[test]
    fn countdown_lifts_a_quiet_phrase() {
        let far = Director::target_intensity(&st(Some(PhraseKind::Intro), 0.5, Some(64)));
        let near = Director::target_intensity(&st(Some(PhraseKind::Intro), 0.5, Some(4)));
        assert!(near > far, "{near} > {far}");
    }

    /// The end of a build-up must leave a visible step for the drop itself.
    #[test]
    fn build_up_leaves_headroom_for_the_drop() {
        let mut end_of_up = st(Some(PhraseKind::Up), 1.0, Some(1));
        end_of_up.in_high_energy = true;
        let before = Director::target_intensity(&end_of_up);
        let drop = Director::target_intensity(&st(Some(PhraseKind::Chorus), 0.0, None));
        assert!(before <= PRE_DROP_CEILING + f32::EPSILON, "{before}");
        assert!(drop - before >= 0.2 - 1e-6, "{before} -> {drop}");
    }

    #[test]
    fn unknown_structure_sits_in_the_middle() {
        let t = Director::target_intensity(&st(None, 0.0, None));
        assert!((t - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn attack_is_fast_release_is_slow() {
        let mut d = Director::new();
        for _ in 0..10 {
            d.update(&st(Some(PhraseKind::Intro), 0.5, None), 1.0 / 60.0);
        }
        // One frame of a chorus must carry most of the way to full intensity.
        let after_attack = d.update(&st(Some(PhraseKind::Chorus), 0.0, None), 1.0 / 60.0);
        assert!(after_attack > 0.8, "{after_attack}");
        // One frame of an outro must barely move it.
        let after_release = d.update(&st(Some(PhraseKind::Outro), 0.0, None), 1.0 / 60.0);
        assert!(
            after_attack - after_release < 0.02,
            "release must be slow: {after_attack} -> {after_release}"
        );
    }

    #[test]
    fn manual_override_wins() {
        let mut d = Director::new();
        d.set_override(Some(0.2));
        let v = d.update(&st(Some(PhraseKind::Chorus), 0.0, None), 1.0);
        assert!((v - 0.2).abs() < f32::EPSILON);
        d.set_override(None);
        assert!(d.update(&st(Some(PhraseKind::Chorus), 0.0, None), 1.0) > 0.9);
    }
}
