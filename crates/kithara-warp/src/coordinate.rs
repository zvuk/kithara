use std::{cmp::Ordering, num::NonZeroU32};

use num_traits::cast::ToPrimitive;

use super::{BeatGridStamp, SessionFrame};

/// A value cannot represent a beat-grid coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum MapCoordinateError {
    /// The supplied coordinate is `NaN` or infinite.
    #[error("coordinate must be finite")]
    NonFinite,
    /// An asset coordinate was below the start of the asset.
    #[error("asset frame must not be negative")]
    NegativeAssetFrame,
    /// An uncertainty was below zero.
    #[error("frame uncertainty must not be negative")]
    NegativeUncertainty,
    /// An integral ordinal cannot be represented exactly as a continuous beat.
    #[error("beat ordinal cannot be represented exactly")]
    InexactBeatOrdinal,
    /// A signed session frame cannot be represented exactly as a scalar.
    #[error("session frame cannot be represented exactly")]
    InexactSessionFrame,
}

/// A continuous frame coordinate in decoded asset-native audio.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, derive_more::Into)]
pub struct AssetFrame(f64);

impl AssetFrame {
    pub(crate) const ZERO: Self = Self(0.0);

    /// Creates a finite, non-negative asset-frame coordinate.
    ///
    /// # Errors
    ///
    /// Returns [`MapCoordinateError`] for a non-finite or negative value.
    pub fn new(value: f64) -> Result<Self, MapCoordinateError> {
        if !value.is_finite() {
            return Err(MapCoordinateError::NonFinite);
        }
        if value < 0.0 {
            return Err(MapCoordinateError::NegativeAssetFrame);
        }
        Ok(Self(value))
    }
}

/// A continuous beat coordinate in one beat grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, derive_more::Into)]
pub struct Beat(f64);

impl Beat {
    /// Creates a finite beat coordinate. Negative beats are valid.
    ///
    /// # Errors
    ///
    /// Returns [`MapCoordinateError::NonFinite`] for a non-finite value.
    pub const fn new(value: f64) -> Result<Self, MapCoordinateError> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(MapCoordinateError::NonFinite)
        }
    }
}

/// An exact integral beat identity carried by a sparse marker.
///
/// Ordinal zero is the canonical downbeat for maps using the default meter
/// origin. Pickups therefore use negative ordinals.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    derive_more::Display,
    derive_more::Into,
)]
#[display("{_0}")]
#[into(i64)]
#[repr(transparent)]
pub struct BeatOrdinal(i64);

impl BeatOrdinal {
    /// Creates an exact musical ordinal. Negative ordinals are valid.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }
}

impl TryFrom<BeatOrdinal> for Beat {
    type Error = MapCoordinateError;

    fn try_from(ordinal: BeatOrdinal) -> Result<Self, Self::Error> {
        let value = ordinal
            .0
            .to_f64()
            .ok_or(MapCoordinateError::InexactBeatOrdinal)?;
        if value.to_i64() == Some(ordinal.0) {
            Ok(Self(value))
        } else {
            Err(MapCoordinateError::InexactBeatOrdinal)
        }
    }
}

/// Maximum absolute error measured in the grid's native frame axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, derive_more::Into)]
pub struct FrameUncertainty(f64);

impl FrameUncertainty {
    pub(crate) const ZERO: Self = Self(0.0);

    /// Creates a finite, non-negative uncertainty.
    ///
    /// # Errors
    ///
    /// Returns [`MapCoordinateError`] for a non-finite or negative value.
    pub fn new(value: f64) -> Result<Self, MapCoordinateError> {
        if !value.is_finite() {
            return Err(MapCoordinateError::NonFinite);
        }
        if value < 0.0 {
            return Err(MapCoordinateError::NegativeUncertainty);
        }
        Ok(Self(value))
    }
}

/// Monotonic generation of the live session-frame axis.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    derive_more::Display,
    derive_more::Into,
)]
#[display("{_0}")]
#[into(u64)]
#[repr(transparent)]
pub struct SessionEpoch(u64);

impl SessionEpoch {
    /// Creates a session epoch.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// The stable bounded coordinate axis of an analysed asset.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct AssetAxis {
    /// Returns the sample rate defining asset frames.
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    /// Returns the exclusive decoded-asset frame bound.
    #[field(get, copy)]
    frame_count: u64,
}

impl AssetAxis {
    /// Creates a bounded asset-native coordinate axis.
    #[must_use]
    pub const fn new(sample_rate: NonZeroU32, frame_count: u64) -> Self {
        Self {
            sample_rate,
            frame_count,
        }
    }

    pub(crate) fn contains(self, frame: AssetFrame) -> bool {
        frame
            .0
            .floor()
            .to_u64()
            .is_some_and(|whole_frame| whole_frame < self.frame_count)
    }

    pub(crate) fn contains_or_eof(self, frame: AssetFrame) -> bool {
        if self.contains(frame) {
            return true;
        }
        let frame = f64::from(frame);
        frame.fract() == 0.0 && frame.to_u64() == Some(self.frame_count)
    }
}

/// The signed live coordinate axis of a session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct SessionAxis {
    /// Returns the output sample rate defining session frames.
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    /// Returns the generation of the signed session-frame axis.
    #[field(get, copy)]
    epoch: SessionEpoch,
}

impl SessionAxis {
    /// Creates a signed live session coordinate axis.
    #[must_use]
    pub const fn new(sample_rate: NonZeroU32, epoch: SessionEpoch) -> Self {
        Self { sample_rate, epoch }
    }
}

/// The coordinate axis carried by one grid snapshot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum MapAxis {
    /// A bounded decoded-asset axis.
    Asset(AssetAxis),
    /// A signed live session axis.
    Session(SessionAxis),
}

impl MapAxis {
    pub(crate) const fn kind(self) -> AxisKind {
        match self {
            Self::Asset(_) => AxisKind::Asset,
            Self::Session(_) => AxisKind::Session,
        }
    }

    /// Converts a frame the decoded stream carries at `output_rate` back to
    /// this grid-native axis, the inverse of [`Self::output_frame`].
    ///
    /// Producer positions and frontiers reach the grid on the output axis
    /// while every marker, segment and cue is measured on the native one.
    #[must_use]
    pub fn native_frame(self, output_frame: u64, output_rate: NonZeroU32) -> f64 {
        output_frame.to_f64().unwrap_or(0.0) * f64::from(self.sample_rate().get())
            / f64::from(output_rate.get())
    }
    /// Converts a frame measured on this grid-native axis into the frame the
    /// decoded stream carries once it is resampled to `output_rate`.
    ///
    /// Analysis measures an asset in its own frames while every producer
    /// position, frontier and activation downstream of the decoder counts
    /// output frames, so a coordinate crossing that boundary is scaled here.
    /// A session axis already counts output frames and scales by one.
    #[must_use]
    pub fn output_frame(self, frame: f64, output_rate: NonZeroU32) -> u64 {
        let scaled = frame * f64::from(output_rate.get()) / f64::from(self.sample_rate().get());
        scaled.round().to_u64().unwrap_or(0)
    }

    /// Returns the sample rate defining this grid-native frame axis.
    #[must_use]
    pub const fn sample_rate(self) -> NonZeroU32 {
        match self {
            Self::Asset(axis) => axis.sample_rate,
            Self::Session(axis) => axis.sample_rate,
        }
    }
}

/// A position tagged with its grid-native coordinate axis.
#[derive(Clone, Copy, Debug, PartialEq, derive_more::From)]
#[non_exhaustive]
pub enum MapPosition {
    /// A position in decoded asset-native frames.
    #[from]
    Asset(AssetFrame),
    /// A position in signed session frames.
    #[from]
    Session(SessionFrame),
}

impl MapPosition {
    pub(crate) const fn kind(self) -> AxisKind {
        match self {
            Self::Asset(_) => AxisKind::Asset,
            Self::Session(_) => AxisKind::Session,
        }
    }

    pub(crate) fn on_axis(kind: AxisKind, value: f64) -> Option<Self> {
        match kind {
            AxisKind::Asset => AssetFrame::new(value).ok().map(Self::Asset),
            AxisKind::Session => value
                .round()
                .to_i64()
                .map(SessionFrame::new)
                .map(Self::Session),
        }
    }
}

impl TryFrom<MapPosition> for f64 {
    type Error = MapCoordinateError;

    fn try_from(position: MapPosition) -> Result<Self, Self::Error> {
        match position {
            MapPosition::Asset(frame) => Ok(Self::from(frame)),
            MapPosition::Session(frame) => {
                let integer = i64::from(frame);
                let scalar = integer
                    .to_f64()
                    .ok_or(MapCoordinateError::InexactSessionFrame)?;
                if scalar.to_i64() == Some(integer) {
                    Ok(scalar)
                } else {
                    Err(MapCoordinateError::InexactSessionFrame)
                }
            }
        }
    }
}

impl PartialOrd for MapPosition {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (*self, *other) {
            (Self::Asset(left), Self::Asset(right)) => left.partial_cmp(&right),
            (Self::Session(left), Self::Session(right)) => left.partial_cmp(&right),
            _ => None,
        }
    }
}

/// A coordinate value tied to one exact grid identity and revision.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct MapPoint<T> {
    /// Returns the grid identity and revision carried by this point.
    #[field(get, copy)]
    stamp: BeatGridStamp,
    /// Returns the stamped value.
    #[field(get)]
    value: T,
}

impl<T> MapPoint<T> {
    /// Stamps `value` for use with one immutable grid snapshot.
    #[must_use]
    pub const fn new(stamp: BeatGridStamp, value: T) -> Self {
        Self { stamp, value }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AxisKind {
    Asset,
    Session,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{AssetFrame, Beat, MapCoordinateError};

    #[kithara::test]
    fn musical_coordinates_preserve_the_validated_domain() {
        assert_eq!(Beat::new(f64::NAN), Err(MapCoordinateError::NonFinite));
        assert_eq!(Beat::new(f64::INFINITY), Err(MapCoordinateError::NonFinite));
        assert_eq!(
            AssetFrame::new(f64::NAN),
            Err(MapCoordinateError::NonFinite)
        );
        assert_eq!(
            AssetFrame::new(f64::NEG_INFINITY),
            Err(MapCoordinateError::NonFinite)
        );
        assert_eq!(
            AssetFrame::new(-1.0),
            Err(MapCoordinateError::NegativeAssetFrame)
        );
        assert_eq!(
            f64::from(
                Beat::new(-1.0)
                    .expect("invariant: negative beats are part of the coordinate domain")
            ),
            -1.0
        );
    }
}
