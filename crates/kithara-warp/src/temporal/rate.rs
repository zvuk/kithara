use num_traits::ToPrimitive;

use crate::{ActiveRegion, RenderContext, SyncMode};

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

impl RenderContext {
    /// Media seconds consumed per output second. Beat modes ignore the manual multiplier.
    /// Missing asset geometry or a stationary beat span keeps original tempo.
    #[must_use]
    pub fn rate_for(&self, region: ActiveRegion) -> f64 {
        if self.mode() == SyncMode::Off {
            return f64::from(self.rate().speed());
        }
        let (Some(beats), Some(asset_bps)) = (self.session_beats(), region.beats_per_second())
        else {
            return 1.0;
        };
        let Some(frames) = i64::from(self.output_frames().end)
            .checked_sub(i64::from(self.output_frames().start))
            .filter(|frames| *frames > 0)
            .and_then(|frames| frames.to_f64())
        else {
            return 1.0;
        };
        let seconds = frames / f64::from(self.sample_rate().get());
        let deck_bps = (f64::from(beats.end) - f64::from(beats.start)) / seconds;
        if deck_bps <= 0.0 {
            return 1.0;
        }
        deck_bps / asset_bps
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::RateTarget;
    use crate::{
        ActiveRegion, RenderContext, SessionBeat, SessionEpoch, SessionFrame, SyncMode,
        TransportRevision,
    };

    #[kithara::test]
    #[case::off(SyncMode::Off, Some(2.0), 2.0, 1.25)]
    #[case::host(SyncMode::HostSync, Some(124.0 / 60.0), 2.0, 120.0 / 124.0)]
    #[case::local(SyncMode::LocalSync, Some(2.0), 1.5, 0.75)]
    #[case::unavailable(SyncMode::LocalSync, None, 2.0, 1.0)]
    #[case::paused(SyncMode::HostSync, Some(2.0), 0.0, 1.0)]
    fn rate_is_derived_once_from_its_owner(
        #[case] mode: SyncMode,
        #[case] asset_bps: Option<f64>,
        #[case] deck_bps: f64,
        #[case] expected: f64,
    ) {
        let context = RenderContext::new(
            SessionFrame::new(0)..SessionFrame::new(48_000),
            NonZeroU32::new(48_000).expect("sample rate"),
            Some(SessionBeat::default()..SessionBeat::new(deck_bps).expect("beat")),
            SessionEpoch::new(0),
            Some(TransportRevision::first()),
        )
        .expect("context")
        .with_rate(mode, RateTarget::default().with_speed(1.25));
        let actual = context.rate_for(ActiveRegion::new(0, u64::MAX, asset_bps));
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }
}
