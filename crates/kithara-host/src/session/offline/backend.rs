use std::num::NonZeroU32;

use audioadapter_buffers::direct::InterleavedSlice;
use bevy_platform::time::Instant;
use firewheel::{
    ActivateInfo, FirewheelContext, backend::BackendProcessInfo, node::StreamStatus,
    processor::FirewheelProcessor,
};
use kithara_config::Config;
use kithara_platform::time::Duration;

use super::{OfflineSessionError, task::consts::CHANNELS};
use crate::session::transport::{OfflineInbox, TransportState};
#[derive(Config, Clone, Copy)]
#[config(construction, builder(state_mod(vis = "pub(crate)")))]
pub(in crate::session::offline) struct BackendConfig {
    #[config(skip = "applied to the activated backend")]
    pub(in crate::session::offline) declared_latency: Duration,
    #[config(skip = "applied to the activated backend")]
    pub(in crate::session::offline) block_frames: NonZeroU32,
    #[config(skip = "transferred to the offline stream")]
    pub(in crate::session::offline) sample_rate: NonZeroU32,
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
pub(crate) struct OfflineStream {
    inbox: Option<OfflineInbox>,
    processor: Box<FirewheelProcessor>,
    sample_rate: NonZeroU32,
}

impl OfflineStream {
    pub(crate) fn render(
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
        if frames != 0
            && let Some(inbox) = &mut self.inbox
        {
            inbox
                .begin_render(frames)
                .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
        }
        self.processor.process(&input, &mut output, process_info);
        if frames != 0
            && let Some(inbox) = &mut self.inbox
        {
            inbox
                .end_render()
                .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
        }
        Ok(())
    }

    pub(crate) fn retire_closing(&mut self) -> Result<(), OfflineSessionError> {
        if let Some(inbox) = &mut self.inbox {
            inbox
                .retire_closing()
                .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
        }
        Ok(())
    }

    /// Activates `cx` for offline rendering and takes ownership of the
    /// processor it hands back.
    pub(in crate::session::offline) fn start(
        cx: &mut FirewheelContext,
        config: BackendConfig,
    ) -> Result<Self, OfflineSessionError> {
        let num_stream_out_channels =
            u32::try_from(CHANNELS).map_err(|_| OfflineSessionError::ChannelCountOverflow)?;
        let inbox = cx
            .proc_store_mut()
            .and_then(|store| store.try_get_mut::<TransportState>())
            .map(TransportState::park_offline_inbox)
            .transpose()
            .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
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
            inbox,
            processor: Box::new(processor),
            sample_rate: config.sample_rate,
        })
    }
}
