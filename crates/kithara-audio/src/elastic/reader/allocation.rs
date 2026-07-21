use std::{collections::VecDeque, num::NonZeroU32};

use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_decode::PcmSpec;
use kithara_stretch::{ElasticConfig, ElasticRateEnvelope, ElasticSpanConfig, SignalsmithBackend};
use num_traits::ToPrimitive;

use super::{ElasticReader, Unprepared, sample_count};
use crate::elastic::{ElasticReaderConfig, ElasticReaderError};

impl ElasticReader<Unprepared> {
    /// Allocates every DSP and source-window buffer before preparation or rendering.
    /// # Errors
    /// Returns a typed format, configuration, buffer-budget, or DSP error.
    pub fn allocate(
        spec: PcmSpec,
        source_frame_count: u64,
        host_sample_rate: NonZeroU32,
        max_output_frames: NonZeroU32,
        output_channels: usize,
        config: ElasticReaderConfig,
        pool: &PcmPool,
    ) -> Result<Self, ElasticReaderError> {
        if spec.sample_rate != host_sample_rate {
            return Err(ElasticReaderError::FormatMismatch);
        }
        if let Some((field, _)) = [
            ("prefetch_blocks", config.prefetch_blocks()),
            (
                "relocation_prefetch_blocks",
                config.relocation_prefetch_blocks(),
            ),
            ("source_window_blocks", config.source_window_blocks()),
            ("ready_window_count", config.ready_window_count()),
            ("output_channels", output_channels),
        ]
        .into_iter()
        .find(|(_, value)| *value == 0)
        {
            return Err(ElasticReaderError::InvalidConfig { field });
        }
        let channels = usize::from(spec.channels);
        if channels == 0 || channels > output_channels {
            return Err(ElasticReaderError::UnsupportedChannelLayout);
        }
        let max_output_frames = usize::try_from(max_output_frames.get())
            .map_err(|_| ElasticReaderError::FrameOverflow)?;
        let rate =
            SignalsmithBackend::<ElasticConfig>::rate_envelope().max_source_frames_per_output();
        let max_source_frames = scaled_frames(max_output_frames, rate)?
            .checked_add(1)
            .ok_or(ElasticReaderError::FrameOverflow)?;
        let backend_config = ElasticConfig::try_from((
            host_sample_rate.get(),
            channels,
            max_source_frames,
            max_output_frames,
        ))?;
        let span_config = ElasticSpanConfig::try_from(config)?;
        let backend = SignalsmithBackend::prepare(backend_config)?;
        let capabilities = backend.capabilities();
        let latency = capabilities.latency();
        let max_warm_frames = capabilities.warmup_request(rate)?.source_frames();
        let source_buffer_frames = max_source_frames.max(max_warm_frames);
        let prefetch_frames = max_source_frames
            .checked_mul(config.prefetch_blocks())
            .ok_or(ElasticReaderError::FrameOverflow)?;
        let preparation_frames = latency
            .source_frames()
            .checked_add(max_warm_frames)
            .and_then(|frames| frames.checked_add(prefetch_frames))
            .ok_or(ElasticReaderError::FrameOverflow)?;
        let source_window_frames = max_source_frames
            .checked_mul(config.source_window_blocks())
            .ok_or(ElasticReaderError::FrameOverflow)?;
        let max_fetch_frames = preparation_frames.max(source_window_frames);
        let source_window_frames =
            u64::try_from(source_window_frames).map_err(|_| ElasticReaderError::FrameOverflow)?;
        let fetch_samples = sample_count(max_fetch_frames, channels)?;
        let window_buffers = (0..config.ready_window_count())
            .map(|_| prepared_buffer(pool, fetch_samples))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            config,
            span_config,
            output_channels,
            backend,
            max_warm_frames,
            max_source_frames,
            max_fetch_frames,
            source_frame_count,
            source_window_frames,
            window_buffers,
            state: Unprepared,
            sample_rate: host_sample_rate,
            ready_windows: VecDeque::with_capacity(config.ready_window_count()),
            fetch: prepared_buffer(pool, fetch_samples)?,
            history: prepared_buffer(pool, sample_count(latency.source_frames(), channels)?)?,
            source: prepared_buffer(pool, sample_count(source_buffer_frames, channels)?)?,
            output: prepared_buffer(pool, sample_count(max_output_frames, channels)?)?,
            relocation_buffer: Some(prepared_buffer(pool, fetch_samples)?),
            discarded: prepared_buffer(pool, sample_count(latency.output_frames(), channels)?)?,
        })
    }

    /// Rate envelope available before allocating a concrete reader.
    #[must_use]
    pub const fn rate_envelope() -> ElasticRateEnvelope {
        SignalsmithBackend::<ElasticConfig>::rate_envelope()
    }
}

fn prepared_buffer(pool: &PcmPool, samples: usize) -> Result<PcmBuf, ElasticReaderError> {
    let mut buffer = pool.get();
    buffer.ensure_len(samples)?;
    Ok(buffer)
}

fn scaled_frames(frames: usize, rate: f64) -> Result<usize, ElasticReaderError> {
    (frames.to_f64().ok_or(ElasticReaderError::FrameOverflow)? * rate)
        .ceil()
        .to_usize()
        .ok_or(ElasticReaderError::FrameOverflow)
}
