use std::num::NonZeroU32;

use audioadapter_buffers::direct::InterleavedSlice;
use bevy_platform::time::Instant;
use firewheel::{
    ActivateInfo, FirewheelContext, backend::BackendProcessInfo, node::StreamStatus,
    processor::FirewheelProcessor,
};
use kithara_config::Config;
use kithara_platform::time::Duration;
use kithara_signal::SessionFrame;

use super::{OfflineSessionError, task::consts::CHANNELS};
use crate::session::{
    state::DeckNode,
    transport::{OfflineInbox, TransportState},
};
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
        clock: SessionFrame,
        frames: usize,
        output: &mut [f32],
        decks: &mut [DeckNode],
    ) -> Result<(), OfflineSessionError> {
        InterleavedSlice::new_mut(output, CHANNELS, frames)
            .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
        let mut cursor = 0;
        while cursor < frames {
            let position = position
                .checked_add(
                    u64::try_from(cursor).map_err(|_| OfflineSessionError::TimelineOverflow)?,
                )
                .ok_or(OfflineSessionError::TimelineOverflow)?;
            let start = SessionFrame::new(
                i64::from(clock)
                    .checked_add(
                        i64::try_from(cursor).map_err(|_| OfflineSessionError::TimelineOverflow)?,
                    )
                    .ok_or(OfflineSessionError::TimelineOverflow)?,
            );
            let quantum = self.prepare_turn(start, frames - cursor, decks)?;
            self.render_turn(
                position,
                quantum,
                &mut output[cursor * CHANNELS..(cursor + quantum) * CHANNELS],
            )?;
            for deck in decks.iter_mut().filter_map(|deck| deck.offline.as_mut()) {
                deck.end_render()
                    .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
            }
            cursor += quantum;
        }
        if frames == 0 {
            self.render_turn(position, frames, output)?;
        }
        Ok(())
    }

    fn prepare_turn(
        &mut self,
        start: SessionFrame,
        frames: usize,
        decks: &mut [DeckNode],
    ) -> Result<usize, OfflineSessionError> {
        let mut quantum = frames;
        if let Some(inbox) = &mut self.inbox {
            inbox
                .drain()
                .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
            for deck in decks.iter_mut().filter_map(|deck| deck.offline.as_mut()) {
                quantum = quantum.min(
                    deck.prepare(inbox, start, quantum, self.sample_rate)
                        .map_err(|error| OfflineSessionError::Graph(error.to_string()))?,
                );
            }
        }
        for deck in decks.iter_mut().filter_map(|deck| deck.offline.as_mut()) {
            deck.begin_render(quantum)
                .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
        }
        Ok(quantum)
    }

    fn render_turn(
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

#[cfg(test)]
mod tests {
    use kithara_render::rt::OfflineDeckError;
    use kithara_test_utils::kithara;
    use kithara_warp::BeatGridId;

    use super::*;
    use crate::{
        HostOwner,
        mock::{ask, deck_snapshot, registration, test_state},
        session::{offline::mock::start, state::SessionStream},
    };

    #[kithara::test]
    fn an_offline_turn_returns_its_mixer_only_after_every_prepared_frame() {
        let mut state = test_state(start);
        let id = BeatGridId::allocate().expect("fixture deck id");
        ask(&mut state, registration(id)).expect("attach fixture deck");
        let session = &mut state.owner.session;
        let Some(SessionStream::Offline(stream)) = &mut session.stream else {
            panic!("fixture offline stream");
        };
        let deck = session.deck_nodes[0]
            .offline
            .as_mut()
            .expect("offline endpoint");
        assert!(matches!(
            deck.begin_render(32),
            Err(OfflineDeckError::InvalidFrames)
        ));
        let inbox = stream.inbox.as_mut().expect("session inbox");
        inbox.drain().expect("admit the fixture scope");
        assert!(matches!(
            deck.prepare(inbox, SessionFrame::new(0), 0, stream.sample_rate),
            Err(OfflineDeckError::InvalidFrames)
        ));
        let prepared = deck
            .prepare(inbox, SessionFrame::new(0), 32, stream.sample_rate)
            .expect("prepare the nonempty turn");
        assert_eq!(prepared, 32);
        assert!(matches!(
            deck.begin_render(33),
            Err(OfflineDeckError::InvalidFrames)
        ));
        deck.begin_render(32).expect("transfer the accepted turn");
        stream
            .render_turn(0, 16, &mut [0.0; 32])
            .expect("first graph subdivision");
        assert!(matches!(
            deck.end_render(),
            Err(OfflineDeckError::MissingMixer)
        ));
        stream
            .render_turn(16, 16, &mut [0.0; 32])
            .expect("second graph subdivision");
        deck.end_render()
            .expect("the complete turn returns its mixer");
        assert!(matches!(
            deck.begin_render(32),
            Err(OfflineDeckError::InvalidFrames)
        ));
        state.owner.begin_pass();
        state.owner.pass();
        assert_eq!(deck_snapshot(&mut state, id).blocks, 2);
    }
}
