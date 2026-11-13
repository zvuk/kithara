use std::{fmt, num::NonZeroU32, ops::RangeInclusive};

/// Interpolation in an effect parameter's native unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxParamMapping {
    Linear,
    Log,
}

/// The native unit a deck effect parameter exposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxParamUnit {
    /// A multiplicative ratio relative to unity.
    Ratio,
}

/// An immutable native parameter domain; no mapping means integral steps.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct FxParamDescriptor {
    #[field(get, vis = "pub")]
    range: RangeInclusive<f64>,
    #[field(get(copy), vis = "pub")]
    unit: FxParamUnit,
    #[field(get(copy), vis = "pub")]
    mapping: Option<FxParamMapping>,
}

impl FxParamDescriptor {
    /// Checks finite ordered bounds, positive log bounds and integral stepped bounds.
    #[must_use]
    pub fn checked(
        range: RangeInclusive<f64>,
        unit: FxParamUnit,
        mapping: Option<FxParamMapping>,
    ) -> Option<Self> {
        let (min, max) = (*range.start(), *range.end());
        if !min.is_finite()
            || !max.is_finite()
            || min > max
            || match mapping {
                Some(FxParamMapping::Linear) => false,
                Some(FxParamMapping::Log) => min <= 0.0,
                None => min.fract() != 0.0 || max.fract() != 0.0,
            }
        {
            return None;
        }
        Some(Self {
            range,
            unit,
            mapping,
        })
    }

    /// Whether `value` is finite, in range and integral for a stepped parameter.
    #[must_use]
    pub fn contains(&self, value: f64) -> bool {
        value.is_finite()
            && self.range.contains(&value)
            && (self.mapping.is_some() || value.fract() == 0.0)
    }
}

/// One effect parameter's index and native-unit value, checked by render before application.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxParamChange {
    pub index: usize,
    pub value: f64,
}

/// The effect a deck FX unit holds.
///
/// It arrives prepared off the audio thread with all the memory it uses, and
/// leaves the deck in the receipt of the command that removed it.
pub trait DeckEffect: Send {
    /// Writes the wet signal of the planar stereo `input` into `wet`, both of
    /// one length.
    fn process(&mut self, input: [&[f32]; 2], wet: [&mut [f32]; 2]);

    /// Domains prepared outside RT. They MUST remain unchanged while installed,
    /// across every supported sample-rate retune; storage MUST cover that domain.
    fn parameters(&self) -> &[FxParamDescriptor];

    /// Applies a declared change validated against [`DeckEffect::parameters`]
    /// from the next frame processed, without allocating or freeing.
    fn set(&mut self, change: FxParamChange);

    /// Returns the effect to the state it was prepared in.
    fn reset(&mut self);

    /// Whether the wet signal stays silent while the input does.
    fn tail_exhausted(&self) -> bool;

    /// Retunes all the effect derives from the sample rate to `sample_rate`,
    /// from the next frame processed, without allocating or freeing.
    fn update_sample_rate(&mut self, sample_rate: NonZeroU32);
}

impl fmt::Debug for dyn DeckEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DeckEffect")
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(-1.0, 1.0, Some(FxParamMapping::Linear), true)]
    #[case(0.0, 0.0, Some(FxParamMapping::Linear), true)]
    #[case(1.0, 0.0, Some(FxParamMapping::Linear), false)]
    #[case(f64::NAN, 1.0, Some(FxParamMapping::Linear), false)]
    #[case(0.0, f64::INFINITY, Some(FxParamMapping::Linear), false)]
    #[case(f64::NEG_INFINITY, 0.0, Some(FxParamMapping::Linear), false)]
    #[case(0.0, 1.0, Some(FxParamMapping::Log), false)]
    #[case(0.001, 1.0, Some(FxParamMapping::Log), true)]
    #[case(0.5, 2.0, None, false)]
    #[case(0.0, 2.0, None, true)]
    fn fx_param_descriptor_checks_its_domain(
        #[case] min: f64,
        #[case] max: f64,
        #[case] mapping: Option<FxParamMapping>,
        #[case] valid: bool,
    ) {
        let descriptor = FxParamDescriptor::checked(min..=max, FxParamUnit::Ratio, mapping);
        assert_eq!(descriptor.is_some(), valid);
        if let Some(descriptor) = descriptor {
            assert_eq!(descriptor.range(), &(min..=max));
            assert_eq!(descriptor.mapping(), mapping);
            assert_eq!(descriptor.unit(), FxParamUnit::Ratio);
        }
    }

    #[kithara::test]
    fn fx_param_descriptor_checks_values_and_steps() {
        for mapping in [
            Some(FxParamMapping::Linear),
            Some(FxParamMapping::Log),
            None,
        ] {
            let descriptor =
                FxParamDescriptor::checked(1.0..=3.0, FxParamUnit::Ratio, mapping).unwrap();
            for value in [1.0, 2.0, 3.0] {
                assert!(descriptor.contains(value));
            }
            for value in [0.0, 4.0, f64::NAN, f64::NEG_INFINITY, f64::INFINITY] {
                assert!(!descriptor.contains(value));
            }
            assert_eq!(descriptor.contains(1.5), mapping.is_some());
        }
    }
}
