use std::num::NonZeroU32;

use futures::future::Either;
use kithara_audio::{AudioConfig, ResamplerBackend};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_command::Inbox;
use kithara_events::EventBus;
use kithara_platform::{maybe_send::BoxFuture, sync::Arc, time::Duration};
use kithara_render::{LaneProtocol, LaneStart, LoadRefusal, OpenedPcm, TrackConfig};
use kithara_signal::FrameCount;
use kithara_stream::StreamType;

use super::{
    super::{PlaybackResamplerBackend, ResourceConfig, ResourceLane, SourceType},
    core::*,
};
use crate::PlayWorker;

type ResourceAudio<S> = Either<
    AudioConfig<kithara_file::File<S>, PlaybackResamplerBackend>,
    AudioConfig<kithara_hls::Hls<S>, PlaybackResamplerBackend>,
>;
type OpenedLane<L> = Result<(OpenedPcm, L, FrameCount), LoadRefusal>;

pub fn resource_audio<S>(
    config: &ResourceConfig<S>,
    pools: &PoolRegion<S>,
) -> Result<ResourceAudio<S>, LoadRefusal>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let track = match SourceType::detect(&config.src)? {
        SourceType::RemoteFile(_) | SourceType::LocalFile(_) => {
            let audio = config.clone().build_file_config(None);
            Either::Left(audio)
        }
        SourceType::HlsStream(_) => {
            let audio = config.clone().build_hls_config(pools.clone(), None)?;
            Either::Right(audio)
        }
    };
    Ok(track)
}

/// Opens an explicit source configuration through the real dispatcher and lane.
pub fn track_load<T, B, S, L, F>(
    (audio, config): (AudioConfig<T, B>, TrackConfig),
    src: Arc<str>,
    worker: PlayWorker<S>,
    sample_rate: NonZeroU32,
    open: F,
) -> ResourceLoad
where
    T: StreamType<Events = EventBus> + 'static,
    T::Config: Send,
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    L: kithara_render::LaneTask + 'static,
    F: FnOnce(
            PlayWorker<S>,
            AudioConfig<T, B>,
            TrackConfig,
            Duration,
            LaneStart,
            Inbox<LaneProtocol>,
        ) -> BoxFuture<'static, OpenedLane<L>>
        + Send
        + 'static,
{
    let geometry =
        geometry(config.warp(), config.audio_buffer_chunks()).map(|ring_depth| LaneGeometry {
            ring_depth,
            declick: config.declick_frames(sample_rate),
        });
    let channel_worker = worker.clone();
    ResourceLoad {
        opener: Box::new(move |position, start, inbox| {
            Box::pin(async move {
                let cancel = audio.cancel().cloned();
                let (opened, lane, latency) =
                    open(worker, audio, config, position, start, inbox).await?;
                let opened = OpenedTrack::new(opened, src);
                Ok((opened, ResourceLane::new(lane, cancel, None), latency))
            })
        }),
        channel: Box::new(move || channel_worker.lane_channel()),
        cancel: None,
        geometry,
    }
}
