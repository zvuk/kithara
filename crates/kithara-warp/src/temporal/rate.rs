/// One coherent live rate target loaded from a single atomic word.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RateTarget(u64);

impl RateTarget {
    pub(super) fn pack(speed: f32, revision: u32) -> u64 {
        (u64::from(revision) << u32::BITS) | u64::from(speed.to_bits())
    }

    pub(super) fn revision_from(packed: u64) -> u32 {
        let [a, b, c, d, _, _, _, _] = packed.to_be_bytes();
        u32::from_be_bytes([a, b, c, d])
    }

    pub(super) const fn unpack(packed: u64) -> Self {
        Self(packed)
    }

    pub(crate) fn speed(self) -> f32 {
        let [_, _, _, _, a, b, c, d] = self.0.to_be_bytes();
        f32::from_bits(u32::from_be_bytes([a, b, c, d]))
    }

    pub(crate) fn revision(self) -> u64 {
        u64::from(Self::revision_from(self.0))
    }
}
