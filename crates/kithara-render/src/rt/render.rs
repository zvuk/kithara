use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_dsp::param::{SmoothedParam, SmootherConfig};
use kithara_effects::{
    DeckEffect, FxParamChange, GainDb,
    /// The deck's equaliser, after its trim.
    eq::{EqConfig, EqLayout, GainShapeError, StereoEq, generate_log_spaced_bands},
    filter::{FilterEffect, FilterKnob},
};
use num_traits::cast::AsPrimitive;

use super::{command::DeckPrograms, processor::StreamShape};
use crate::{
    bridge::{DeckFxChange, DeckMixSettingsChange, DeckParameter, DeckPart, Returned},
    consts::{FADE_CHUNK, GAIN_SPAN},
};

/// A deck's chain in Traktor's Insert order: trim, equaliser, FX units in
/// series, the filter knob, then fader and crossfader.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct RenderPass {
    /// Values most recently emitted by the control buffers, for the applied snapshot.
    #[field(#[must_use] get(copy), vis = "pub(crate)")]
    control_values: [f64; DeckParameter::CHANNELS],
    /// The deck's trim, fader and crossfader, each ramped to its new target
    /// from the frame it is set on.
    controls: [SmoothedParam; DeckParameter::CHANNELS],
    control_buffers: [SampleBuffer; DeckParameter::CHANNELS],
    eq: StereoEq,
    /// The deck's FX units, after its equaliser.
    units: Box<[FxUnit]>,
    /// The deck filter, after the FX units; transparent at its center.
    filter: FilterEffect,
    /// The wet signal of the unit running now.
    wet: [SampleBuffer; crate::consts::MIN_STEREO],
    /// Mutes fader times crossfader after the units, so it cuts their tails too.
    muted: bool,
    /// Set until the first range renders, which moves every ramp to its target at once.
    priming: bool,
}

/// One FX unit: an optional effect, its dry/wet and its power.
struct FxUnit {
    effect: Option<Box<dyn DeckEffect>>,
    mix: SmoothedParam,
    on: bool,
}

impl RenderPass {
    pub(crate) fn new<S>(
        pools: &PoolRegion<S>,
        shape: StreamShape,
        config: super::DeckMixerConfig,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let eq_config = EqConfig::builder(pools.clone()).build();
        let eq = if config.eq_bands() > 0 {
            let layout = EqLayout::new(
                &eq_config,
                &generate_log_spaced_bands(config.eq_bands()),
                config.sample_rate(),
            )?;
            StereoEq::with_layout(&eq_config, shape.sample_rate, Box::new(layout))
        } else {
            StereoEq::new(&eq_config, shape.sample_rate)
        };
        let capacity = usize::try_from(shape.max_block_frames.get()).unwrap_or(usize::MAX);
        let mix = config.mix();
        let mut filter = FilterEffect::new(shape.sample_rate, config.filter());
        filter.configure(mix.filter());
        filter.reset();
        Ok(Self {
            control_values: [
                f64::from(f32::from(mix.trim())),
                f64::from(f32::from(mix.volume())),
                f64::from(f32::from(mix.crossfader())),
            ],
            controls: [
                smoothed(f32::from(mix.trim()), shape.sample_rate),
                smoothed(f32::from(mix.volume()), shape.sample_rate),
                smoothed(f32::from(mix.crossfader()), shape.sample_rate),
            ],
            eq,
            units: (0..config.fx_units())
                .map(|_| FxUnit {
                    effect: None,
                    mix: smoothed(0.0, shape.sample_rate),
                    on: false,
                })
                .collect(),
            wet: [
                pools.get_with_len::<f32>(capacity)?,
                pools.get_with_len::<f32>(capacity)?,
            ],
            filter,
            control_buffers: [
                pools.get_with_len::<f32>(capacity)?,
                pools.get_with_len::<f32>(capacity)?,
                pools.get_with_len::<f32>(capacity)?,
            ],
            muted: mix.muted(),
            priming: true,
        })
    }

    /// Whether this is the first range the deck renders; every ramp moves to its target.
    pub(crate) fn take_priming(&mut self) -> bool {
        let priming = std::mem::take(&mut self.priming);
        if priming {
            self.idle();
        }
        priming
    }

    pub(in crate::rt) fn finish(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
    ) -> Result<(), GainShapeError> {
        self.run(left, right, None)
    }

    pub(in crate::rt) fn finish_curves(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        programs: &DeckPrograms,
    ) -> Result<(), GainShapeError> {
        self.run(left, right, Some(programs))
    }

    /// Pass a stereo chunk through the chain under the values prepared for this chunk.
    fn run(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        programs: Option<&DeckPrograms>,
    ) -> Result<(), GainShapeError> {
        let frames = left.len().min(right.len());
        let controls = &mut self.control_buffers;
        for (index, (control, output)) in self
            .controls
            .iter_mut()
            .zip(controls.iter_mut())
            .enumerate()
        {
            native_values(
                control,
                programs.and_then(|programs| programs.values(index, frames)),
                &mut output[..frames],
            );
            if let Some(value) = output.get(frames.saturating_sub(1)) {
                self.control_values[index] = f64::from(*value);
            }
        }
        kithara_dsp::multiply_in_place(left, &controls[0][..frames]);
        kithara_dsp::multiply_in_place(right, &controls[0][..frames]);
        let gains = programs.map_or(&[][..], DeckPrograms::eq_values);
        self.eq.process_with_gains(left, right, gains)?;
        for unit in &mut self.units {
            unit.process(left, right, &mut self.wet);
        }
        let planes = &mut [&mut left[..frames], &mut right[..frames]];
        let knobs = programs.and_then(|rows| rows.values(DeckParameter::Filter.index(), frames));
        if let Some(knobs) = knobs {
            self.filter
                .process_program(planes, 0..frames, knobs)
                .expect("prepared stereo knob program");
        } else {
            self.filter
                .process(planes, 0..frames)
                .expect("stereo planes");
        }
        let [output, volume, crossfader] = controls;
        output[..frames].copy_from_slice(&volume[..frames]);
        kithara_dsp::multiply_in_place(&mut output[..frames], &volume[..frames]);
        kithara_dsp::multiply_in_place(&mut output[..frames], &crossfader[..frames]);
        if self.muted {
            output[..frames].fill(0.0);
        }
        kithara_dsp::multiply_in_place(left, &output[..frames]);
        kithara_dsp::multiply_in_place(right, &output[..frames]);
        Ok(())
    }

    pub(in crate::rt) fn advance(&mut self, frames: usize, programs: &DeckPrograms) {
        for (index, control) in self.controls.iter_mut().enumerate() {
            if let Some(value) = programs
                .values(index, frames)
                .and_then(|values| values.last())
            {
                control.set_value(value.as_());
                control.reset_to_target();
            }
        }
        for (band, values) in programs.eq_values().iter().enumerate() {
            if let Some(value) = values.last() {
                self.eq
                    .set_gain_explicit(band, GainDb::from(AsPrimitive::<f32>::as_(*value)));
            }
        }
        let knobs = programs.values(DeckParameter::Filter.index(), frames);
        if let Some(value) = knobs.and_then(<[f64]>::last) {
            if let Some(knob) = FilterKnob::checked(*value) {
                self.filter.configure(knob);
                self.filter.reset();
            }
        } else {
            self.filter.advance(frames);
        }
    }

    pub(crate) fn idle(&mut self) {
        for (control, applied) in self.controls.iter_mut().zip(&mut self.control_values) {
            control.reset_to_target();
            *applied = f64::from(control.target_value());
        }
        for unit in &mut self.units {
            unit.mix.reset_to_target();
        }
    }

    /// Whether a unit still sounds a tail of input the deck no longer plays,
    /// or still ramps its dry/wet, which settles on its own frame only while
    /// the chain renders.
    pub(crate) fn ringing(&self) -> bool {
        !self.filter.tail_exhausted()
            || self.units.iter().any(|unit| {
                unit.running()
                    && (!unit.mix.has_settled()
                        || unit
                            .effect
                            .as_ref()
                            .is_some_and(|effect| !effect.tail_exhausted()))
            })
    }

    pub(crate) fn set_mix(&mut self, change: DeckMixSettingsChange) {
        match change {
            DeckMixSettingsChange::Trim(value) => self.controls[0].set_value(f32::from(value)),
            DeckMixSettingsChange::Volume(value) => self.controls[1].set_value(f32::from(value)),
            DeckMixSettingsChange::Crossfader(value) => {
                self.controls[2].set_value(f32::from(value))
            }
            DeckMixSettingsChange::Muted(value) => self.muted = value,
            DeckMixSettingsChange::Filter(value) => self.filter.configure(value),
        }
    }

    pub(crate) const fn fx_units(&self) -> usize {
        self.units.len()
    }

    /// Whether `unit` holds an effect that takes `change`.
    pub(crate) fn accepts_fx(&self, unit: usize, change: FxParamChange) -> bool {
        self.units
            .get(unit)
            .and_then(|unit| unit.effect.as_ref())
            .is_some_and(|effect| effect.accepts(&change))
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

    delegate::delegate! {
        to self.eq {
            #[call(read_gains)]
            pub(crate) fn read_eq(&self, gains: &mut [GainDb]) -> usize;
            /// Ramp `band` of the deck's equaliser to `gain` from the next frame rendered.
            #[call(set_gain)]
            pub(crate) fn set_eq_gain(&mut self, band: usize, gain: GainDb);
            #[call(accepts_gain_program)]
            pub(crate) fn accepts_eq_program(&self, band: usize) -> bool;
            /// Cross the deck's equaliser over to `layout`, answering the layout it displaced.
            #[call(take_layout)]
            pub(crate) fn take_eq_layout(&mut self, layout: Box<EqLayout>) -> Option<Box<EqLayout>>;
        }
        to self.filter {
            /// Glide the deck filter knob to `knob` from the position it runs at.
            #[call(configure)]
            pub(crate) fn set_filter_knob(&mut self, knob: FilterKnob);
            /// The knob position the deck filter runs at.
            #[call(knob)]
            pub(crate) fn filter_knob(&self) -> f64;
        }
    }

    /// Retune the deck's chain, with every effect its units hold, to
    /// `sample_rate`. A ramp in flight goes on toward its target at the new rate.
    pub(crate) fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        for control in &mut self.controls {
            retune(control, sample_rate);
        }
        for unit in &mut self.units {
            retune(&mut unit.mix, sample_rate);
            if let Some(effect) = &mut unit.effect {
                effect.update_sample_rate(sample_rate);
            }
        }
        self.eq.update_sample_rate(sample_rate);
        self.filter.update_sample_rate(sample_rate);
    }
}

impl FxUnit {
    /// On, holding an effect, and not settled fully dry.
    fn running(&self) -> bool {
        self.on && self.effect.is_some() && !self.mix.has_settled_at(0.0)
    }

    /// Run the unit's effect on the frames of a stereo pair and mix its wet
    /// signal in. While the dry/wet ramps, the effect runs up to the frame the
    /// ramp settles on, whatever block holds it; settled fully dry, the unit
    /// neither runs its effect nor touches the frames. Off or empty, the unit
    /// is not heard, so its dry/wet takes its target at once.
    fn process(&mut self, left: &mut [f32], right: &mut [f32], wet: &mut [SampleBuffer; 2]) {
        let on = self.on;
        let Some(effect) = self.effect.as_mut().filter(|_| on) else {
            self.mix.reset_to_target();
            return;
        };
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

fn smoothed(value: f32, sample_rate: NonZeroU32) -> SmoothedParam {
    SmoothedParam::new(value, GAIN_SPAN, SmootherConfig::default(), sample_rate)
}

/// Retune `param` to `sample_rate`. The smoother keeps its target scaled by
/// the coefficient of the rate the target was set at, so the target is set
/// again at the new rate; otherwise a ramp in flight converges short of or past
/// its target and never settles.
fn retune(param: &mut SmoothedParam, sample_rate: NonZeroU32) {
    param.update_sample_rate(sample_rate);
    param.set_value(param.target_value());
}

fn native_values(control: &mut SmoothedParam, explicit: Option<&[f64]>, output: &mut [f32]) {
    if let Some(values) = explicit {
        kithara_dsp::narrow_f64(values, output);
        if let Some(value) = values.last() {
            control.set_value(value.as_());
            control.reset_to_target();
        }
    } else {
        control.process_into_buffer(output);
    }
}

#[cfg(test)]
mod tests {
    use kithara_dsp::param::DEFAULT_SMOOTH_SECONDS;
    use kithara_platform::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    use kithara_signal::SessionFrame;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        bridge::{
            CrossfaderGain, DeckMixSettings, DeckProtocol, FxMix, ParameterCurve, ParameterKnot,
            Trim,
        },
        rt::DeckMixerConfig,
        test_pools::pools,
    };

    fn apply_gain(gain: &mut SmoothedParam, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        let mut values = vec![0.0; frames];
        native_values(gain, None, &mut values);
        kithara_dsp::multiply_in_place(&mut left[..frames], &values);
        kithara_dsp::multiply_in_place(&mut right[..frames], &values);
    }

    #[kithara::test]
    fn gain_matches_scalar_pcm_and_terminal_state() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        for (target, settled) in [
            (1.0, false),
            (0.0, false),
            (0.5, false),
            (0.999_99, false),
            (0.5, true),
            (0.0, true),
        ] {
            for length in [0, 1, 255, 256, 257, 513] {
                for (left_extra, right_extra) in [(0, 0), (3, 0), (0, 3)] {
                    let mut gain = smoothed(1.0, rate);
                    gain.set_value(target);
                    if settled {
                        gain.reset_to_target();
                        assert!(gain.has_settled());
                    }
                    let mut expected_gain = gain;
                    if target == 0.999_99 && length > FADE_CHUNK {
                        let mut premature = expected_gain;
                        premature.process_into_buffer(&mut [0.0; FADE_CHUNK]);
                        assert!(
                            premature.has_settled(),
                            "the first scratch chunk could settle early"
                        );
                    }
                    let mut left: Vec<_> = [0.25, -0.75, 0.0, -0.0]
                        .into_iter()
                        .cycle()
                        .take(length + left_extra)
                        .collect();
                    let mut right = vec![-0.5; length + right_extra];
                    let (mut expected_left, mut expected_right) = (left.clone(), right.clone());
                    let moving = !expected_gain.has_settled();
                    for (l, r) in expected_left.iter_mut().zip(&mut expected_right) {
                        let gain = if moving {
                            expected_gain.next_smoothed()
                        } else {
                            target
                        };
                        *l *= gain;
                        *r *= gain;
                    }
                    if moving {
                        expected_gain.settle();
                    }
                    apply_gain(&mut gain, &mut left, &mut right);
                    assert_eq!(
                        left.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                        expected_left
                            .iter()
                            .map(|x| x.to_bits())
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(
                        right.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                        expected_right
                            .iter()
                            .map(|x| x.to_bits())
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(gain, expected_gain, "target {target}, length {length}");
                }
            }
        }
    }

    /// A constant wet signal recording each prepared sample rate.
    struct Tuned(Arc<Mutex<Vec<u32>>>, f32);

    impl DeckEffect for Tuned {
        fn process(&mut self, _: [&[f32]; 2], wet: [&mut [f32]; 2]) {
            for wet in wet {
                wet.fill(self.1);
            }
        }

        fn accepts(&self, _: &FxParamChange) -> bool {
            false
        }

        fn set(&mut self, _: FxParamChange) {}

        fn reset(&mut self) {}

        fn tail_exhausted(&self) -> bool {
            true
        }

        fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
            self.0.lock().push(sample_rate.get());
        }
    }

    #[kithara::test]
    fn an_installed_effect_runs_at_the_deck_sample_rate() {
        let rate = |hz| NonZeroU32::new(hz).expect("rate");
        let mut render = RenderPass::new(
            &pools(),
            StreamShape::new(rate(512), rate(48_000)),
            DeckMixerConfig::builder().build(),
        )
        .expect("deck chain");
        let tuned = Arc::new(Mutex::new(Vec::new()));
        let install = DeckFxChange::Install {
            unit: 0,
            effect: Box::new(Tuned(Arc::clone(&tuned), 0.0)),
        };
        assert!(
            render.change_fx(install, rate(48_000)).is_none(),
            "an empty unit returns nothing"
        );
        render.update_sample_rate(rate(96_000));
        assert_eq!(*tuned.lock(), [48_000, 96_000]);
    }

    #[kithara::test]
    fn eq_program_pcm_keeps_fx_after_eq_and_fader_after_fx() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        let mix = DeckMixSettings::builder()
            .crossfader(CrossfaderGain::checked(0.5).expect("half output"))
            .build();
        let mut render = RenderPass::new(
            &pools(),
            StreamShape::new(NonZeroU32::new(4).expect("four frames"), rate),
            DeckMixerConfig::builder().eq_bands(1).mix(mix).build(),
        )
        .expect("one-band deck");
        let effect = Box::new(Tuned(Arc::new(Mutex::new(Vec::new())), 0.25));
        for change in [
            DeckFxChange::Install { unit: 0, effect },
            DeckFxChange::Power { unit: 0, on: true },
            DeckFxChange::Mix {
                unit: 0,
                value: FxMix::checked(1.0).expect("fully wet"),
            },
        ] {
            render.change_fx(change, rate);
        }
        render.idle();
        let start = SessionFrame::new(32);
        let program = ParameterCurve::new(vec![
            ParameterKnot::new(start, 6.0, kithara_dsp::curve::CurveShape::HOLD)
                .expect("held gain"),
        ])
        .expect("gain programme");
        let (mut sender, _) = kithara_command::scoped_channel::<DeckProtocol, DeckProtocol>(
            kithara_command::ScopedConfig::builder().build(),
        );
        let scope = sender.open(1).expect("scope");
        let mut programs = DeckPrograms::new(scope, 1, 2, 4);
        programs.write_curve(DeckParameter::eq_index(0), &program, start, 4);
        let (mut left, mut right) = ([0.125; 4], [-0.5; 4]);
        render
            .finish_curves(&mut left, &mut right, &programs)
            .expect("valid gain rows");
        assert_eq!(left.map(f32::to_bits), [0.125_f32.to_bits(); 4]);
        assert_eq!(right.map(f32::to_bits), [0.125_f32.to_bits(); 4]);
        let mut gains = [GainDb::DEFAULT];
        assert_eq!(render.read_eq(&mut gains), 1);
        assert_eq!(gains[0], GainDb::MAX);
    }

    /// A ramp a sample-rate change catches goes on toward its target at the
    /// new rate: one ramp later the deck sounds its settled chain and its unit
    /// no longer rings.
    #[kithara::test]
    fn a_ramp_caught_by_a_sample_rate_change_settles_on_its_target() {
        const BLOCK: usize = 512;
        const INPUT: f32 = 0.5;
        let rate = |hz| NonZeroU32::new(hz).expect("rate");
        let levels = |level| {
            [
                DeckMixSettingsChange::Trim(Trim::checked(level).expect("a trim")),
                DeckMixSettingsChange::Crossfader(
                    CrossfaderGain::checked(level).expect("a crossfader share"),
                ),
            ]
        };
        let dry_wet = |value| DeckFxChange::Mix {
            unit: 0,
            value: FxMix::checked(value).expect("a dry/wet"),
        };
        let block = |render: &mut RenderPass| {
            let (mut left, mut right) = ([INPUT; BLOCK], [INPUT; BLOCK]);
            render
                .finish(&mut left, &mut right)
                .expect("manual gain rows");
            [left, right]
        };
        // With the coefficient of the old rate, ramps up after a fall in rate
        // converge short of their targets, and ramps down after a rise past them.
        for (from, to, [level_from, level_to], [mix_from, mix_to]) in [
            (48_000, 44_100, [0.5, 1.0], [0.0, 0.5]),
            (44_100, 48_000, [1.0, 0.5], [1.0, 0.5]),
        ] {
            let mut render = RenderPass::new(
                &pools(),
                StreamShape::new(
                    rate(u32::try_from(BLOCK).expect("a block fits u32")),
                    rate(from),
                ),
                DeckMixerConfig::builder().eq_bands(0).build(),
            )
            .expect("deck chain");
            let effect = Box::new(Tuned(Arc::new(Mutex::new(Vec::new())), 0.0));
            for change in [
                DeckFxChange::Install { unit: 0, effect },
                DeckFxChange::Power { unit: 0, on: true },
                dry_wet(mix_from),
            ] {
                render.change_fx(change, rate(from));
            }
            for change in levels(level_from) {
                render.set_mix(change);
            }
            render.idle();
            for change in levels(level_to) {
                render.set_mix(change);
            }
            render.change_fx(dry_wet(mix_to), rate(from));
            block(&mut render);
            render.update_sample_rate(rate(to));
            let ramp = Duration::from_secs_f32(DEFAULT_SMOOTH_SECONDS).as_nanos() * u128::from(to);
            let ramp = usize::try_from(ramp.div_ceil(1_000_000_000)).expect("a ramp fits usize");
            for _ in 0..ramp.div_ceil(BLOCK) {
                block(&mut render);
            }
            let settled = INPUT * level_to * (1.0 - mix_to) * level_to;
            assert!(
                block(&mut render)
                    .iter()
                    .flatten()
                    .all(|&sample| sample == settled),
                "{from} Hz to {to} Hz: the chain sounds {settled} once settled"
            );
            assert!(
                !render.ringing(),
                "{from} Hz to {to} Hz: a settled unit with its tail spent no longer rings"
            );
        }
    }
}
