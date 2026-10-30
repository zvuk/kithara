/// Stateless maximum over the intervals supplied by the reconstruction owner.
#[derive(Debug)]
pub struct PeakHold;

impl PeakHold {
    /// Largest supplied peak, starting at positive zero; `NaN` values are ignored.
    #[must_use]
    pub fn peak(intervals: &[f32]) -> f32 {
        intervals.iter().copied().fold(0.0_f32, f32::max)
    }

    /// Link peaks into `output` over the common prefix, leaving its tail alone.
    /// Uses scalar `f32::max` semantics, including `NaN` values and signed zeros.
    pub fn link(peaks: &[f32], output: &mut [f32]) -> usize {
        let len = peaks.len().min(output.len());
        for (output, peak) in output.iter_mut().zip(peaks) {
            *output = output.max(*peak);
        }
        len
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn maximum_keeps_scalar_specials_and_common_prefix() {
        let values = [0.0, -0.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.25];
        for left in values {
            for right in values {
                let mut output = [left, -17.0];
                assert_eq!(PeakHold::link(&[right], &mut output), 1);
                let expected = left.max(right);
                assert!(
                    output[0].to_bits() == expected.to_bits()
                        || (output[0].is_nan() && expected.is_nan())
                );
                assert_eq!(output[1], -17.0);
                assert_eq!(PeakHold::link(&[], &mut output), 0);
            }
        }
        assert_eq!(PeakHold::peak(&[]).to_bits(), 0.0_f32.to_bits());
        assert_eq!(PeakHold::peak(&[0.25, f32::NAN, 2.0, 0.5]), 2.0);
    }
}
