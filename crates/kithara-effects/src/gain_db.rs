use kithara_derive::Ranged;

use crate::consts;

/// Gain of one EQ band, in dB. `0.0` is unity and [`GainDb::MIN`] kills the
/// band. The range is asymmetric on purpose: cutting stays useful far past
/// the point where boosting only clips.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = -24.0, max = 6.0, default = 0.0, clamp)]
pub struct GainDb(f32);

impl GainDb {
    /// The gain a knob sitting at `knob` asks for: `0.0` is [`Self::MIN`],
    /// `0.5` is unity, `1.0` is [`Self::MAX`]. Each half of the travel is
    /// scaled against its own end, so unity stays in the middle of the knob
    /// even though the range around it is not symmetric.
    #[must_use]
    pub fn at_knob(knob: f32) -> Self {
        let offset = knob.clamp(0.0, 1.0) - 0.5;
        Self::from(2.0 * offset * Self::half_span(offset))
    }

    fn half_span(side: f32) -> f32 {
        if side < 0.0 {
            -f32::from(Self::MIN)
        } else {
            f32::from(Self::MAX)
        }
    }

    /// The knob position this gain sits at. Inverse of [`Self::at_knob`].
    #[must_use]
    pub fn knob(self) -> f32 {
        let db = f32::from(self);
        0.5 + db / (2.0 * Self::half_span(db))
    }

    /// This gain as a linear amplitude. The floor of the range means the band
    /// is off, so it maps to exact zero rather than to the very small number
    /// the dB curve gives there.
    pub(crate) fn linear(self) -> f32 {
        if self == Self::MIN {
            return 0.0;
        }
        consts::DB_LOG_BASE.powf(f32::from(self) / consts::DB_DIVISOR)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn the_middle_of_the_knob_is_unity() {
        assert_eq!(GainDb::at_knob(0.5), GainDb::DEFAULT);
    }

    #[kithara::test]
    fn unity_sits_in_the_middle_of_the_knob() {
        assert_eq!(GainDb::default().knob(), 0.5);
    }

    #[kithara::test]
    fn the_bottom_of_the_knob_is_the_floor_of_the_range() {
        assert_eq!(GainDb::at_knob(0.0), GainDb::MIN);
    }

    #[kithara::test]
    fn the_top_of_the_knob_is_the_ceiling_of_the_range() {
        assert_eq!(GainDb::at_knob(1.0), GainDb::MAX);
    }

    /// The two halves are scaled separately, so the mapping is only usable if
    /// it survives a round trip at every position, not just at the three ends.
    #[kithara::test]
    fn a_knob_position_survives_the_round_trip() {
        for step in 0..=20u8 {
            let knob = f32::from(step) / 20.0;
            let round_trip = GainDb::at_knob(knob).knob();
            assert!(
                (round_trip - knob).abs() < 1e-6,
                "knob {knob} came back as {round_trip}"
            );
        }
    }

    #[kithara::test]
    fn a_knob_past_its_travel_is_held_at_the_end() {
        assert_eq!(GainDb::at_knob(2.0), GainDb::MAX);
        assert_eq!(GainDb::at_knob(-1.0), GainDb::MIN);
    }

    #[kithara::test]
    fn the_floor_of_the_range_is_exact_silence() {
        assert_eq!(GainDb::MIN.linear(), 0.0);
    }

    /// A request below the floor is held at the floor, so it kills the band
    /// too rather than landing on the dB curve's value for it.
    #[kithara::test]
    fn a_gain_under_the_floor_is_exact_silence_as_well() {
        assert_eq!(GainDb::from(-30.0).linear(), 0.0);
    }

    #[kithara::test]
    #[case::unity_at_zero(0.0, 1.0, 0.001)]
    #[case::boost_at_6db(6.0, 2.0, 0.02)]
    fn a_gain_in_db_maps_onto_its_amplitude(
        #[case] db: f32,
        #[case] expected: f32,
        #[case] eps: f32,
    ) {
        assert!((GainDb::from(db).linear() - expected).abs() < eps);
    }
}
