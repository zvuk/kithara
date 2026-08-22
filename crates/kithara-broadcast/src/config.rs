use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use bon::Builder;
use kithara_platform::time::Duration;

use crate::{BroadcastError, BroadcastResult};

/// Audio, segmentation, retention, and origin settings for a live broadcast.
#[derive(Debug, Clone, Builder)]
#[non_exhaustive]
pub struct BroadcastConfig {
    #[builder(default = BroadcastConfig::SAMPLE_RATE)]
    pub sample_rate: u32,
    #[builder(default = BroadcastConfig::CHANNELS)]
    pub channels: u16,
    #[builder(default = BroadcastConfig::BIT_RATE)]
    pub bit_rate: u64,
    #[builder(default = BroadcastConfig::TARGET)]
    pub segment_target: Duration,
    #[builder(default = BroadcastConfig::WINDOW)]
    pub window: usize,
    #[builder(default = BroadcastConfig::GRACE)]
    pub grace: usize,
    #[builder(default = BroadcastConfig::BIND)]
    pub bind: SocketAddr,
}

impl BroadcastConfig {
    /// AAC-LC bit rate the encoder targets.
    pub const BIT_RATE: u64 = 128_000;

    /// Loopback on an ephemeral port.
    pub const BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

    /// Channel count of the mix.
    pub const CHANNELS: u16 = 2;

    /// Segments kept fetchable past the playlist window.
    pub const GRACE: usize = 3;

    /// Sample rate of the mix.
    pub const SAMPLE_RATE: u32 = 48_000;

    /// Media duration a segment is cut at.
    pub const TARGET: Duration = Duration::from_secs(4);

    /// Segments a client sees in the playlist.
    pub const WINDOW: usize = 6;

    const MILLIS_PER_SECOND: u64 = 1_000;

    const MIN_TARGETS: u64 = 3;

    pub(crate) fn target_ticks(&self) -> BroadcastResult<u64> {
        u64::try_from(self.segment_target.as_millis())
            .ok()
            .and_then(|millis| millis.checked_mul(u64::from(self.sample_rate)))
            .map(|ticks| ticks / Self::MILLIS_PER_SECOND)
            .filter(|ticks| *ticks > 0)
            .ok_or(BroadcastError::InvalidConfig {
                field: "segment_target",
            })
    }

    pub(crate) fn target_seconds(&self) -> BroadcastResult<u64> {
        Ok(self.target_ticks()?.div_ceil(u64::from(self.sample_rate)))
    }

    pub(crate) fn validate(&self) -> BroadcastResult<()> {
        if self.sample_rate == 0 {
            return Err(BroadcastError::InvalidConfig {
                field: "sample_rate",
            });
        }
        if self.channels == 0 {
            return Err(BroadcastError::InvalidConfig { field: "channels" });
        }
        if self.bit_rate == 0 {
            return Err(BroadcastError::InvalidConfig { field: "bit_rate" });
        }
        if self.window == 0 {
            return Err(BroadcastError::InvalidConfig { field: "window" });
        }

        let window = u64::try_from(self.window)
            .map_err(|_| BroadcastError::InvalidConfig { field: "window" })?;
        let span_ts = window
            .checked_mul(self.target_ticks()?)
            .ok_or(BroadcastError::InvalidConfig { field: "window" })?;
        let minimum_ts = Self::MIN_TARGETS * self.target_seconds()? * u64::from(self.sample_rate);
        if span_ts < minimum_ts {
            return Err(BroadcastError::PlaylistTooShort {
                window: self.window,
                span_ts,
                minimum_ts,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::BroadcastConfig;

    #[kithara::test(native, flash(false))]
    fn the_default_configuration_serves_a_long_enough_playlist() {
        BroadcastConfig::builder()
            .build()
            .validate()
            .expect("the defaults hold the RFC 8216 live window");
    }

    #[kithara::test(native, flash(false))]
    fn a_window_shorter_than_three_target_durations_is_rejected() {
        let short = BroadcastConfig::builder()
            .segment_target(Duration::from_millis(500))
            .window(5)
            .build();

        short
            .validate()
            .expect_err("five 500 ms segments span 2.5 s of a 1 s target duration");
        BroadcastConfig::builder()
            .segment_target(Duration::from_millis(500))
            .window(6)
            .build()
            .validate()
            .expect("six of them span exactly three target durations");
    }

    #[kithara::test(native, flash(false))]
    fn the_target_duration_rounds_the_segment_target_up_to_seconds() {
        let config = BroadcastConfig::builder()
            .segment_target(Duration::from_millis(1_500))
            .build();

        assert_eq!(config.target_seconds().expect("seconds"), 2);
        assert_eq!(config.target_ticks().expect("ticks"), 72_000);
    }

    #[kithara::test(native, flash(false))]
    fn zero_audio_is_rejected() {
        assert!(
            BroadcastConfig::builder()
                .sample_rate(0)
                .build()
                .validate()
                .is_err()
        );
        assert!(
            BroadcastConfig::builder()
                .channels(0)
                .build()
                .validate()
                .is_err()
        );
        assert!(
            BroadcastConfig::builder()
                .segment_target(Duration::ZERO)
                .build()
                .validate()
                .is_err()
        );
    }
}
