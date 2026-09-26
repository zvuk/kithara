use num_traits::cast;

use super::SAW_PERIOD;

mod consts {
    /// Half a period: the point a shortest-path step folds around.
    pub(super) const HALF_PERIOD: i32 = PERIOD / 2;
    /// One saw period in the signed units the wrapping arithmetic here needs.
    ///
    /// The generator advances the saw by exactly one i16 unit per frame, so a
    /// phase in units and a phase in frames are the same number; only the type
    /// differs.
    pub(super) const PERIOD: i32 = 65_536;
}

const _: () = assert!(
    SAW_PERIOD == 65_536,
    "one frame advances the saw by one i16 unit, so PERIOD must equal SAW_PERIOD",
);

/// Saw phase of one decoded sample, in `0..SAW_PERIOD`.
///
/// # Panics
///
/// Panics when `sample` is not finite: a phase is defined only for a real
/// amplitude.
#[must_use]
pub fn units(sample: f32) -> usize {
    let scaled = (f64::from(sample) * f64::from(consts::HALF_PERIOD))
        .round()
        .rem_euclid(f64::from(consts::PERIOD));
    let value: i32 = cast(scaled).expect("a phase is defined only for a finite sample");
    let phase = (value + consts::HALF_PERIOD).rem_euclid(consts::PERIOD);
    usize::try_from(phase).expect("invariant: a phase is never negative")
}

/// Shortest signed step from `from` to `to`, folded at half a period.
///
/// The result spans exactly the 16-bit range, so it reaches `f32` without
/// losing precision.
///
/// # Panics
///
/// Panics unless both phases came from [`units`]: a value past `i32::MAX` has
/// no place on the saw.
#[must_use]
pub fn delta(from: usize, to: usize) -> i16 {
    let from = i32::try_from(from).expect("invariant: a phase fits i32");
    let to = i32::try_from(to).expect("invariant: a phase fits i32");
    let folded = (to - from + consts::HALF_PERIOD).rem_euclid(consts::PERIOD) - consts::HALF_PERIOD;
    cast(folded).expect("invariant: a folded step spans exactly the 16-bit range")
}

/// Circular distance between two phases, ignoring direction.
#[must_use]
pub fn distance(a: usize, b: usize) -> usize {
    let apart = a.abs_diff(b);
    apart.min(SAW_PERIOD - apart)
}

#[cfg(all(test, feature = "native-fixtures", not(target_arch = "wasm32")))]
mod tests {
    use kithara_test_utils::kithara;

    use super::{SAW_PERIOD, delta, distance, units};
    use crate::fixtures::phase_endpoints;

    #[kithara::test(native, flash(false))]
    fn units_round_trip_the_i16_sawtooth_range(phase_endpoints: Vec<f32>) {
        assert_eq!(units(phase_endpoints[0]), 0);
        assert_eq!(units(phase_endpoints[1]), 1);
        assert_eq!(units(phase_endpoints[2]), 32_768);
        assert_eq!(units(phase_endpoints[3]), SAW_PERIOD - 1);
    }

    #[kithara::test(native, flash(false))]
    fn delta_takes_the_short_way_across_the_wrap() {
        assert_eq!(delta(0, 1), 1);
        assert_eq!(delta(1, 0), -1);
        assert_eq!(delta(SAW_PERIOD - 1, 0), 1);
        assert_eq!(delta(0, SAW_PERIOD - 1), -1);
    }

    #[kithara::test(native, flash(false))]
    fn distance_ignores_direction_across_the_wrap() {
        assert_eq!(distance(0, SAW_PERIOD - 1), 1);
        assert_eq!(distance(SAW_PERIOD - 1, 0), 1);
        assert_eq!(distance(0, 32_768), 32_768);
    }
}
