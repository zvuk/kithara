use std::{
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
};

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use num_traits::cast::ToPrimitive;
use smallvec::SmallVec;

use self::filter::Filter;
use super::{GlideConfig, GlideInterpolation};
use crate::{ResamplerBuildError, ResamplerError, ResamplerMode};

mod consts {
    pub(super) const FILTER_CUTOFF_TO_NYQUIST: f64 = 0.9;
    pub(super) const FILTER_LOW_PASS_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;
}

pub(in crate::glide) struct RenderRequest<'a, I, O> {
    pub(in crate::glide) input: &'a [I],
    pub(in crate::glide) output: &'a mut [O],
    pub(in crate::glide) previous: &'a [SampleBuffer],
    pub(in crate::glide) config: GlideConfig,
    pub(in crate::glide) mode: ResamplerMode,
    pub(in crate::glide) filter_ratio: f64,
    pub(in crate::glide) produced: usize,
}

#[derive(fieldwork::Fieldwork)]
pub(in crate::glide) struct GlideEngine {
    filter_cutoff: Option<f64>,
    filtered_previous: SampleBuffer,
    positions: SampleBuffer,
    filtered: SmallVec<[SampleBuffer; 8]>,
    filters: SmallVec<[Option<Filter>; 8]>,
    padded: SmallVec<[SampleBuffer; 8]>,
    max_input_frames: usize,
    #[field(get(copy, name = position_capacity, vis = "pub(in crate::glide)"))]
    max_output_frames: usize,
}

impl GlideEngine {
    pub(in crate::glide) fn new<S>(
        pools: &PoolRegion<S>,
        channels: NonZeroUsize,
        max_input_frames: usize,
        max_ratio_adjustment: f64,
        backend: &'static str,
    ) -> Result<Self, ResamplerBuildError>
    where
        S: HasPool<f32>,
    {
        let max_output_frames = max_output_frames(max_input_frames, max_ratio_adjustment);
        let mut positions = pools.get::<f32>();
        ensure_build_len(&mut positions, max_output_frames, backend)?;
        let mut filtered_previous = pools.get::<f32>();
        ensure_build_len(&mut filtered_previous, channels.get(), backend)?;
        filtered_previous.fill(0.0);
        let mut padded = SmallVec::new();
        let mut filtered = SmallVec::new();
        let mut filters = SmallVec::new();
        for _ in 0..channels.get() {
            let mut padded_channel = pools.get::<f32>();
            ensure_build_len(
                &mut padded_channel,
                max_input_frames.saturating_add(2),
                backend,
            )?;
            padded.push(padded_channel);

            let mut filtered_channel = pools.get::<f32>();
            ensure_build_len(
                &mut filtered_channel,
                max_input_frames.saturating_add(2),
                backend,
            )?;
            filtered.push(filtered_channel);
            filters.push(None);
        }
        Ok(Self {
            positions,
            filtered_previous,
            padded,
            filtered,
            filters,
            max_input_frames,
            max_output_frames,
            filter_cutoff: None,
        })
    }

    fn ensure_filters(&mut self, sample_rate: f64, cutoff: f64) -> Result<(), ResamplerError> {
        if self
            .filter_cutoff
            .is_some_and(|current| (current - cutoff).abs() < 1.0)
        {
            return Ok(());
        }
        for filter in &mut self.filters {
            let ready = if let Some(filter) = filter {
                filter.retune(sample_rate, cutoff, consts::FILTER_LOW_PASS_Q)
            } else {
                *filter = Filter::low_pass(sample_rate, cutoff, consts::FILTER_LOW_PASS_Q);
                filter.is_some()
            };
            if !ready {
                return Err(ResamplerError::Backend {
                    op: filter::consts::FILTER_OP,
                    detail: filter::consts::FILTER_ERROR.into(),
                });
            }
        }
        self.filter_cutoff = Some(cutoff);
        Ok(())
    }

    pub(in crate::glide) fn positions_mut(
        &mut self,
        frames: usize,
    ) -> Result<&mut [f32], ResamplerError> {
        if frames > self.max_output_frames {
            return Err(ResamplerError::Backend {
                op: backend::consts::POSITIONS_OP,
                detail: "output frame request exceeds preallocated position buffer".into(),
            });
        }
        Ok(&mut self.positions[..frames])
    }

    pub(in crate::glide) fn render<I, O>(
        &mut self,
        request: RenderRequest<'_, I, O>,
    ) -> Result<(), ResamplerError>
    where
        I: Deref<Target = [f32]>,
        O: DerefMut<Target = [f32]>,
    {
        let RenderRequest {
            input,
            previous,
            output,
            produced,
            config,
            filter_ratio,
            mode,
        } = request;
        let input_frames = input.first().map_or(0, |channel| channel.deref().len());
        if input_frames > self.max_input_frames {
            return Err(ResamplerError::Backend {
                op: backend::consts::INPUT_OP,
                detail: "input frame count exceeds preallocated source buffer".into(),
            });
        }
        let sample_rate = sample_rate(mode);
        let cutoff = config
            .anti_alias
            .then(|| low_pass_cutoff(sample_rate, filter_ratio))
            .filter(|_| filter_ratio > 1.0);
        if let Some(cutoff) = cutoff {
            self.ensure_filters(sample_rate, cutoff)?;
        }

        for (channel_idx, source) in input.iter().take(self.padded.len()).enumerate() {
            let source = source.deref();
            let source = if cutoff.is_some() {
                let filtered = &mut self.filtered[channel_idx];
                let filter =
                    self.filters[channel_idx]
                        .as_mut()
                        .ok_or_else(|| ResamplerError::Backend {
                            op: filter::consts::FILTER_OP,
                            detail: "anti-alias filter was not initialized".into(),
                        })?;
                filtered[0] = self.filtered_previous[channel_idx];
                filter.process(source, &mut filtered[1..input_frames.saturating_add(1)]);
                self.filtered_previous[channel_idx] = filtered[input_frames];
                filtered[input_frames.saturating_add(1)] = filtered[input_frames];
                &filtered[..input_frames.saturating_add(2)]
            } else {
                let padded = &mut self.padded[channel_idx];
                padded[0] = previous[channel_idx][0];
                backend::copy(source, &mut padded[1..input_frames.saturating_add(1)]);
                padded[input_frames.saturating_add(1)] = source.last().copied().unwrap_or(0.0);
                &padded[..input_frames.saturating_add(2)]
            };

            backend::interpolate(
                config.interpolation,
                source,
                &self.positions[..produced],
                &mut output[channel_idx].deref_mut()[..produced],
            );
        }
        Ok(())
    }

    pub(in crate::glide) fn reset(&mut self) {
        self.filtered_previous.fill(0.0);
        for filter in self.filters.iter_mut().flatten() {
            filter.reset();
        }
    }
}

fn ensure_build_len(
    buffer: &mut SampleBuffer,
    frames: usize,
    backend: &'static str,
) -> Result<(), ResamplerBuildError> {
    buffer
        .ensure_len(frames)
        .map_err(|err| ResamplerBuildError::BackendBuild {
            backend,
            detail: err.to_string(),
        })
}

fn low_pass_cutoff(sample_rate: f64, ratio: f64) -> f64 {
    consts::FILTER_CUTOFF_TO_NYQUIST * sample_rate / (2.0 * ratio.max(1.0))
}

fn max_output_frames(input_frames: usize, max_ratio_adjustment: f64) -> usize {
    let Some(input_frames) = input_frames.to_f64() else {
        return usize::MAX;
    };
    let frames = (input_frames * max_ratio_adjustment).ceil();
    frames.to_usize().unwrap_or(usize::MAX).saturating_add(2)
}

fn sample_rate(mode: ResamplerMode) -> f64 {
    match mode {
        ResamplerMode::FixedRatio {
            source_sample_rate, ..
        } => f64::from(source_sample_rate.get()),
        ResamplerMode::VariableRatio { sample_rate, .. } => f64::from(sample_rate.get()),
    }
}

#[cfg(all(
    feature = "apple-accelerate",
    any(target_os = "macos", target_os = "ios")
))]
mod backend {
    use kithara_apple::accelerate::{copy_f32, linear_interpolate_f32, quadratic_interpolate_f32};

    use super::GlideInterpolation;

    pub(in crate::glide::engine) mod consts {
        pub(in crate::glide::engine) const INPUT_OP: &str = "glide accelerate input";
        pub(in crate::glide::engine) const POSITIONS_OP: &str = "glide accelerate positions";
    }

    pub(super) fn copy(source: &[f32], target: &mut [f32]) {
        copy_f32(source, target);
    }

    pub(super) fn interpolate(
        kind: GlideInterpolation,
        source: &[f32],
        positions: &[f32],
        target: &mut [f32],
    ) {
        match kind {
            GlideInterpolation::Linear => linear_interpolate_f32(source, positions, target),
            GlideInterpolation::Quadratic => quadratic_interpolate_f32(source, positions, target),
        };
    }
}

#[cfg(not(all(
    feature = "apple-accelerate",
    any(target_os = "macos", target_os = "ios")
)))]
mod backend {
    use num_traits::cast::ToPrimitive;

    use super::GlideInterpolation;

    pub(in crate::glide::engine) mod consts {
        pub(in crate::glide::engine) const INPUT_OP: &str = "glide scalar input";
        pub(in crate::glide::engine) const POSITIONS_OP: &str = "glide scalar positions";
    }

    pub(super) fn copy(source: &[f32], target: &mut [f32]) {
        target.copy_from_slice(source);
    }

    pub(super) fn interpolate(
        kind: GlideInterpolation,
        source: &[f32],
        positions: &[f32],
        target: &mut [f32],
    ) {
        match kind {
            GlideInterpolation::Linear => {
                interpolate_with::<LinearInterpolation>(source, positions, target);
            }
            GlideInterpolation::Quadratic => {
                interpolate_with::<QuadraticInterpolation>(source, positions, target);
            }
        }
    }

    trait Interpolation {
        fn sample(source: &[f32], base: usize, frac: f32) -> f32;
    }

    struct LinearInterpolation;

    impl Interpolation for LinearInterpolation {
        fn sample(source: &[f32], base: usize, frac: f32) -> f32 {
            let center = source.get(base).copied().unwrap_or(0.0);
            let right = source.get(base.saturating_add(1)).copied().unwrap_or(0.0);
            center.mul_add(1.0 - frac, right * frac)
        }
    }

    struct QuadraticInterpolation;

    impl Interpolation for QuadraticInterpolation {
        fn sample(source: &[f32], base: usize, frac: f32) -> f32 {
            let left = if base == 0 {
                source.first().copied().unwrap_or(0.0)
            } else {
                source.get(base.saturating_sub(1)).copied().unwrap_or(0.0)
            };
            let center = source.get(base).copied().unwrap_or(0.0);
            let right = source.get(base.saturating_add(1)).copied().unwrap_or(0.0);
            let slope = 0.5 * (right - left);
            let curve = 0.5 * (right - 2.0 * center + left);
            center + frac * slope + frac * frac * curve
        }
    }

    fn interpolate_with<I>(source: &[f32], positions: &[f32], target: &mut [f32])
    where
        I: Interpolation,
    {
        for (position, output) in positions.iter().zip(target.iter_mut()) {
            let base = position.floor().to_usize().unwrap_or(usize::MAX);
            let frac = position - base.to_f32().unwrap_or(0.0);
            *output = I::sample(source, base, frac);
        }
    }
}

mod filter {
    use num_traits::cast::ToPrimitive;

    pub(in crate::glide::engine) mod consts {
        pub(in crate::glide::engine) const FILTER_OP: &str = "glide scalar filter";
        pub(in crate::glide::engine) const FILTER_ERROR: &str = "failed to create low-pass filter";
    }

    pub(super) struct Filter {
        coefficients: [f64; 5],
        delay: [f64; 4],
    }

    impl Filter {
        pub(super) fn low_pass(sample_rate: f64, cutoff_hz: f64, q: f64) -> Option<Self> {
            Some(Self {
                coefficients: rbj_low_pass_coefficients(sample_rate, cutoff_hz, q)?,
                delay: [0.0; 4],
            })
        }

        pub(super) fn process(&mut self, source: &[f32], target: &mut [f32]) -> usize {
            let frames = source.len().min(target.len());
            let [b0, b1, b2, a1, a2] = self.coefficients;
            let [mut x1, mut x2, mut y1, mut y2] = self.delay;
            for (input, output) in source[..frames].iter().zip(target[..frames].iter_mut()) {
                let x0 = f64::from(*input);
                let y0 = b0.mul_add(x0, b1.mul_add(x1, b2.mul_add(x2, a1.mul_add(y1, a2 * y2))));
                *output = y0.to_f32().unwrap_or(0.0);
                x2 = x1;
                x1 = x0;
                y2 = y1;
                y1 = y0;
            }
            self.delay = [x1, x2, y1, y2];
            frames
        }

        pub(super) fn reset(&mut self) {
            self.delay = [0.0; 4];
        }

        pub(super) fn retune(&mut self, sample_rate: f64, cutoff: f64, q: f64) -> bool {
            let Some(coefficients) = rbj_low_pass_coefficients(sample_rate, cutoff, q) else {
                return false;
            };
            self.coefficients = coefficients;
            true
        }
    }

    fn rbj_low_pass_coefficients(sample_rate: f64, cutoff_hz: f64, q: f64) -> Option<[f64; 5]> {
        if !sample_rate.is_finite() || !cutoff_hz.is_finite() || !q.is_finite() {
            return None;
        }
        if sample_rate <= 0.0 || cutoff_hz <= 0.0 || q <= 0.0 {
            return None;
        }
        let nyquist = sample_rate * 0.5;
        let cutoff = cutoff_hz.min(nyquist * 0.999);
        let omega = std::f64::consts::TAU * cutoff / sample_rate;
        let sin = omega.sin();
        let cos = omega.cos();
        let alpha = sin / (2.0 * q);
        let b0 = (1.0 - cos) * 0.5;
        let b1 = 1.0 - cos;
        let b2 = b0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha;
        Some([b0 / a0, b1 / a0, b2 / a0, -a1 / a0, -a2 / a0])
    }
}
