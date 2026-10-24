use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::AudioSpec;

use crate::{
    error::DecodeError,
    gapless::{GaplessInfo, GaplessTailCompensation},
};

/// Meaning of a decoder's declared duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderDuration {
    /// Raw PCM span length before configured gapless trimming.
    RawLength(Duration),
    /// Logical media endpoint before configured trailing trimming.
    MediaEnd(Duration),
}

/// Decoder-owned per-track playback contract.
///
/// `#[non_exhaustive]` because callers construct it with
/// `..Default::default()` and the decoder may add further track-level facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecoderTrackInfo {
    /// Gapless trim information applied by the engine pipeline.
    pub gapless: Option<GaplessInfo>,
    /// Fused sample-rate-conversion tail compensation.
    pub gapless_tail: Option<GaplessTailCompensation>,
}

/// Immutable decoder facts consumed when constructing a gapless trimmer.
///
/// This profile references the existing gapless contracts instead of copying
/// their frame counts into another source of truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GaplessProfile {
    spec: AudioSpec,
    gapless: Option<GaplessInfo>,
    tail_compensation: Option<GaplessTailCompensation>,
    default_priming_frames: u64,
}

impl GaplessProfile {
    #[must_use]
    pub const fn new(
        spec: AudioSpec,
        gapless: Option<GaplessInfo>,
        tail_compensation: Option<GaplessTailCompensation>,
        default_priming_frames: u64,
    ) -> Self {
        Self {
            spec,
            gapless,
            tail_compensation,
            default_priming_frames,
        }
    }

    #[must_use]
    pub const fn default_priming_frames(self) -> u64 {
        self.default_priming_frames
    }

    #[must_use]
    pub const fn gapless(self) -> Option<GaplessInfo> {
        self.gapless
    }

    #[must_use]
    pub const fn spec(self) -> AudioSpec {
        self.spec
    }

    #[must_use]
    pub const fn tail_compensation(self) -> Option<GaplessTailCompensation> {
        self.tail_compensation
    }
}

/// Optional audio track metadata.
///
/// Intentionally without `#[non_exhaustive]`: downstream fixtures and
/// processors construct this stable optional-field value with struct literals.
#[derive(Debug, Clone, Default)]
pub struct TrackMetadata {
    /// Album name.
    pub album: Option<String>,
    /// Artist name.
    pub artist: Option<String>,
    /// Album artwork bytes.
    pub artwork: Option<Arc<Vec<u8>>>,
    /// Track title.
    pub title: Option<String>,
}

impl TrackMetadata {
    /// Fill unknown fields from another metadata snapshot, preserving every
    /// field already supplied by this snapshot.
    pub fn fill_missing_from(&mut self, missing: &Self) {
        self.album = self.album.take().or_else(|| missing.album.clone());
        self.artist = self.artist.take().or_else(|| missing.artist.clone());
        self.artwork = self.artwork.take().or_else(|| missing.artwork.clone());
        self.title = self.title.take().or_else(|| missing.title.clone());
    }
}

pub(crate) fn checked_audio_spec(
    channels: u16,
    sample_rate: u32,
    resource: &'static str,
) -> Result<AudioSpec, DecodeError> {
    let sample_rate = std::num::NonZeroU32::new(sample_rate)
        .ok_or(DecodeError::InvalidSampleRate { resource })?;
    Ok(AudioSpec::new(channels, sample_rate))
}

/// Immutable original PCM geometry before a nonzero seek.
///
/// Position follows the decoder's physical output clock. The comparison basis
/// removes only the head gap reflected in that first position; wrappers retain
/// its exact time independently of codec modelling and later PTS gaps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PcmOrigin {
    position: Duration,
    comparison_base: Duration,
}

impl PcmOrigin {
    /// Captures physical position and its original content-comparison basis.
    #[must_use]
    pub const fn new(position: Duration, comparison_base: Duration) -> Self {
        Self {
            position,
            comparison_base,
        }
    }

    #[must_use]
    pub const fn position(self) -> Duration {
        self.position
    }

    #[must_use]
    pub const fn comparison_base(self) -> Duration {
        self.comparison_base
    }
}
