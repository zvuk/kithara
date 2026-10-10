use std::{marker::PhantomData, num::NonZeroU32};

use futures::future::Either;
use kithara_audio::ResamplerBackend;
use kithara_bufpool::HasPool;
use kithara_command::Inbox;
use kithara_decode::DecodeError;
use kithara_events::EventBus;
use kithara_platform::{maybe_send::BoxFuture, sync::Arc, time::Duration};
use kithara_render::{LaneProtocol, LaneStart, LoadRefusal, PcmReceiver, TrackConfig};
use kithara_signal::FrameCount;
use kithara_stream::StreamType;

use super::{
    super::{PlaybackResamplerBackend, ResourceConfig, ResourceLane, SourceType},
    core::*,
};
use crate::PlayWorker;

type ResourceTrack<S> = Either<
    TrackConfig<kithara_file::File<S>, PlaybackResamplerBackend>,
    TrackConfig<kithara_hls::Hls<S>, PlaybackResamplerBackend>,
>;
type OpenedLane<L> = Result<(PcmReceiver, L, FrameCount), LoadRefusal>;

pub fn resource_tracks<S>(
    config: &ResourceConfig<S>,
) -> Result<(PlayWorker<S>, ResourceTrack<S>), LoadRefusal>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let worker = config.worker.clone().ok_or(DecodeError::InvalidData {
        detail: "ResourceConfig requires an explicit PlayWorker",
    })?;
    let track = match SourceType::detect(&config.src)? {
        SourceType::RemoteFile(_) | SourceType::LocalFile(_) => {
            let audio = config.clone().build_file_config(&worker, None);
            Either::Left(config.build_track_config(audio))
        }
        SourceType::HlsStream(_) => {
            let audio = config.clone().build_hls_config(&worker, None)?;
            Either::Right(config.build_track_config(audio))
        }
    };
    Ok((worker, track))
}

/// Opens an explicit source configuration through the real dispatcher and lane.
pub fn track_load<T, B, S, L, F>(
    config: TrackConfig<T, B>,
    src: Arc<str>,
    worker: PlayWorker<S>,
    sample_rate: NonZeroU32,
    open: F,
) -> ResourceLoad<S>
where
    T: StreamType<Events = EventBus> + 'static,
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    L: kithara_render::LaneTask + 'static,
    F: FnOnce(
            PlayWorker<S>,
            TrackConfig<T, B>,
            Duration,
            LaneStart,
            Inbox<LaneProtocol>,
        ) -> BoxFuture<'static, OpenedLane<L>>
        + Send
        + 'static,
{
    let geometry = geometry(config.warp(), config.audio_buffer_chunks())
        .map(|depth| (depth, config.declick_frames(sample_rate)));
    let channel_worker = worker.clone();
    ResourceLoad {
        opener: Box::new(move |position, start, inbox| {
            Box::pin(async move {
                let pools = worker.pools().clone();
                let cancel = config.audio().cancel().cloned();
                let (receiver, lane, latency) =
                    open(worker, config, position, start, inbox).await?;
                let opened = OpenedTrack::new(receiver, src, &pools).map_err(LoadRefusal::Pool)?;
                Ok((opened, ResourceLane::new(lane, cancel, None), latency))
            })
        }),
        channel: Some(Box::new(move || channel_worker.lane_channel())),
        cancel: None,
        geometry,
        marker: PhantomData,
    }
}
