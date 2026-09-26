use std::num::NonZeroU32;

use audioadapter_buffers::direct::InterleavedSlice;
use bevy_platform::time::Instant;
use bon::Builder;
use firewheel::{
    ActivateInfo, FirewheelContext, backend::BackendProcessInfo, node::StreamStatus,
    processor::FirewheelProcessor,
};
use kithara_platform::time::Duration;

use super::{OfflineSessionError, task::consts::CHANNELS};

#[derive(Builder, Clone, Copy)]
#[builder(state_mod(vis = "pub(crate)"))]
pub(super) struct BackendConfig {
    pub(super) declared_latency: Duration,
    pub(super) block_frames: NonZeroU32,
    pub(super) sample_rate: NonZeroU32,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self::builder()
            .block_frames(NonZeroU32::MIN)
            .declared_latency(Duration::ZERO)
            .sample_rate(NonZeroU32::MIN)
            .build()
    }
}

/// The offline stream. There is no device behind it: the renderer drives the
/// processor itself, one requested block at a time, so a caller pulls audio at
/// whatever pace it likes instead of a sound card setting it.
pub(super) struct OfflineStream {
    processor: FirewheelProcessor,
    sample_rate: NonZeroU32,
}

impl OfflineStream {
    pub(super) fn render(
        &mut self,
        position: u64,
        frames: usize,
        output: &mut [f32],
    ) -> Result<(), OfflineSessionError> {
        let rate = u64::from(self.sample_rate.get());
        let whole_seconds = position / rate;
        let remainder =
            u32::try_from(position % rate).map_err(|_| OfflineSessionError::TimelineOverflow)?;
        let process_info = BackendProcessInfo {
            frames,
            process_timestamp: Some(Instant::now()),
            duration_since_stream_start: Duration::from_secs(whole_seconds)
                + Duration::from_secs_f64(f64::from(remainder) / f64::from(self.sample_rate.get())),
            input_stream_status: StreamStatus::empty(),
            output_stream_status: StreamStatus::empty(),
            dropped_frames: 0,
            process_to_playback_delay: None,
        };
        let input = InterleavedSlice::new(&[] as &[f32], 0, 0)
            .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
        let mut output = InterleavedSlice::new_mut(output, CHANNELS, frames)
            .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
        self.processor.process(&input, &mut output, process_info);
        Ok(())
    }

    /// Activates `cx` for offline rendering and takes ownership of the
    /// processor it hands back.
    pub(super) fn start(
        cx: &mut FirewheelContext,
        config: BackendConfig,
    ) -> Result<Self, OfflineSessionError> {
        let num_stream_out_channels =
            u32::try_from(CHANNELS).map_err(|_| OfflineSessionError::ChannelCountOverflow)?;
        let processor = cx
            .activate(ActivateInfo {
                num_stream_out_channels,
                sample_rate: config.sample_rate,
                max_block_frames: config.block_frames,
                num_stream_in_channels: 0,
                input_to_output_latency_seconds: config.declared_latency.as_secs_f64(),
            })
            .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
        Ok(Self {
            processor,
            sample_rate: config.sample_rate,
        })
    }
}

#[cfg(test)]
mod tests {
    use firewheel::{FirewheelConfig, channel_config::ChannelCount};
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn start_hands_firewheel_the_configured_block_latency_and_rate() {
        let block_frames = NonZeroU32::new(127).expect("fixture block frames");
        let declared_latency = Duration::from_millis(7);
        let sample_rate = NonZeroU32::new(48_000).expect("fixture sample rate");
        let mut ctx = FirewheelContext::new(FirewheelConfig {
            num_graph_outputs: ChannelCount::STEREO,
            ..FirewheelConfig::default()
        });

        let config = BackendConfig::builder()
            .block_frames(block_frames)
            .declared_latency(declared_latency)
            .sample_rate(sample_rate)
            .build();
        let _stream = OfflineStream::start(&mut ctx, config).expect("fixture offline stream");

        let stream = ctx
            .stream_info()
            .expect("an activated context has a stream");
        assert_eq!(stream.max_block_frames, block_frames);
        assert_eq!(stream.sample_rate, sample_rate);
        assert_eq!(
            stream.input_to_output_latency_seconds,
            declared_latency.as_secs_f64()
        );
        assert_eq!(
            stream.num_stream_out_channels,
            u32::try_from(CHANNELS).expect("stereo channel count fits u32")
        );
    }

    #[kithara::test]
    fn render_fills_the_requested_block() {
        let block_frames = NonZeroU32::new(64).expect("fixture block frames");
        let sample_rate = NonZeroU32::new(48_000).expect("fixture sample rate");
        let mut ctx = FirewheelContext::new(FirewheelConfig {
            num_graph_outputs: ChannelCount::STEREO,
            ..FirewheelConfig::default()
        });
        let config = BackendConfig::builder()
            .block_frames(block_frames)
            .declared_latency(Duration::ZERO)
            .sample_rate(sample_rate)
            .build();
        let mut stream = OfflineStream::start(&mut ctx, config).expect("fixture offline stream");

        let frames = 16;
        let mut output = vec![f32::NAN; frames * CHANNELS];
        stream
            .render(0, frames, &mut output)
            .expect("the offline stream renders a block on demand");

        assert!(
            output.iter().all(|sample| sample.is_finite()),
            "every requested frame is written"
        );
    }
}
