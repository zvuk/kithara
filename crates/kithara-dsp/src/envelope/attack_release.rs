use crate::backend;

/// Immediate attack to a smaller target, exponential release toward unity.
/// The fused release runs before the clamp, so recovery cannot exceed a target.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AttackRelease {
    gain: f32,
    release: f32,
}

impl AttackRelease {
    /// Start at unity with the caller's per-sample release coefficient.
    #[must_use]
    pub fn new(release: f32) -> Self {
        Self { gain: 1.0, release }
    }

    /// Replace each target by its gain, in order. Returns the slice length.
    /// Coefficients and targets are not sanitized or clamped to a domain.
    pub fn process(&mut self, targets: &mut [f32]) -> usize {
        backend::attack_release(self.release, &mut self.gain, targets)
    }

    /// Forget the gain reduction, preserving the release coefficient.
    pub fn reset(&mut self) {
        self.gain = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn complete_gain_state_matches_release_before_clamp_at_every_partition() {
        for release in [0.0, 0.9, 0.999_583_4, 1.0] {
            let mut envelope = AttackRelease::new(release);
            let mut gain = 1.0_f32;
            for count in [0, 1, 7, 255, 256, 257, 600] {
                let mut targets: Vec<f32> = (0..count)
                    .map(|index| *[1.0, 0.25, 0.8, 1.0, 0.99, 0.0].get(index % 6).unwrap())
                    .collect();
                let mut expected = targets.clone();
                for target in &mut expected {
                    gain = (1.0 - gain).mul_add(-release, 1.0);
                    if *target < gain {
                        gain = *target;
                    }
                    *target = gain;
                }
                assert_eq!(envelope.process(&mut targets), count);
                assert_eq!(
                    targets
                        .iter()
                        .map(|value| value.to_bits())
                        .collect::<Vec<_>>(),
                    expected
                        .iter()
                        .map(|value| value.to_bits())
                        .collect::<Vec<_>>()
                );
                assert_eq!(envelope.gain.to_bits(), gain.to_bits());
            }
            envelope.reset();
            assert_eq!(envelope.gain.to_bits(), 1.0_f32.to_bits());
            assert_eq!(envelope.release.to_bits(), release.to_bits());
        }
        let mut envelope = AttackRelease::new(0.9);
        let mut target = [0.25];
        envelope.process(&mut target);
        assert_eq!(target, [0.25]);
        assert_ne!(
            target[0].to_bits(),
            (1.0_f32 - 0.25).mul_add(-0.9, 1.0).to_bits()
        );
    }
}
