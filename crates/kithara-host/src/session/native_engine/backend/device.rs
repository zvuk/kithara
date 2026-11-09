use std::num::NonZeroU32;

use firewheel::{
    ActivateInfo,
    cpal::{
        CpalConfig,
        cpal::{
            self,
            traits::{DeviceTrait, HostTrait},
        },
    },
};

/// The selected device and its supported output format, before graph activation.
pub(in crate::session::native_engine) struct AcquiredOutput {
    pub(super) device: cpal::Device,
    pub(in crate::session::native_engine) config: cpal::StreamConfig,
    pub(super) format: cpal::SampleFormat,
    pub(super) conversion_samples: usize,
    pub(in crate::session::native_engine) info: ActivateInfo,
}

impl AcquiredOutput {
    pub(in crate::session::native_engine) fn new(request: CpalConfig) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no default output device")?;
        let default = device
            .default_output_config()
            .map_err(|error| error.to_string())?;
        let mut selected = default.clone();
        if let Some(rate) = request.output.desired_sample_rate {
            for candidate in device
                .supported_output_configs()
                .map_err(|error| error.to_string())?
            {
                if candidate.channels() == default.channels()
                    && candidate.sample_format() == default.sample_format()
                    && let Some(candidate) = candidate.try_with_sample_rate(rate)
                {
                    selected = candidate;
                    break;
                }
            }
        }
        let mut config = selected.config();
        #[cfg(not(target_os = "ios"))]
        if let Some(frames) = request.output.desired_block_frames
            && let cpal::SupportedBufferSize::Range { min, max } = *selected.buffer_size()
            && (min..=max).contains(&frames)
        {
            config.buffer_size = cpal::BufferSize::Fixed(frames);
        }
        let sample_rate = NonZeroU32::new(config.sample_rate).ok_or("zero output sample rate")?;
        if config.channels == 0 {
            return Err("zero output channels".into());
        }
        let channels = usize::from(config.channels);
        let conversion_samples = crate::consts::SESSION_OUTPUT_CONVERSION_SAMPLES
            .checked_div(channels)
            .and_then(|frames| frames.checked_mul(channels))
            .ok_or("output conversion geometry overflow")?;
        let max_block_frames = match config.buffer_size {
            cpal::BufferSize::Fixed(frames) => {
                NonZeroU32::new(frames).ok_or("zero output block")?
            }
            // A graph processing limit; a default-size callback may contain several blocks.
            cpal::BufferSize::Default => crate::consts::SESSION_OUTPUT_MAX_BLOCK_FRAMES,
        };
        Ok(Self {
            info: ActivateInfo {
                sample_rate,
                max_block_frames,
                num_stream_in_channels: 0,
                num_stream_out_channels: u32::from(config.channels),
                input_to_output_latency_seconds: 0.0,
            },
            format: selected.sample_format(),
            conversion_samples,
            device,
            config,
        })
    }
}
