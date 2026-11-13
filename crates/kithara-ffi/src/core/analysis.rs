use std::num::NonZeroU32;

/// Position of a source frame on its own sample-rate axis, in seconds.
#[must_use]
pub fn seconds_at(frame: u64, rate: NonZeroU32) -> f64 {
    let frame: f64 = num_traits::cast(frame).unwrap_or(f64::MAX);
    frame / f64::from(rate.get())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::seconds_at;

    #[kithara::test]
    fn a_marker_reads_as_its_position_on_the_source_axis() {
        let rate = NonZeroU32::new(44_100).expect("nonzero rate");

        assert!((seconds_at(0, rate) - 0.0).abs() < f64::EPSILON);
        assert!((seconds_at(44_100, rate) - 1.0).abs() < f64::EPSILON);
        assert!((seconds_at(22_050, rate) - 0.5).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn the_axis_is_the_source_rate_the_pass_was_opened_on() {
        let frame = 48_000;

        let at_48k = seconds_at(frame, NonZeroU32::new(48_000).expect("nonzero rate"));
        let at_44k = seconds_at(frame, NonZeroU32::new(44_100).expect("nonzero rate"));

        assert!((at_48k - 1.0).abs() < f64::EPSILON);
        assert!(at_44k > at_48k);
    }
}
