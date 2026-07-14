use kithara_bufpool::PcmBuf;

use super::GlideConfig;
use crate::ResamplerMode;

pub(in crate::glide) struct RenderRequest<'a, 'out, 'channel> {
    pub(in crate::glide) input: &'a [&'a [f32]],
    pub(in crate::glide) previous: &'a [PcmBuf],
    pub(in crate::glide) output: &'out mut [&'channel mut [f32]],
    pub(in crate::glide) produced: usize,
    pub(in crate::glide) config: GlideConfig,
    pub(in crate::glide) filter_ratio: f64,
    pub(in crate::glide) mode: ResamplerMode,
}

#[cfg(all(
    feature = "apple-accelerate",
    any(target_os = "macos", target_os = "ios")
))]
mod imp {
    use std::num::NonZeroUsize;

    use kithara_apple::accelerate::{
        BiquadFilter, copy_f32, linear_interpolate_f32, quadratic_interpolate_f32,
    };
    use kithara_bufpool::{PcmBuf, PcmPool};
    use num_traits::cast::ToPrimitive;
    use smallvec::SmallVec;

    use super::{super::GlideInterpolation, RenderRequest};
    use crate::{ResamplerBuildError, ResamplerError, ResamplerMode};

    struct FilterParams {
        cutoff_to_nyquist: f64,
        low_pass_q: f64,
    }

    const FILTER_PARAMS: FilterParams = FilterParams {
        cutoff_to_nyquist: 0.9,
        low_pass_q: std::f64::consts::FRAC_1_SQRT_2,
    };

    #[derive(fieldwork::Fieldwork)]
    pub(in crate::glide) struct GlideEngine {
        positions: PcmBuf,
        padded: SmallVec<[PcmBuf; 8]>,
        filtered: SmallVec<[PcmBuf; 8]>,
        filters: SmallVec<[Option<BiquadFilter>; 8]>,
        max_input_frames: usize,
        #[field(get(copy, name = position_capacity, vis = "pub(in crate::glide)"))]
        max_output_frames: usize,
        filter_cutoff: Option<f64>,
    }

    impl GlideEngine {
        pub(in crate::glide) fn new(
            pool: &PcmPool,
            channels: NonZeroUsize,
            max_input_frames: usize,
            max_ratio_adjustment: f64,
            backend: &'static str,
        ) -> Result<Self, ResamplerBuildError> {
            let max_output_frames = max_output_frames(max_input_frames, max_ratio_adjustment);
            let mut positions = pool.get();
            ensure_build_len(&mut positions, max_output_frames, backend)?;
            let mut padded = SmallVec::new();
            let mut filtered = SmallVec::new();
            let mut filters = SmallVec::new();
            for _ in 0..channels.get() {
                let mut padded_channel = pool.get();
                ensure_build_len(
                    &mut padded_channel,
                    max_input_frames.saturating_add(2),
                    backend,
                )?;
                padded.push(padded_channel);

                let mut filtered_channel = pool.get();
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
                *filter = BiquadFilter::low_pass(sample_rate, cutoff, FILTER_PARAMS.low_pass_q);
                if filter.is_none() {
                    return Err(ResamplerError::Backend {
                        op: "glide accelerate filter",
                        detail: "failed to create vDSP low-pass filter".into(),
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
                    op: "glide accelerate positions",
                    detail: "output frame request exceeds preallocated position buffer".into(),
                });
            }
            Ok(&mut self.positions[..frames])
        }

        pub(in crate::glide) fn render(
            &mut self,
            request: RenderRequest<'_, '_, '_>,
        ) -> Result<(), ResamplerError> {
            let RenderRequest {
                input,
                previous,
                output,
                produced,
                config,
                filter_ratio,
                mode,
            } = request;
            let input_frames = input.first().map_or(0, |channel| channel.len());
            if input_frames > self.max_input_frames {
                return Err(ResamplerError::Backend {
                    op: "glide accelerate input",
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

            for (channel_idx, source) in input.iter().enumerate() {
                let padded = &mut self.padded[channel_idx];
                padded[0] = previous[channel_idx][0];
                copy_f32(source, &mut padded[1..input_frames.saturating_add(1)]);
                padded[input_frames.saturating_add(1)] = 0.0;

                let source = if cutoff.is_some() {
                    let filtered = &mut self.filtered[channel_idx];
                    let filter = self.filters[channel_idx].as_mut().ok_or_else(|| {
                        ResamplerError::Backend {
                            op: "glide accelerate filter",
                            detail: "anti-alias filter was not initialized".into(),
                        }
                    })?;
                    filter.process(&padded[..input_frames.saturating_add(2)], filtered);
                    &filtered[..input_frames.saturating_add(2)]
                } else {
                    &padded[..input_frames.saturating_add(2)]
                };

                let positions = &self.positions[..produced];
                match config.interpolation {
                    GlideInterpolation::Linear => {
                        linear_interpolate_f32(
                            source,
                            positions,
                            &mut output[channel_idx][..produced],
                        );
                    }
                    GlideInterpolation::Quadratic => {
                        quadratic_interpolate_f32(
                            source,
                            positions,
                            &mut output[channel_idx][..produced],
                        );
                    }
                }
            }
            Ok(())
        }

        pub(in crate::glide) fn reset(&mut self) {
            for filter in &mut self.filters {
                if let Some(filter) = filter.as_mut() {
                    filter.reset();
                }
            }
        }
    }

    fn ensure_build_len(
        buffer: &mut PcmBuf,
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
        FILTER_PARAMS.cutoff_to_nyquist * sample_rate / (2.0 * ratio.max(1.0))
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
}

#[cfg(not(all(
    feature = "apple-accelerate",
    any(target_os = "macos", target_os = "ios")
)))]
mod imp {
    use std::num::NonZeroUsize;

    use kithara_bufpool::{PcmBuf, PcmPool};
    use num_traits::cast::ToPrimitive;
    use smallvec::SmallVec;

    use super::{super::GlideInterpolation, RenderRequest};
    use crate::{ResamplerBuildError, ResamplerError, ResamplerMode};

    struct FilterParams {
        cutoff_to_nyquist: f64,
        low_pass_q: f64,
    }

    const FILTER_PARAMS: FilterParams = FilterParams {
        cutoff_to_nyquist: 0.9,
        low_pass_q: std::f64::consts::FRAC_1_SQRT_2,
    };

    #[derive(fieldwork::Fieldwork)]
    pub(in crate::glide) struct GlideEngine {
        positions: PcmBuf,
        padded: SmallVec<[PcmBuf; 8]>,
        filtered: SmallVec<[PcmBuf; 8]>,
        filters: SmallVec<[Option<ScalarBiquad>; 8]>,
        max_input_frames: usize,
        #[field(get(copy, name = position_capacity, vis = "pub(in crate::glide)"))]
        max_output_frames: usize,
        filter_cutoff: Option<f64>,
    }

    impl GlideEngine {
        pub(in crate::glide) fn new(
            pool: &PcmPool,
            channels: NonZeroUsize,
            max_input_frames: usize,
            max_ratio_adjustment: f64,
            backend: &'static str,
        ) -> Result<Self, ResamplerBuildError> {
            let max_output_frames = max_output_frames(max_input_frames, max_ratio_adjustment);
            let mut positions = pool.get();
            ensure_build_len(&mut positions, max_output_frames, backend)?;
            let mut padded = SmallVec::new();
            let mut filtered = SmallVec::new();
            let mut filters = SmallVec::new();
            for _ in 0..channels.get() {
                let mut padded_channel = pool.get();
                ensure_build_len(
                    &mut padded_channel,
                    max_input_frames.saturating_add(2),
                    backend,
                )?;
                padded.push(padded_channel);

                let mut filtered_channel = pool.get();
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
                *filter = ScalarBiquad::low_pass(sample_rate, cutoff, FILTER_PARAMS.low_pass_q);
                if filter.is_none() {
                    return Err(ResamplerError::Backend {
                        op: "glide scalar filter",
                        detail: "failed to create low-pass filter".into(),
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
                    op: "glide scalar positions",
                    detail: "output frame request exceeds preallocated position buffer".into(),
                });
            }
            Ok(&mut self.positions[..frames])
        }

        pub(in crate::glide) fn render(
            &mut self,
            request: RenderRequest<'_, '_, '_>,
        ) -> Result<(), ResamplerError> {
            let RenderRequest {
                input,
                previous,
                output,
                produced,
                config,
                filter_ratio,
                mode,
            } = request;
            let input_frames = input.first().map_or(0, |channel| channel.len());
            if input_frames > self.max_input_frames {
                return Err(ResamplerError::Backend {
                    op: "glide scalar input",
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

            for (channel_idx, source) in input.iter().enumerate() {
                let padded = &mut self.padded[channel_idx];
                padded[0] = previous[channel_idx][0];
                padded[1..input_frames.saturating_add(1)].copy_from_slice(source);
                padded[input_frames.saturating_add(1)] = 0.0;

                let source = if cutoff.is_some() {
                    let filtered = &mut self.filtered[channel_idx];
                    let filter = self.filters[channel_idx].as_mut().ok_or_else(|| {
                        ResamplerError::Backend {
                            op: "glide scalar filter",
                            detail: "anti-alias filter was not initialized".into(),
                        }
                    })?;
                    filter.process(&padded[..input_frames.saturating_add(2)], filtered);
                    &filtered[..input_frames.saturating_add(2)]
                } else {
                    &padded[..input_frames.saturating_add(2)]
                };

                let positions = &self.positions[..produced];
                match config.interpolation {
                    GlideInterpolation::Linear => {
                        interpolate::<LinearInterpolation>(
                            source,
                            positions,
                            &mut output[channel_idx][..produced],
                        );
                    }
                    GlideInterpolation::Quadratic => {
                        interpolate::<QuadraticInterpolation>(
                            source,
                            positions,
                            &mut output[channel_idx][..produced],
                        );
                    }
                }
            }
            Ok(())
        }

        pub(in crate::glide) fn reset(&mut self) {
            for filter in &mut self.filters {
                if let Some(filter) = filter.as_mut() {
                    filter.reset();
                }
            }
        }
    }

    struct ScalarBiquad {
        coefficients: [f64; 5],
        delay: [f64; 4],
    }

    impl ScalarBiquad {
        fn low_pass(sample_rate: f64, cutoff_hz: f64, q: f64) -> Option<Self> {
            Some(Self {
                coefficients: rbj_low_pass_coefficients(sample_rate, cutoff_hz, q)?,
                delay: [0.0; 4],
            })
        }

        fn process(&mut self, source: &[f32], target: &mut [f32]) -> usize {
            let frames = source.len().min(target.len());
            let [b0, b1, b2, a1, a2] = self.coefficients;
            let [mut x1, mut x2, mut y1, mut y2] = self.delay;
            for (input, output) in source.iter().zip(target.iter_mut()).take(frames) {
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

        fn reset(&mut self) {
            self.delay = [0.0; 4];
        }
    }

    fn ensure_build_len(
        buffer: &mut PcmBuf,
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

    fn interpolate<I>(source: &[f32], positions: &[f32], target: &mut [f32])
    where
        I: Interpolation,
    {
        for (position, output) in positions.iter().zip(target.iter_mut()) {
            let base = position.floor().to_usize().unwrap_or(usize::MAX);
            let frac = position - base.to_f32().unwrap_or(0.0);
            *output = I::sample(source, base, frac);
        }
    }

    fn low_pass_cutoff(sample_rate: f64, ratio: f64) -> f64 {
        FILTER_PARAMS.cutoff_to_nyquist * sample_rate / (2.0 * ratio.max(1.0))
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

pub(super) use imp::GlideEngine;
