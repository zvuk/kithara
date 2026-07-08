use std::num::NonZeroUsize;

use kithara_bufpool::{PcmBuf, PcmPool};
use num_traits::cast::ToPrimitive;
use smallvec::SmallVec;

use super::{GlideConfig, interpolator};
use crate::{
    RatioGlide, Resampler, ResamplerBuildError, ResamplerCapabilities, ResamplerControl,
    ResamplerError, ResamplerMode, ResamplerOptions, ResamplerProcess, ResamplerSettings,
};

pub(super) struct GlideResampler {
    channels: NonZeroUsize,
    config: GlideConfig,
    current_ratio: f64,
    glide: GlideState,
    input_frames: usize,
    mode: ResamplerMode,
    options: ResamplerOptions,
    previous: SmallVec<[PcmBuf; 8]>,
    cursor: f64,
}

impl GlideResampler {
    pub(super) fn new(
        backend: &'static str,
        config: GlideConfig,
        settings: &ResamplerSettings,
    ) -> Result<Self, ResamplerBuildError> {
        let ratio = initial_ratio(settings.mode);
        validate_ratio_bounds(backend, settings.options, ratio)?;
        let glide = initial_glide(backend, settings.mode, settings.options, ratio)?;
        let previous = previous_buffers(&settings.pcm_pool, settings.channels, backend)?;
        Ok(Self {
            config,
            glide,
            previous,
            channels: settings.channels,
            current_ratio: ratio,
            input_frames: settings.options.chunk_size,
            mode: settings.mode,
            options: settings.options,
            cursor: 0.0,
        })
    }

    fn advance_glide(&mut self) {
        if self.glide.remaining == 0 {
            return;
        }
        self.current_ratio += self.glide.step;
        self.glide.remaining = self.glide.remaining.saturating_sub(1);
        if self.glide.remaining == 0 {
            self.current_ratio = self.glide.target;
        }
    }

    fn can_passthrough(&self) -> bool {
        self.glide.remaining == 0
            && (self.current_ratio - 1.0).abs() <= self.options.passthrough_tolerance
    }

    fn consume_frames(&mut self, input: &[&[f32]], input_frames: usize, produced: usize) -> usize {
        if self.can_passthrough() {
            self.store_previous(input, produced);
            return produced;
        }
        let consumed = self
            .cursor
            .floor()
            .to_usize()
            .unwrap_or(usize::MAX)
            .min(input_frames.saturating_sub(1));
        self.store_previous(input, consumed);
        self.cursor -= consumed.to_f64().unwrap_or(0.0);
        consumed
    }

    fn output_ratio(&self) -> f64 {
        if self.glide.remaining > 0 {
            self.current_ratio.min(self.glide.target)
        } else {
            self.current_ratio
        }
    }

    fn render_interpolated(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
        input_frames: usize,
        output_capacity: usize,
    ) -> usize {
        let mut produced = 0;
        while produced < output_capacity && can_sample(self.cursor, input_frames) {
            let ratio = self.current_ratio;
            for channel in 0..self.channels.get() {
                output[channel][produced] = interpolator::interpolate(
                    input[channel],
                    self.previous[channel][0],
                    self.cursor,
                    ratio,
                    self.config.interpolation,
                    self.config.anti_alias,
                );
            }
            self.cursor += ratio;
            self.advance_glide();
            produced += 1;
        }
        produced
    }

    fn render_passthrough(&self, input: &[&[f32]], output: &mut [&mut [f32]], frames: usize) {
        for channel in 0..self.channels.get() {
            output[channel][..frames].copy_from_slice(&input[channel][..frames]);
        }
    }

    fn store_previous(&mut self, input: &[&[f32]], consumed: usize) {
        if consumed == 0 {
            return;
        }
        let frame = consumed.saturating_sub(1);
        self.previous
            .iter_mut()
            .zip(input.iter())
            .take(self.channels.get())
            .for_each(|(previous, input)| {
                previous[0] = input[frame];
            });
    }
}

impl Resampler for GlideResampler {
    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO
            | ResamplerCapabilities::VARIABLE_RATIO
            | ResamplerCapabilities::RATIO_GLIDE
            | ResamplerCapabilities::REALTIME_SAFE
            | ResamplerCapabilities::STANDALONE
    }

    fn channels(&self) -> NonZeroUsize {
        self.channels
    }

    fn control_mut(&mut self) -> Option<&mut dyn ResamplerControl> {
        Some(self)
    }

    fn input_frames_max(&self) -> usize {
        self.input_frames
    }

    fn input_frames_next(&self) -> usize {
        self.input_frames
    }

    fn mode(&self) -> ResamplerMode {
        self.mode
    }

    fn output_frames_for_input(&self, input_frames: usize) -> usize {
        frames_for_ratio(input_frames, self.output_ratio())
    }

    fn output_frames_max(&self) -> usize {
        self.output_frames_next()
    }

    fn output_frames_next(&self) -> usize {
        self.output_frames_for_input(self.input_frames)
            .saturating_add(2)
    }

    fn process_into_buffer(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, ResamplerError> {
        let input_frames = validate_input(input, self.channels.get())?;
        let output_capacity = validate_output(output, self.channels.get())?;
        if input_frames == 0 || output_capacity == 0 {
            return Ok(ResamplerProcess::new(0, 0));
        }

        let produced = if self.can_passthrough() {
            let frames = input_frames.min(output_capacity);
            self.render_passthrough(input, output, frames);
            frames
        } else {
            self.render_interpolated(input, output, input_frames, output_capacity)
        };
        let consumed = self.consume_frames(input, input_frames, produced);
        Ok(ResamplerProcess::new(consumed, produced))
    }

    fn reset(&mut self) {
        self.current_ratio = initial_ratio(self.mode);
        self.glide = GlideState::default();
        for previous in &mut self.previous {
            previous[0] = 0.0;
        }
        self.cursor = 0.0;
    }
}

impl ResamplerControl for GlideResampler {
    fn glide_ratio(&mut self, glide: RatioGlide) -> Result<(), ResamplerError> {
        validate_runtime_ratio(self.options, glide.target_ratio)?;
        let frames = glide.frames.get();
        let frames_f64 = f64::from(frames);
        self.glide = GlideState {
            remaining: frames,
            step: (glide.target_ratio - self.current_ratio) / frames_f64,
            target: glide.target_ratio,
        };
        Ok(())
    }

    fn set_ratio(&mut self, ratio: f64) -> Result<(), ResamplerError> {
        validate_runtime_ratio(self.options, ratio)?;
        self.current_ratio = ratio;
        self.glide = GlideState::default();
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct GlideState {
    remaining: u32,
    step: f64,
    target: f64,
}

fn can_sample(cursor: f64, input_frames: usize) -> bool {
    if input_frames < 2 {
        return false;
    }
    let Some(base) = cursor.floor().to_usize() else {
        return false;
    };
    base.saturating_add(1) < input_frames
}

fn frames_for_ratio(input_frames: usize, ratio: f64) -> usize {
    let Some(input_frames) = input_frames.to_f64() else {
        return usize::MAX;
    };
    let frames = (input_frames / ratio).ceil();
    if !frames.is_finite() || frames <= 0.0 {
        return 0;
    }
    frames.to_usize().unwrap_or(usize::MAX)
}

fn initial_glide(
    backend: &'static str,
    mode: ResamplerMode,
    options: ResamplerOptions,
    initial_ratio: f64,
) -> Result<GlideState, ResamplerBuildError> {
    let ResamplerMode::VariableRatio {
        glide: Some(glide), ..
    } = mode
    else {
        return Ok(GlideState::default());
    };
    validate_ratio_bounds(backend, options, glide.target_ratio)?;
    let frames = glide.frames.get();
    Ok(GlideState {
        remaining: frames,
        step: (glide.target_ratio - initial_ratio) / f64::from(frames),
        target: glide.target_ratio,
    })
}

fn initial_ratio(mode: ResamplerMode) -> f64 {
    match mode {
        ResamplerMode::FixedRatio {
            source_sample_rate,
            target_sample_rate,
        } => f64::from(source_sample_rate.get()) / f64::from(target_sample_rate.get()),
        ResamplerMode::VariableRatio { initial_ratio, .. } => initial_ratio,
    }
}

fn previous_buffers(
    pool: &PcmPool,
    channels: NonZeroUsize,
    backend: &'static str,
) -> Result<SmallVec<[PcmBuf; 8]>, ResamplerBuildError> {
    let mut buffers = SmallVec::new();
    for _ in 0..channels.get() {
        let mut buffer = pool.get();
        buffer
            .ensure_len(1)
            .map_err(|err| ResamplerBuildError::BackendBuild {
                backend,
                detail: err.to_string(),
            })?;
        buffer[0] = 0.0;
        buffers.push(buffer);
    }
    Ok(buffers)
}

fn validate_input(input: &[&[f32]], channels: usize) -> Result<usize, ResamplerError> {
    if input.len() < channels {
        return Err(ResamplerError::InvalidBuffer {
            detail: "not enough input channels for glide resampler",
        });
    }
    let frames = input[0].len();
    if input
        .iter()
        .take(channels)
        .any(|channel| channel.len() != frames)
    {
        return Err(ResamplerError::InvalidBuffer {
            detail: "input channels must have equal frame counts",
        });
    }
    Ok(frames)
}

fn validate_output(output: &[&mut [f32]], channels: usize) -> Result<usize, ResamplerError> {
    if output.len() < channels {
        return Err(ResamplerError::InvalidBuffer {
            detail: "not enough output channels for glide resampler",
        });
    }
    let frames = output[0].len();
    if output
        .iter()
        .take(channels)
        .any(|channel| channel.len() != frames)
    {
        return Err(ResamplerError::InvalidBuffer {
            detail: "output channels must have equal frame counts",
        });
    }
    Ok(frames)
}

fn validate_ratio_bounds(
    backend: &'static str,
    options: ResamplerOptions,
    ratio: f64,
) -> Result<(), ResamplerBuildError> {
    if !ratio.is_finite() || ratio <= 0.0 {
        return Err(ResamplerBuildError::InvalidRatio {
            ratio,
            resource: "glide",
        });
    }
    let min = 1.0 / options.max_ratio_adjustment;
    if ratio < min || ratio > options.max_ratio_adjustment {
        return Err(ResamplerBuildError::BackendBuild {
            backend,
            detail: "glide ratio exceeds configured max_ratio_adjustment".into(),
        });
    }
    Ok(())
}

fn validate_runtime_ratio(options: ResamplerOptions, ratio: f64) -> Result<(), ResamplerError> {
    if !ratio.is_finite() || ratio <= 0.0 {
        return Err(ResamplerError::Backend {
            op: "glide ratio",
            detail: "ratio must be finite and positive".into(),
        });
    }
    let min = 1.0 / options.max_ratio_adjustment;
    if ratio < min || ratio > options.max_ratio_adjustment {
        return Err(ResamplerError::Backend {
            op: "glide ratio",
            detail: "ratio exceeds configured max_ratio_adjustment".into(),
        });
    }
    Ok(())
}
