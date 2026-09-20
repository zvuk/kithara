/// One coherent live rate target loaded from a single atomic word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateTarget(u64);

impl RateTarget {
    pub(super) fn pack(speed: f32, revision: u32) -> u64 {
        (u64::from(revision) << u32::BITS) | u64::from(speed.to_bits())
    }

    #[must_use]
    pub fn revision(self) -> u64 {
        u64::from(Self::revision_from(self.0))
    }

    pub(super) fn revision_from(packed: u64) -> u32 {
        let [a, b, c, d, _, _, _, _] = packed.to_be_bytes();
        u32::from_be_bytes([a, b, c, d])
    }

    #[must_use]
    pub fn speed(self) -> f32 {
        let [_, _, _, _, a, b, c, d] = self.0.to_be_bytes();
        f32::from_bits(u32::from_be_bytes([a, b, c, d]))
    }

    pub(super) const fn unpack(packed: u64) -> Self {
        Self(packed)
    }
}

impl Default for RateTarget {
    fn default() -> Self {
        Self(Self::pack(1.0, 0))
    }
}

impl RateTarget {
    /// Keeps the request identity while replacing its multiplier with a smoothed value.
    #[must_use]
    pub fn with_speed(self, speed: f32) -> Self {
        Self(Self::pack(speed, Self::revision_from(self.0)))
    }

    pub(super) const fn packed(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::RateTarget;

    #[kithara::test]
    fn a_smoothed_target_keeps_the_identity_of_its_request() {
        let target = RateTarget::default().with_speed(1.25);
        let smoothed = target.with_speed(1.1);

        assert_eq!(smoothed.revision(), target.revision());
        assert!((smoothed.speed() - 1.1).abs() < f32::EPSILON);
    }
}
