use std::num::NonZeroU32;

use kithara_bufpool::SampleBuffer;
use kithara_dsp::param::SmoothedParam;
use kithara_effects::{DeckEffect, FxParamChange};
use num_traits::cast::AsPrimitive;

use super::RenderPass;
use crate::{
    bridge::{DeckFxChange, DeckPart, Returned},
    consts::FADE_CHUNK,
};

/// One FX unit: an optional effect, its dry/wet and its power.
pub(super) struct FxUnit {
    pub(super) effect: Option<Box<dyn DeckEffect>>,
    pub(super) mix: SmoothedParam,
    pub(super) on: bool,
}

impl RenderPass {
    pub(crate) const fn fx_units(&self) -> usize {
        self.units.len()
    }

    /// Whether `unit` holds an effect.
    pub(crate) fn holds_fx(&self, unit: usize) -> bool {
        self.units
            .get(unit)
            .is_some_and(|unit| unit.effect.is_some())
    }

    /// The dry/wet each unit applies, in unit order.
    pub(crate) fn fx_mixes(&self) -> impl Iterator<Item = f64> + '_ {
        self.units
            .iter()
            .map(|unit| f64::from(unit.mix.target_value()))
    }

    /// Check a parameter against the effect after preceding installs and clears.
    pub(crate) fn accepts_fx(
        &self,
        unit: usize,
        change: FxParamChange,
        preceding: &[DeckPart],
    ) -> bool {
        let current = self.units.get(unit).and_then(|unit| unit.effect.as_deref());
        preceding
            .iter()
            .rev()
            .find_map(|part| match part {
                DeckPart::Fx(DeckFxChange::Install {
                    unit: target,
                    effect,
                }) if *target == unit => Some(Some(effect.as_ref())),
                DeckPart::Fx(DeckFxChange::Clear { unit: target }) if *target == unit => Some(None),
                _ => None,
            })
            .unwrap_or(current)
            .and_then(|effect| effect.parameters().get(change.index))
            .is_some_and(|descriptor| descriptor.contains(change.value))
    }

    /// Apply `change` to its unit, tuning an installed effect to the deck's
    /// `sample_rate`, and answer what goes back in the receipt.
    pub(crate) fn change_fx(
        &mut self,
        change: DeckFxChange,
        sample_rate: NonZeroU32,
    ) -> Option<DeckPart> {
        let Some(unit) = self.units.get_mut(change.unit()) else {
            return Some(DeckPart::Fx(change));
        };
        let taken = match change {
            DeckFxChange::Install {
                unit: index,
                mut effect,
            } => {
                effect.update_sample_rate(sample_rate);
                (index, unit.effect.replace(effect))
            }
            DeckFxChange::Clear { unit: index } => (index, unit.effect.take()),
            DeckFxChange::Power { on, .. } => {
                if on
                    && !unit.on
                    && let Some(effect) = &mut unit.effect
                {
                    effect.reset();
                }
                unit.on = on;
                return Some(DeckPart::Fx(change));
            }
            DeckFxChange::Mix { value, .. } => {
                unit.mix.set_value(f32::from(value));
                return Some(DeckPart::Fx(change));
            }
            DeckFxChange::Param { change: param, .. } => {
                if let Some(effect) = &mut unit.effect {
                    effect.set(param);
                }
                return Some(DeckPart::Fx(change));
            }
        };
        let (unit, effect) = taken;
        effect.map(|effect| DeckPart::Returned(Returned::Fx { unit, effect }))
    }
}

impl FxUnit {
    /// On, holding an effect, and not settled fully dry.
    pub(super) fn running(&self) -> bool {
        self.on && self.effect.is_some() && !self.mix.has_settled_at(0.0)
    }

    /// Take the last dry/wet a program prepared, bypassing the ramp.
    pub(super) fn hold(&mut self, programmed: Option<&[f64]>) {
        if let Some(value) = programmed.and_then(<[f64]>::last) {
            self.mix.set_value(value.as_());
            self.mix.reset_to_target();
        }
    }

    /// Run the unit's effect on the frames of a stereo pair and mix its wet
    /// signal in. A program's dry/wet applies on each frame as prepared.
    /// While the manual dry/wet ramps, the effect runs up to the frame the
    /// ramp settles on, whatever block holds it; settled fully dry, the unit
    /// neither runs its effect nor touches the frames. Off or empty, the unit
    /// is not heard, so its dry/wet takes its target at once.
    pub(super) fn process(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        wet: &mut [SampleBuffer; 2],
        programmed: Option<&[f64]>,
    ) {
        let on = self.on;
        let Some(effect) = self.effect.as_mut().filter(|_| on) else {
            self.mix.reset_to_target();
            self.hold(programmed);
            return;
        };
        if let Some(values) = programmed {
            let frames = left.len().min(right.len()).min(values.len());
            let [wet_left, wet_right] = wet;
            let (left, right) = (&mut left[..frames], &mut right[..frames]);
            let (wet_left, wet_right) = (&mut wet_left[..frames], &mut wet_right[..frames]);
            effect.process([&*left, &*right], [&mut *wet_left, &mut *wet_right]);
            let mut mixes = [0.0; FADE_CHUNK];
            let mut keeps = [0.0; FADE_CHUNK];
            for start in (0..frames).step_by(FADE_CHUNK) {
                let end = (start + FADE_CHUNK).min(frames);
                let len = end - start;
                kithara_dsp::narrow_f64(&values[start..end], &mut mixes[..len]);
                for (keep, mix) in keeps.iter_mut().zip(&mixes[..len]) {
                    *keep = 1.0 - *mix;
                }
                for (dry, wet) in [
                    (&mut left[start..end], &wet_left[start..end]),
                    (&mut right[start..end], &wet_right[start..end]),
                ] {
                    kithara_dsp::multiply_in_place(dry, &keeps[..len]);
                    kithara_dsp::mul_add(wet, &mixes[..len], dry);
                }
            }
            self.hold(programmed);
            return;
        }
        if self.mix.has_settled_at(0.0) {
            return;
        }
        let frames = left.len().min(right.len());
        let [wet_left, wet_right] = wet;
        let mut mixes = [0.0; FADE_CHUNK];
        let mut keeps = [0.0; FADE_CHUNK];
        let mut start = 0;
        while start < frames && !self.mix.has_settled() {
            let chunk = FADE_CHUNK.min(frames - start);
            let len = ramp(&mut self.mix, &mut mixes[..chunk], &mut keeps[..chunk]);
            let end = start + len;
            let (left, right) = (&mut left[start..end], &mut right[start..end]);
            let (wet_left, wet_right) = (&mut wet_left[start..end], &mut wet_right[start..end]);
            effect.process([&*left, &*right], [&mut *wet_left, &mut *wet_right]);
            for (dry, wet) in [(left, &*wet_left), (right, &*wet_right)] {
                kithara_dsp::multiply_in_place(dry, &keeps[..len]);
                kithara_dsp::mul_add(wet, &mixes[..len], dry);
            }
            start = end;
        }
        if start == frames || self.mix.has_settled_at(0.0) {
            return;
        }
        let (left, right) = (&mut left[start..frames], &mut right[start..frames]);
        let (wet_left, wet_right) = (&mut wet_left[start..frames], &mut wet_right[start..frames]);
        effect.process([&*left, &*right], [&mut *wet_left, &mut *wet_right]);
        let mix = self.mix.target_value();
        for (dry, wet) in [(left, &*wet_left), (right, &*wet_right)] {
            kithara_dsp::scale_in_place(dry, 1.0 - mix);
            kithara_dsp::scale_add(wet, mix, dry);
        }
    }
}

/// Fill `mixes` with the next values of a ramping `mix` and `keeps` with their
/// complements, up to the frame the ramp settles on, and answer how many
/// frames that is. The ramp settles on the first frame close enough to its
/// target, whatever block holds it.
fn ramp(mix: &mut SmoothedParam, mixes: &mut [f32], keeps: &mut [f32]) -> usize {
    let frames = mixes.len().min(keeps.len());
    mixes
        .iter_mut()
        .zip(keeps)
        .position(|(value, keep)| {
            *value = mix.next_smoothed();
            *keep = 1.0 - *value;
            mix.settle()
        })
        .map_or(frames, |settled| settled + 1)
}
