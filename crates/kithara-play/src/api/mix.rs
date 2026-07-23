use core::f32::consts::PI;

use crate::error::PlayError;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_b_endpoints_are_exact() {
        assert_eq!(crossfader_gain(CrossfaderBus::A, 0.0).unwrap(), 1.0);
        assert_eq!(crossfader_gain(CrossfaderBus::A, 1.0).unwrap(), 0.0);
        assert_eq!(crossfader_gain(CrossfaderBus::B, 0.0).unwrap(), 0.0);
        assert_eq!(crossfader_gain(CrossfaderBus::B, 1.0).unwrap(), 1.0);
    }

    #[test]
    fn center_is_equal_power() {
        let center = 0.5_f32.sqrt();
        assert!((crossfader_gain(CrossfaderBus::A, 0.5).unwrap() - center).abs() < 1e-6);
        assert!((crossfader_gain(CrossfaderBus::B, 0.5).unwrap() - center).abs() < 1e-6);
    }

    #[test]
    fn bypass_is_always_unity() {
        for &position in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            assert_eq!(
                crossfader_gain(CrossfaderBus::Bypass, position).unwrap(),
                1.0
            );
        }
    }

    #[test]
    fn invalid_position_is_rejected() {
        assert!(crossfader_gain(CrossfaderBus::A, -0.1).is_err());
        assert!(crossfader_gain(CrossfaderBus::A, 1.1).is_err());
        assert!(crossfader_gain(CrossfaderBus::B, f32::NAN).is_err());
        assert!(crossfader_gain(CrossfaderBus::Bypass, f32::INFINITY).is_err());
    }
}
