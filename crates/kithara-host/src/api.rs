use core::f32::consts::PI;

pub use kithara_play::{
    SessionBeat, SessionDuckingMode, SessionTransportSnapshot, SlotId, Tempo, TempoError,
    TransportRevision,
};
pub use kithara_sync::{SyncIntent, SyncMode, SyncStatusSnapshot};
use kithara_warp::BeatGridId;

use crate::error::PlayError;

/// One deck's accepted Sync intent and actual executor evidence from the
/// canonical Host-owned group.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct DeckSyncState {
    /// Mode the deck owner accepted.
    pub mode: SyncMode,
    /// Preparation or mapped execution evidence currently held by the owner.
    pub status: SyncStatusSnapshot,
}

/// One canonical Host member's desired linear mix level.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct HostLevel {
    #[field(get, copy)]
    pub(crate) grid_id: BeatGridId,
    #[field(get, copy)]
    pub(crate) level: f32,
}

impl HostLevel {
    #[must_use]
    pub const fn new(grid_id: BeatGridId, level: f32) -> Self {
        Self { grid_id, level }
    }
}

/// Which side of the DJ crossfader a mix input is assigned to. `Bypass` is unity
/// at any position, for an ordinary fader with no crossfade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CrossfaderBus {
    A,
    B,
    Bypass,
}

/// Equal-power crossfader coefficient for `bus` at `position`, which runs `0.0`
/// (fully toward A) to `1.0` (fully toward B). Stateless.
///
/// # Errors
/// Returns [`PlayError::MixPosition`] when `position` is not finite or outside
/// `0.0..=1.0`.
pub fn crossfader_gain(bus: CrossfaderBus, position: f32) -> Result<f32, PlayError> {
    if !position.is_finite() || !(0.0..=1.0).contains(&position) {
        return Err(PlayError::MixPosition { position });
    }
    let gain = match bus {
        CrossfaderBus::Bypass => 1.0,
        CrossfaderBus::A => {
            if position == 0.0 {
                1.0
            } else if position == 1.0 {
                0.0
            } else {
                (position * PI / 2.0).cos()
            }
        }
        CrossfaderBus::B => {
            if position == 0.0 {
                0.0
            } else if position == 1.0 {
                1.0
            } else {
                (position * PI / 2.0).sin()
            }
        }
    };
    Ok(gain)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn a_b_endpoints_are_exact() {
        assert_eq!(crossfader_gain(CrossfaderBus::A, 0.0).unwrap(), 1.0);
        assert_eq!(crossfader_gain(CrossfaderBus::A, 1.0).unwrap(), 0.0);
        assert_eq!(crossfader_gain(CrossfaderBus::B, 0.0).unwrap(), 0.0);
        assert_eq!(crossfader_gain(CrossfaderBus::B, 1.0).unwrap(), 1.0);
    }

    #[kithara::test(native, flash(false))]
    fn center_is_equal_power() {
        let center = 0.5_f32.sqrt();
        assert!((crossfader_gain(CrossfaderBus::A, 0.5).unwrap() - center).abs() < 1e-6);
        assert!((crossfader_gain(CrossfaderBus::B, 0.5).unwrap() - center).abs() < 1e-6);
    }

    #[kithara::test(native, flash(false))]
    fn bypass_is_always_unity() {
        for &position in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            assert_eq!(
                crossfader_gain(CrossfaderBus::Bypass, position).unwrap(),
                1.0
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn invalid_position_is_rejected() {
        assert!(crossfader_gain(CrossfaderBus::A, -0.1).is_err());
        assert!(crossfader_gain(CrossfaderBus::A, 1.1).is_err());
        assert!(crossfader_gain(CrossfaderBus::B, f32::NAN).is_err());
        assert!(crossfader_gain(CrossfaderBus::Bypass, f32::INFINITY).is_err());
    }
}
