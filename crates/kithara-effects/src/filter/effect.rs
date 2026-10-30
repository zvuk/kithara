use std::num::NonZeroUsize;

use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_config::LiveConfig;
use kithara_dsp::{
    filter::{Biquad, Coefficients, FilterError},
    param::{SmoothedParam64, SmootherConfig},
};
use kithara_signal::{AudioChunk, AudioSpec};

use super::{FilterConfig, FilterSettings, FilterSettingsChange};
use crate::{AudioEffect, EffectChange, EffectPrepareError, EffectRefusal, EffectTarget, consts};

/// Why a filter's numerical state or pooled scratch could not be prepared.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PrepareError {
    #[error(transparent)]
    Filter(#[from] FilterError),
    #[error(transparent)]
    Pool(#[from] PoolError),
}

/// Causal pooled filtering with retained history and per-frame manual smoothing.
///
/// Steps use the shared default one-pole in f64 and settle each frame within
/// its declared fraction of the current step's distance.
#[derive(kithara_config::ConfigOwner)]
#[config_owner(settings)]
#[non_exhaustive]
pub struct FilterEffect<S> {
    spec: AudioSpec,
    settings: FilterSettings,
    pools: PoolRegion<S>,
    filter: Biquad,
    target_coefficients: Coefficients<f64>,
    cutoff: SmoothedParam64,
    q: SmoothedParam64,
    gain: SmoothedParam64,
    planes: Vec<SampleBuffer>,
}

impl<S: HasPool<f32>> FilterEffect<S> {
    /// Prepare state and pooled scratch outside audio processing.
    /// # Errors
    /// Refuses invalid geometry, unrepresentable coefficients or insufficient pool budget.
    pub fn new(spec: AudioSpec, config: FilterConfig<S>) -> Result<Self, PrepareError> {
        let channels = NonZeroUsize::new(usize::from(spec.channels)).ok_or(FilterError::Shape)?;
        let coefficients = config.coefficients(f64::from(spec.sample_rate.get()))?;
        let FilterConfig { pools, settings } = config;
        let mut filter = Biquad::new(channels, NonZeroUsize::MIN)?;
        filter.retune(0, coefficients)?;
        let planes = (0..channels.get())
            .map(|_| pools.get_with_len::<f32>(consts::FILTER_BLOCK_FRAMES))
            .collect::<Result<_, _>>()?;
        let smoothing = SmootherConfig::default();
        Ok(Self {
            spec,
            settings,
            pools,
            filter,
            target_coefficients: coefficients,
            cutoff: SmoothedParam64::new(
                f64::from(settings.cutoff_hz()),
                0.0,
                smoothing,
                spec.sample_rate,
            ),
            q: SmoothedParam64::new(f64::from(settings.q()), 0.0, smoothing, spec.sample_rate),
            gain: SmoothedParam64::new(
                f64::from(f32::from(settings.gain_db())),
                0.0,
                smoothing,
                spec.sample_rate,
            ),
            planes,
        })
    }

    fn settled(&self) -> bool {
        self.cutoff.has_settled() && self.q.has_settled() && self.gain.has_settled()
    }

    fn process_planes(&mut self, frames: usize) {
        let mut frame = 0;
        while frame < frames {
            if self.settled() {
                self.filter
                    .process(&mut self.planes, frame..frames)
                    .expect("prepared planes cover the block");
                break;
            }
            for parameter in [&mut self.cutoff, &mut self.q, &mut self.gain] {
                if !parameter.has_settled() {
                    parameter.next_smoothed();
                    parameter.settle();
                }
            }
            let coefficients = if self.settled() {
                self.target_coefficients
            } else {
                self.settings
                    .coefficients_at(
                        f64::from(self.spec.sample_rate.get()),
                        self.cutoff.current_value(),
                        self.q.current_value(),
                        self.gain.current_value(),
                    )
                    .expect("admitted smoothing stays inside the physical parameter bounds")
            };
            self.filter
                .retune(0, coefficients)
                .expect("admitted smoothing box has stable f32 coefficients");
            self.filter
                .process(&mut self.planes, frame..frame + 1)
                .expect("prepared planes cover the frame");
            frame += 1;
        }
    }
}

impl<S: HasPool<f32> + Send + Sync + 'static> AudioEffect for FilterEffect<S> {
    fn configure(
        &mut self,
        target: EffectTarget,
        change: EffectChange,
    ) -> Result<(), EffectRefusal> {
        let (EffectTarget::Filter { .. }, EffectChange::Filter(change)) = (target, change) else {
            return Err(EffectRefusal);
        };
        let mut settings = self.settings;
        settings.apply_change(change);
        if settings == self.settings {
            return Ok(());
        }
        let rate = self.spec.sample_rate;
        let coefficients = settings
            .coefficients(f64::from(rate.get()))
            .map_err(|_| EffectRefusal)?;
        settings
            .admit_smoothing(
                f64::from(rate.get()),
                self.cutoff.current_value(),
                self.q.current_value(),
                self.gain.current_value(),
            )
            .map_err(|_| EffectRefusal)?;
        let transition = |current: f64, target: f64| {
            let mut parameter = SmoothedParam64::new(
                current,
                (target - current).abs(),
                SmootherConfig::default(),
                rate,
            );
            parameter.set_value(target);
            parameter
        };
        let (mut cutoff, mut q, mut gain) = (self.cutoff, self.q, self.gain);
        match change {
            FilterSettingsChange::CutoffHz(value) => {
                cutoff = transition(cutoff.current_value(), f64::from(value));
            }
            FilterSettingsChange::Q(value) => q = transition(q.current_value(), f64::from(value)),
            FilterSettingsChange::GainDb(value) => {
                gain = transition(gain.current_value(), f64::from(f32::from(value)));
            }
        }
        self.filter
            .retune(0, coefficients)
            .map_err(|_| EffectRefusal)?;
        self.settings = settings;
        self.target_coefficients = coefficients;
        self.cutoff = cutoff;
        self.q = q;
        self.gain = gain;
        Ok(())
    }

    fn service_deferred(&mut self, spec: AudioSpec) -> Result<(), EffectPrepareError> {
        if self.accepts_spec(spec) {
            return Ok(());
        }
        if spec.channels == 0 {
            return Err(EffectPrepareError::Shape);
        }
        let rate = f64::from(spec.sample_rate.get());
        if !self.settled() {
            self.settings.admit_smoothing(
                rate,
                self.cutoff.current_value(),
                self.q.current_value(),
                self.gain.current_value(),
            )?;
        }
        let mut prepared = Self::new(
            spec,
            FilterConfig::from((self.pools.clone(), self.settings)),
        )
        .map_err(EffectPrepareError::from)?;
        let coefficients = self.settings.coefficients_at(
            rate,
            self.cutoff.current_value(),
            self.q.current_value(),
            self.gain.current_value(),
        )?;
        prepared.filter.retune(0, coefficients)?;
        prepared.cutoff = self.cutoff;
        prepared.q = self.q;
        prepared.gain = self.gain;
        for parameter in [&mut prepared.cutoff, &mut prepared.q, &mut prepared.gain] {
            parameter.update_sample_rate(spec.sample_rate);
        }
        *self = prepared;
        Ok(())
    }

    fn accepts_spec(&self, spec: AudioSpec) -> bool {
        self.spec == spec
    }

    fn flush(&mut self) -> Option<AudioChunk> {
        None
    }

    fn held_source_frames(&self) -> u64 {
        0
    }

    fn process(&mut self, mut chunk: AudioChunk) -> Option<AudioChunk> {
        let channels = self.planes.len();
        debug_assert!(self.accepts_spec(chunk.spec()));
        debug_assert!(chunk.samples.len().is_multiple_of(channels));
        for block in chunk
            .samples
            .chunks_mut(consts::FILTER_BLOCK_FRAMES * channels)
        {
            let frames = block.len() / channels;
            kithara_dsp::deinterleave_variable(
                block,
                NonZeroUsize::new(channels).expect("prepared nonzero channels"),
                &mut self.planes,
                0..frames,
            );
            self.process_planes(frames);
            for (frame, output) in block.chunks_exact_mut(channels).enumerate() {
                for (sample, plane) in output.iter_mut().zip(&self.planes) {
                    *sample = plane[frame];
                }
            }
        }
        Some(chunk)
    }

    fn reset(&mut self) {
        self.filter
            .retune(0, self.target_coefficients)
            .expect("prepared target coefficients");
        self.filter.reset();
        for parameter in [&mut self.cutoff, &mut self.q, &mut self.gain] {
            parameter.reset_to_target();
        }
    }
}
