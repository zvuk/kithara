use kithara_derive::Ranged;

/// Linear output ceiling from -60 dBFS to full scale.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 0.001, max = 1.0, default = 0.98)]
pub struct Ceiling(f32);

impl TryFrom<f32> for Ceiling {
    type Error = CeilingError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::checked(value).ok_or(CeilingError { value })
    }
}

/// A requested ceiling lies outside the supported linear output range.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("limiter ceiling must be between {min} and {max}, got {value}",
    min = f32::from(Ceiling::MIN), max = f32::from(Ceiling::MAX))]
#[non_exhaustive]
pub struct CeilingError {
    value: f32,
}

/// Gain recovery time in milliseconds, from 1 ms to 5 seconds.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 1.0, max = 5_000.0, default = 50.0)]
pub struct ReleaseMs(f32);

impl TryFrom<f32> for ReleaseMs {
    type Error = ReleaseMsError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::checked(value).ok_or(ReleaseMsError { value })
    }
}

/// A requested release lies outside the supported recovery time range.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("limiter release must be between {min} and {max} ms, got {value}",
    min = f32::from(ReleaseMs::MIN), max = f32::from(ReleaseMs::MAX))]
#[non_exhaustive]
pub struct ReleaseMsError {
    value: f32,
}

/// Time the output trails the input so the gain is down before a peak plays, from 0 to 20 ms.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 0.0, max = 20.0, default = 0.0)]
pub struct LookaheadMs(f32);

impl TryFrom<f32> for LookaheadMs {
    type Error = LookaheadMsError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::checked(value).ok_or(LookaheadMsError { value })
    }
}

/// A requested lookahead lies outside the supported delay range.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("limiter lookahead must be between {min} and {max} ms, got {value}",
    min = f32::from(LookaheadMs::MIN), max = f32::from(LookaheadMs::MAX))]
#[non_exhaustive]
pub struct LookaheadMsError {
    value: f32,
}
