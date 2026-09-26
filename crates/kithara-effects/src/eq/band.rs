use bon::Builder;
use num_traits::cast::AsPrimitive;

use crate::{GainDb, consts};

/// The type of biquad filter used for an EQ band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
#[non_exhaustive]
pub enum FilterKind {
    LowShelf,
    #[default]
    Peaking,
    HighShelf,
}

impl From<u8> for FilterKind {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::LowShelf,
            consts::HIGH_SHELF_DISCRIMINANT => Self::HighShelf,
            _ => Self::Peaking,
        }
    }
}

/// Configuration for a single EQ band.
#[derive(Debug, Clone, Copy, PartialEq, Builder, fieldwork::Fieldwork)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
#[fieldwork(get)]
#[derive(kithara_derive::BuiltDefault)]
pub struct EqBandConfig {
    #[builder(default)]
    #[field(get(copy))]
    kind: FilterKind,
    #[builder(default)]
    #[field(get(copy))]
    gain_db: GainDb,
    #[builder(default = consts::DEFAULT_FREQ)]
    frequency: f32,
    #[builder(default = std::f32::consts::FRAC_1_SQRT_2)]
    q_factor: f32,
}

impl EqBandConfig {
    pub const fn set_gain_db(&mut self, gain_db: GainDb) {
        self.gain_db = gain_db;
    }
}

/// Generate logarithmically-spaced EQ bands from 60 Hz to 18 kHz.
#[must_use]
pub fn generate_log_spaced_bands(count: usize) -> Vec<EqBandConfig> {
    if count == 0 {
        return Vec::new();
    }

    let count_f32: f32 = count.as_();
    let q_factor = consts::Q_SCALE_FACTOR * (count_f32 / consts::Q_REFERENCE_BANDS).sqrt();

    if count == 1 {
        return vec![
            EqBandConfig::builder()
                .frequency((consts::BAND_MIN_FREQ * consts::BAND_MAX_FREQ).sqrt())
                .q_factor(q_factor)
                .build(),
        ];
    }

    let log_min = consts::BAND_MIN_FREQ.log10();
    let last_count_f32: f32 = (count - 1).as_();
    let log_step = (consts::BAND_MAX_FREQ.log10() - log_min) / last_count_f32;
    let last = count - 1;

    (0..count)
        .map(|index| {
            let kind = if index == 0 {
                FilterKind::LowShelf
            } else if index == last {
                FilterKind::HighShelf
            } else {
                FilterKind::Peaking
            };
            let index_f32: f32 = index.as_();
            EqBandConfig::builder()
                .kind(kind)
                .frequency(consts::LOG_FREQ_BASE.powf(index_f32.mul_add(log_step, log_min)))
                .q_factor(q_factor)
                .build()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(0, 0)]
    #[case(1, 1)]
    #[case(10, 10)]
    fn generates_log_spaced_bands(#[case] count: usize, #[case] expected_len: usize) {
        let bands = generate_log_spaced_bands(count);
        assert_eq!(bands.len(), expected_len);
        match count {
            0 => assert!(bands.is_empty()),
            1 => {
                assert!(
                    (bands[0].frequency() - (consts::BAND_MIN_FREQ * consts::BAND_MAX_FREQ).sqrt())
                        .abs()
                        < 1.0
                );
            }
            10 => {
                assert!((bands[0].frequency() - consts::BAND_MIN_FREQ).abs() < 1.0);
                assert!((bands[9].frequency() - consts::BAND_MAX_FREQ).abs() < 1.0);
                assert!(
                    bands
                        .windows(2)
                        .all(|pair| pair[1].frequency() > pair[0].frequency())
                );
                assert!(
                    bands
                        .iter()
                        .all(|band| f32::from(band.gain_db()).abs() < f32::EPSILON)
                );
            }
            _ => {}
        }
    }

    #[kithara::test]
    fn generated_band_kinds_match_positions() {
        let bands = generate_log_spaced_bands(10);
        assert_eq!(bands[0].kind(), FilterKind::LowShelf);
        assert_eq!(bands[9].kind(), FilterKind::HighShelf);
        assert!(
            bands[1..9]
                .iter()
                .all(|band| band.kind() == FilterKind::Peaking)
        );
    }

    #[kithara::test]
    fn filter_kind_default_is_peaking() {
        assert_eq!(FilterKind::default(), FilterKind::Peaking);
        assert_eq!(EqBandConfig::default().kind(), FilterKind::Peaking);
    }

    #[kithara::test]
    fn three_band_crossovers_match_layout() {
        let bands = generate_log_spaced_bands(3);
        let low = (bands[0].frequency() * bands[1].frequency()).sqrt();
        let high = (bands[1].frequency() * bands[2].frequency()).sqrt();
        assert!((200.0..350.0).contains(&low));
        assert!((2000.0..6000.0).contains(&high));
    }

    #[kithara::test]
    fn a_band_cannot_hold_a_gain_outside_the_range() {
        let mut band = EqBandConfig::default();
        band.set_gain_db(GainDb::from(100.0));
        assert_eq!(band.gain_db(), GainDb::MAX);
        band.set_gain_db(GainDb::from(-100.0));
        assert_eq!(band.gain_db(), GainDb::MIN);
    }
}
