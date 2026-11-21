use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_command::Seq;
use kithara_config::LiveConfig;
use kithara_dsp::param::{SmoothedParam, SmootherConfig};
use kithara_effects::{
    GainDb,
    eq::{EqConfig, EqLayout, GainShapeError, StereoEq, generate_log_spaced_bands},
    filter::{FilterEffect, FilterKnob},
};
use num_traits::cast::AsPrimitive;

use super::fx::FxUnit;
use crate::{
    bridge::{CrossfaderGain, DeckMixSettings, DeckMixSettingsChange, DeckParameter},
    consts::GAIN_SPAN,
    rt::{DeckMixerConfig, command::DeckPrograms, processor::StreamShape},
};

/// A deck's chain in Traktor's Insert order: trim, equaliser, FX units in
/// series, the filter knob, then fader and crossfader.
pub(crate) struct RenderPass {
    trim_value: f32,
    mix: DeckMixSettings,
    /// The deck's trim, ramped to each new target from the frame it is set on.
    trim: SmoothedParam,
    control_buffers: [SampleBuffer; DeckParameter::CHANNELS],
    /// The deck's equaliser, after its trim.
    eq: StereoEq,
    /// The deck's FX units, after its equaliser.
    pub(super) units: Box<[FxUnit]>,
    /// The deck filter, after the FX units; transparent at its center.
    filter: FilterEffect,
    /// The wet signal of the unit running now.
    wet: [SampleBuffer; crate::consts::MIN_STEREO],
    /// Fader times crossfader after the units, cutting their tails; programmed factors stay outside its target.
    output: SmoothedParam,
    output_current: f32,
    output_owners: [Option<Seq>; DeckParameter::CHANNELS],
    /// Set until the first range renders, which moves every ramp to its target at once.
    priming: bool,
}

impl RenderPass {
    pub(crate) fn new<S>(
        pools: &PoolRegion<S>,
        shape: StreamShape,
        config: DeckMixerConfig,
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
            trim_value: f32::from(mix.trim()),
            mix,
            trim: smoothed(f32::from(mix.trim()), shape.sample_rate),
            output: smoothed(mix.output_gain(), shape.sample_rate),
            output_current: mix.output_gain(),
            output_owners: [None; DeckParameter::CHANNELS],
            eq,
            units: (0..config.fx_units())
                .map(|_| FxUnit {
                    effect: None,
                    mix: smoothed(0.0, shape.sample_rate),
                    on: false,
                    delivered: vec![None; config.fx_parameters()].into_boxed_slice(),
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
        self.prepare_output(programs);
        let trim_values = programs.and_then(|rows| rows.values(DeckParameter::TRIM, frames));
        let trim_state = trim_values.is_none().then_some(self.trim);
        let trim = &mut self.control_buffers[DeckParameter::TRIM][..frames];
        if let Some(value) = native_values(&mut self.trim, trim_values, trim, false) {
            self.trim_value = value;
        }
        apply_gain(trim_state, left, right, trim);
        let gains = programs.map_or(&[][..], DeckPrograms::eq_values);
        self.eq.process_with_gains(left, right, gains)?;
        for (index, unit) in self.units.iter_mut().enumerate() {
            unit.run(index, [&mut *left, &mut *right], &mut self.wet, programs);
        }
        let planes = &mut [&mut left[..frames], &mut right[..frames]];
        let knobs = programs.and_then(|rows| rows.values(DeckParameter::FILTER, frames));
        if let Some(knobs) = knobs {
            self.filter
                .process_program(planes, 0..frames, knobs)
                .expect("prepared stereo knob program");
        } else {
            self.filter
                .process(planes, 0..frames)
                .expect("stereo planes");
        }
        let programmed = self.output_owners.iter().any(Option::is_some);
        if programmed {
            for (index, value) in [
                (DeckParameter::VOLUME, f32::from(self.mix.volume())),
                (DeckParameter::CROSSFADER, f32::from(self.mix.crossfader())),
            ] {
                let output = &mut self.control_buffers[index][..frames];
                if self.output_owners[index].is_some() {
                    if let Some(values) = programs.and_then(|rows| rows.values(index, frames)) {
                        kithara_dsp::narrow_f64(values, output);
                    } else {
                        output.fill(value);
                    }
                    if let Some(value) = output.last() {
                        let change = if index == DeckParameter::VOLUME {
                            Some(DeckMixSettingsChange::Volume((*value).into()))
                        } else {
                            CrossfaderGain::checked(*value).map(DeckMixSettingsChange::Crossfader)
                        };
                        if let Some(change) = change {
                            self.mix.apply_change(change);
                        }
                    }
                } else {
                    output.fill(1.0);
                }
            }
        }
        let state = (!programmed).then_some(self.output);
        let [output, volume, crossfader] = &mut self.control_buffers;
        let output = &mut output[..frames];
        let value = native_values(&mut self.output, None, output, programmed);
        if self.output.has_settled() {
            self.output_current = self.output.target_value();
        } else if let Some(value) = value {
            self.output_current = value;
        }
        if programmed {
            kithara_dsp::multiply_squared_in_place(
                output,
                &volume[..frames],
                &crossfader[..frames],
            );
        }
        apply_gain(state, left, right, output);
        Ok(())
    }

    /// Move programs and the knob glide across frames that sounded nothing.
    /// The filter holds no history then, so a program's last position is taken at once.
    pub(in crate::rt) fn advance(
        &mut self,
        frames: usize,
        programs: &DeckPrograms,
    ) -> Result<(), GainShapeError> {
        self.prepare_output(Some(programs));
        for index in [
            DeckParameter::TRIM,
            DeckParameter::VOLUME,
            DeckParameter::CROSSFADER,
        ] {
            if let Some(value) = programs.values(index, frames).and_then(<[f64]>::last) {
                let value: f32 = value.as_();
                match index {
                    DeckParameter::TRIM => {
                        self.trim_value = value;
                        self.trim.set_value(value);
                        self.trim.reset_to_target();
                    }
                    DeckParameter::VOLUME => self
                        .mix
                        .apply_change(DeckMixSettingsChange::Volume(value.into())),
                    _ => {
                        if let Some(value) = CrossfaderGain::checked(value) {
                            self.mix
                                .apply_change(DeckMixSettingsChange::Crossfader(value));
                        }
                    }
                }
            }
        }
        self.eq.advance_gains(programs.eq_values(), frames)?;
        for (index, unit) in self.units.iter_mut().enumerate() {
            unit.advance(programs, index, frames);
        }
        let knobs = programs.values(DeckParameter::FILTER, frames);
        if let Some(value) = knobs.and_then(<[f64]>::last) {
            if let Some(knob) = FilterKnob::checked(*value) {
                self.filter.configure(knob);
                self.filter.reset();
            }
        } else {
            self.filter.advance(frames);
        }
        Ok(())
    }

    pub(crate) fn idle(&mut self) {
        self.trim.reset_to_target();
        self.trim_value = self.trim.target_value();
        self.output.reset_to_target();
        self.output_current = self.output.target_value();
        for unit in &mut self.units {
            unit.mix.reset_to_target();
        }
    }

    /// Whether a unit still sounds a tail of input the deck no longer plays,
    /// or still ramps its dry/wet, which settles on its own frame only while
    /// the chain renders.
    pub(in crate::rt) fn ringing(&self, programs: Option<&DeckPrograms>, frames: usize) -> bool {
        !self.filter.tail_exhausted()
            || self.units.iter().enumerate().any(|(index, unit)| {
                unit.running(programs.and_then(|rows| rows.mix_values(index, frames)))
                    && (!unit.mix.has_settled()
                        || unit
                            .effect
                            .as_ref()
                            .is_some_and(|effect| !effect.tail_exhausted()))
            })
    }

    delegate::delegate! {
        to self.eq {
            #[call(target_gain)]
            pub(crate) fn eq_target_gain(&self, band: usize) -> Option<GainDb>;
            #[call(killed)]
            pub(crate) fn eq_killed(&self, band: usize) -> Option<bool>;
            #[call(band_count)]
            pub(crate) fn eq_bands(&self) -> usize;
            /// Ramp `band` of the deck's equaliser to `gain` from the next frame rendered.
            #[call(set_gain)]
            pub(crate) fn set_eq_gain(&mut self, band: usize, gain: GainDb);
            #[call(set_kill)]
            pub(crate) fn set_eq_kill(&mut self, band: usize, on: bool);
            #[call(reset_gains)]
            pub(crate) fn reset_eq(&mut self);
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
        retune(&mut self.trim, sample_rate);
        retune(&mut self.output, sample_rate);
        for unit in &mut self.units {
            retune(&mut unit.mix, sample_rate);
            if let Some(effect) = &mut unit.effect {
                effect.update_sample_rate(sample_rate);
            }
        }
        self.eq.update_sample_rate(sample_rate);
        self.filter.update_sample_rate(sample_rate);
    }
    pub(crate) fn set_mix(&mut self, change: DeckMixSettingsChange) {
        match change {
            DeckMixSettingsChange::Volume(_) => self.release_output(DeckParameter::VOLUME),
            DeckMixSettingsChange::Crossfader(_) => self.release_output(DeckParameter::CROSSFADER),
            _ => {}
        }
        self.mix.apply_change(change);
        match change {
            DeckMixSettingsChange::Trim(value) => self.trim.set_value(f32::from(value)),
            DeckMixSettingsChange::Filter(value) => self.set_filter_knob(value),
            _ => {}
        }
        self.output
            .set_value(output_target(self.mix, self.output_owners));
    }

    #[must_use]
    pub(crate) fn control_values(&self) -> [f64; DeckParameter::CHANNELS] {
        [
            f64::from(self.trim_value),
            f64::from(f32::from(self.mix.volume())),
            f64::from(f32::from(self.mix.crossfader())),
        ]
    }

    fn release_output(&mut self, index: usize) {
        if self.output_owners[index].take().is_none() {
            return;
        }
        let (volume, cross) = if index == DeckParameter::VOLUME {
            (f32::from(self.mix.volume()), 1.0)
        } else {
            (1.0, f32::from(self.mix.crossfader()))
        };
        let mut current = [self.output_current];
        kithara_dsp::multiply_squared_in_place(&mut current, &[volume], &[cross]);
        self.output.set_value(current[0]);
        self.output.reset_to_target();
        self.output_current = current[0];
    }

    /// Program entry snaps the remainder to manual targets; release folds held factors.
    fn prepare_output(&mut self, programs: Option<&DeckPrograms>) {
        let mut owners = [None; DeckParameter::CHANNELS];
        for (index, parameter) in [
            (DeckParameter::VOLUME, DeckParameter::Volume),
            (DeckParameter::CROSSFADER, DeckParameter::Crossfader),
        ] {
            owners[index] = programs.and_then(|programs| programs.owner(parameter));
        }
        let target = output_target(self.mix, owners);
        let entering = owners
            .iter()
            .zip(&self.output_owners)
            .any(|(new, old)| new.is_some() && new != old);
        if entering {
            self.output.set_value(target);
            self.output.reset_to_target();
            self.output_current = target;
        } else {
            for index in [DeckParameter::VOLUME, DeckParameter::CROSSFADER] {
                if owners[index].is_none() {
                    self.release_output(index);
                }
            }
        }
        self.output.set_value(target);
        self.output_owners = owners;
    }
}

fn output_target(mix: DeckMixSettings, owners: [Option<Seq>; DeckParameter::CHANNELS]) -> f32 {
    let mut remainder = mix;
    if owners[DeckParameter::VOLUME].is_some() {
        remainder.apply_change(DeckMixSettingsChange::Volume(
            kithara_signal::FaderValue::DEFAULT,
        ));
    }
    if owners[DeckParameter::CROSSFADER].is_some() {
        remainder.apply_change(DeckMixSettingsChange::Crossfader(CrossfaderGain::DEFAULT));
    }
    remainder.output_gain()
}

fn apply_gain(state: Option<SmoothedParam>, left: &mut [f32], right: &mut [f32], gains: &[f32]) {
    if state.is_some_and(|gain| gain.has_settled_at(1.0)) {
        return;
    }
    let frames = left.len().min(right.len()).min(gains.len());
    if let Some(gain) = state.filter(SmoothedParam::has_settled) {
        kithara_dsp::scale_in_place(&mut left[..frames], gain.target_value());
        kithara_dsp::scale_in_place(&mut right[..frames], gain.target_value());
    } else {
        kithara_dsp::multiply_in_place(left, gains);
        kithara_dsp::multiply_in_place(right, gains);
    }
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

fn native_values(
    control: &mut SmoothedParam,
    explicit: Option<&[f64]>,
    output: &mut [f32],
    buffer_required: bool,
) -> Option<f32> {
    if let Some(values) = explicit {
        kithara_dsp::narrow_f64(values, output);
        if let Some(value) = values.last() {
            control.set_value(value.as_());
            control.reset_to_target();
        }
    } else if buffer_required || control.is_smoothing() {
        control.process_into_buffer(output);
    } else {
        return (!output.is_empty()).then_some(control.target_value());
    }
    output.last().copied()
}

#[cfg(test)]
mod tests {
    use kithara_dsp::param::DEFAULT_SMOOTH_SECONDS;
    use kithara_effects::{DeckEffect, FxParamChange, FxParamDescriptor};
    use kithara_platform::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    use kithara_signal::SessionFrame;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        bridge::{
            CrossfaderGain, DeckFxChange, DeckMixSettings, DeckProtocol, FxMix, ParameterCurve,
            ParameterKnot, Trim,
        },
        consts::FADE_CHUNK,
        rt::DeckMixerConfig,
        test_pools::pools,
    };

    fn apply_gain(gain: &mut SmoothedParam, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        let mut values = vec![0.0; frames];
        let state = *gain;
        let _ = native_values(gain, None, &mut values, false);
        super::apply_gain(Some(state), left, right, &values);
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

        fn parameters(&self) -> &[FxParamDescriptor] {
            &[]
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
            kithara_command::ScopedConfig::builder()
                .scope(kithara_command::ChannelConfig::builder().targets(1).build())
                .build(),
        );
        let scope = sender.open(1).expect("scope");
        let mut programs = DeckPrograms::new(scope, [1, 2, 0], 4);
        programs.write_curve(DeckParameter::eq_index(0), &program, start, 4);
        let (mut left, mut right) = ([0.125; 4], [-0.5; 4]);
        render
            .finish_curves(&mut left, &mut right, &programs)
            .expect("valid gain rows");
        assert_eq!(left.map(f32::to_bits), [0.125_f32.to_bits(); 4]);
        assert_eq!(right.map(f32::to_bits), [0.125_f32.to_bits(); 4]);
        assert_eq!(render.eq_bands(), 1);
        assert_eq!(render.eq_target_gain(0), Some(GainDb::MAX));
    }

    #[kithara::test]
    fn manual_output_keeps_the_main_combined_amplitude_ramp() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        let mut render = RenderPass::new(
            &pools(),
            StreamShape::new(NonZeroU32::new(16).expect("block"), rate),
            DeckMixerConfig::builder().eq_bands(0).build(),
        )
        .expect("chain");
        let volume = kithara_signal::FaderValue::checked(0.5).expect("volume");
        let crossfader = CrossfaderGain::checked(0.5).expect("crossfader");
        let mix = DeckMixSettings::builder()
            .volume(volume)
            .crossfader(crossfader)
            .build();
        let mut reference = smoothed(1.0, rate);
        reference.set_value(mix.output_gain());
        let expected: [f32; 16] = std::array::from_fn(|_| 0.5 * reference.next_smoothed());
        render.set_mix(DeckMixSettingsChange::Volume(volume));
        render.set_mix(DeckMixSettingsChange::Crossfader(crossfader));
        let (mut left, mut right) = ([0.5; 16], [0.5; 16]);
        render.finish(&mut left, &mut right).expect("manual PCM");
        assert_eq!(left.map(f32::to_bits), expected.map(f32::to_bits));
        assert_eq!(right.map(f32::to_bits), expected.map(f32::to_bits));
    }

    #[kithara::test]
    fn quiet_eq_program_keeps_each_gain_application_without_advancing_filter_history() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        let config = DeckMixerConfig::builder().eq_bands(2).build();
        let shape = StreamShape::new(NonZeroU32::new(3).expect("three frames"), rate);
        let mut actual = RenderPass::new(&pools(), shape, config).expect("actual chain");
        let mut reference = RenderPass::new(&pools(), shape, config).expect("reference chain");
        for render in [&mut actual, &mut reference] {
            render.set_eq_gain(1, GainDb::from(-6.0));
        }
        let (mut sender, _inbox) = kithara_command::scoped_channel::<DeckProtocol, DeckProtocol>(
            kithara_command::ScopedConfig::builder()
                .scope(kithara_command::ChannelConfig::builder().targets(1).build())
                .build(),
        );
        let scope = sender.open(1).expect("scope");
        let start = SessionFrame::new(0);
        let curve = ParameterCurve::new(
            [0.0, -6.0, 0.0]
                .into_iter()
                .enumerate()
                .map(|(frame, value)| {
                    ParameterKnot::new(
                        SessionFrame::new(i64::try_from(frame).expect("frame")),
                        value,
                        kithara_dsp::curve::CurveShape::HOLD,
                    )
                    .expect("gain knot")
                })
                .collect(),
        )
        .expect("prepared gain program");
        let mut programs = DeckPrograms::new(scope, [2, config.fx_units(), 0], 3);
        programs.write_curve(DeckParameter::eq_index(0), &curve, start, 3);
        actual.advance(3, &programs).expect("quiet gain rows");
        actual.idle();
        for value in [0.0, -6.0, 0.0] {
            reference
                .eq
                .advance_gains(&[[value]], 1)
                .expect("one explicit gain frame");
        }
        reference.idle();
        let (mut actual_left, mut actual_right) = ([0.5; 3], [-0.25; 3]);
        let (mut reference_left, mut reference_right) = (actual_left, actual_right);
        actual
            .finish(&mut actual_left, &mut actual_right)
            .expect("actual PCM");
        reference
            .finish(&mut reference_left, &mut reference_right)
            .expect("reference PCM");
        assert_eq!(
            actual_left.map(f32::to_bits),
            reference_left.map(f32::to_bits)
        );
        assert_eq!(
            actual_right.map(f32::to_bits),
            reference_right.map(f32::to_bits)
        );
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
                !render.ringing(None, 0),
                "{from} Hz to {to} Hz: a settled unit with its tail spent no longer rings"
            );
        }
    }
}
