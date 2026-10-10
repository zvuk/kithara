use std::{
    fmt::{self, Debug, Formatter},
    marker::PhantomData,
    num::{NonZeroU32, NonZeroUsize},
    pin::pin,
};

use delegate::delegate;
use futures::future::{Either, select};
use kithara_audio::{
    Audio, AudioObserver, AudioReadError, AudioReader, ChunkOutcome, ReadOutcome, ResamplerBackend,
    SeekOutcome,
};
use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_command::{Inbox, Sender};
use kithara_decode::{DecodeError, TrackMetadata};
use kithara_events::{EventBus, EventReceiver, EventSet};
use kithara_platform::{CancelToken, maybe_send::BoxFuture, sync::Arc, time::Duration};
use kithara_render::{
    LaneProtocol, LaneStart, LoadRefusal, Open, PcmReceiver,
    rt::{
        DeckMixerConfig,
        track::{PcmConsumer, PlayerResource},
    },
};
use kithara_signal::{AudioSpec, FrameCount};
use num_traits::ToPrimitive;
use tracing::warn;

use super::super::{PlaybackResamplerBackend, ResourceConfig, ResourceLane, SourceType};
use crate::PlayError;

/// A directly owned decoded reader used outside the deck's packet-ring path.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct Resource {
    #[field(get, deref = false)]
    src: Arc<str>,
    #[field(get = event_bus)]
    bus: EventBus,
    reader: Box<dyn AudioReader>,
}

impl Resource {
    /// Opens a decoded source for direct pulls by the dispatcher owning `wake`.
    ///
    /// # Errors
    /// Returns source detection, configuration, or decoder initialization failures.
    pub async fn open<S, B>(
        config: ResourceConfig<S, B>,
        wake: kithara_worker::Wake,
    ) -> Result<Self, DecodeError>
    where
        B: Default + ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        let src: Arc<str> = Arc::from(config.src.to_string());
        let source_type = SourceType::detect(&config.src)?;
        let worker = config.worker.clone().ok_or(DecodeError::InvalidData {
            detail: "ResourceConfig requires an explicit PlayWorker",
        })?;
        let pools = worker.pools().clone();
        let wake: Arc<dyn kithara_stream::WorkerWake> =
            Arc::new(kithara_render::StreamWake::new(wake));
        Ok(match source_type {
            SourceType::RemoteFile(_) | SourceType::LocalFile(_) => Self::from_reader(
                Audio::prepare(config.build_file_config(&worker, None), wake, pools).await?,
                Some(src),
            ),
            SourceType::HlsStream(_) => Self::from_reader(
                Audio::prepare(config.build_hls_config(&worker, None)?, wake, pools).await?,
                Some(src),
            ),
        })
    }

    /// Wraps a directly owned reader and prepares its first input off-RT.
    #[must_use]
    pub fn from_reader<R: AudioReader + 'static>(mut reader: R, src: Option<Arc<str>>) -> Self {
        let bus = reader.event_bus().clone();
        let src = src.unwrap_or_else(|| Arc::from("unknown"));
        if let Err(error) = reader.preload() {
            warn!(%src, %error, "resource preload failed");
        }
        Self {
            src,
            bus,
            reader: Box::new(reader),
        }
    }

    /// Prepares input on the reader's owning thread without a producer gate.
    ///
    /// # Errors
    /// Returns the reader's source or decoder failure.
    pub async fn preload(&mut self) -> Result<(), AudioReadError> {
        self.reader.preload()
    }

    /// Subscribe to unified source and decoder events.
    #[must_use]
    pub fn subscribe<E: EventSet>(&self) -> EventReceiver<E> {
        self.bus.subscribe()
    }

    delegate! {
        to self.reader {
            /// Adaptive bitrate control, when the source has one.
            #[must_use]
            pub fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            /// Source span already cached on disk.
            #[must_use]
            pub fn cached_span(&self) -> Duration;
            /// Source position through which input has been decoded.
            #[must_use]
            pub fn decoded_frontier(&self) -> Duration;
            /// Total source duration, when known.
            #[must_use]
            pub fn duration(&self) -> Option<Duration>;
            /// Tags captured from the source.
            #[must_use]
            pub fn metadata(&self) -> &TrackMetadata;
            /// Current committed source position.
            #[must_use]
            pub fn position(&self) -> Duration;
            /// Current decoded-audio format.
            #[must_use]
            pub fn spec(&self) -> AudioSpec;
            /// Read one decoded chunk with its metadata.
            pub fn next_chunk(&mut self) -> Result<ChunkOutcome, AudioReadError>;
            /// Read interleaved decoded samples.
            pub fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, AudioReadError>;
            /// Read deinterleaved decoded samples.
            pub fn read_planar<'a>(
                &mut self,
                output: &'a mut [&'a mut [f32]],
            ) -> Result<ReadOutcome, AudioReadError>;
            /// Seek synchronously on the source's owning thread.
            pub fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError>;
            /// Rebuild decoder resampling on the source's owning thread.
            pub fn set_host_sample_rate(&mut self, sample_rate: NonZeroU32);
        }
    }
}

impl Debug for Resource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Resource")
            .field("src", &self.src)
            .finish_non_exhaustive()
    }
}

type ResourceOpen = Result<(OpenedTrack, ResourceLane, FrameCount), LoadRefusal>;
type LaneChannel = (Sender<LaneProtocol>, Inbox<LaneProtocol>);
type ResourceOpener =
    dyn FnOnce(Duration, LaneStart, Inbox<LaneProtocol>) -> BoxFuture<'static, ResourceOpen> + Send;
type ResourceMarker<S, B> = fn() -> (S, B);

pub(super) fn geometry(
    warp: &kithara_warp::WarpConfig,
    audio_buffer_chunks: NonZeroUsize,
) -> Result<FrameCount, PlayError> {
    let source_block = warp.source_block_frames().get();
    let packet = warp
        .render_quantum_frames()
        .map_or(source_block, |quantum| quantum.get().min(source_block));
    audio_buffer_chunks
        .get()
        .checked_add(1)
        .and_then(|packets| packets.checked_mul(packet))
        .and_then(|frames| frames.checked_sub(1))
        .map(FrameCount::new)
        .ok_or_else(|| PlayError::Internal("lane ring frame depth overflow".into()))
}

/// A single-use source open and its preparation-owned lane wiring.
pub struct ResourceLoad<S, B = PlaybackResamplerBackend> {
    pub(super) opener: Box<ResourceOpener>,
    pub(super) channel: Option<Box<dyn Fn() -> LaneChannel + Send>>,
    pub(super) cancel: Option<CancelToken>,
    pub(super) geometry: Result<(FrameCount, FrameCount), PlayError>,
    pub(super) marker: PhantomData<ResourceMarker<S, B>>,
}

impl<S, B> Debug for ResourceLoad<S, B> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceLoad")
            .finish_non_exhaustive()
    }
}

impl<S, B> ResourceLoad<S, B> {
    /// Allocates the owner's lane sender before transferring the inbox in Load.
    pub(crate) fn lane_channel(
        &self,
    ) -> Result<(Sender<LaneProtocol>, Inbox<LaneProtocol>), PlayError> {
        let channel = self.channel.as_ref().ok_or_else(|| {
            PlayError::Internal("ResourceConfig requires an explicit PlayWorker".into())
        })?;
        Ok(channel())
    }

    /// Maximum rendered lead, including the held packet, and the lane's Jump ramp.
    pub(crate) fn lane_geometry(&self) -> Result<(FrameCount, FrameCount), PlayError> {
        self.geometry.clone()
    }
}

impl<S, B> ResourceLoad<S, B>
where
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Captures the configured source and decoder observer for one open.
    #[must_use]
    pub fn new(config: ResourceConfig<S, B>, observer: Box<dyn AudioObserver>) -> Self {
        let geometry = Self::config_geometry(&config);
        let channel = config.worker.clone().map(|worker| {
            Box::new(move || worker.lane_channel()) as Box<dyn Fn() -> LaneChannel + Send>
        });
        let cancel = config.cancel.clone();
        Self {
            opener: Box::new(move |position, start, inbox| {
                Box::pin(Self::load(config, observer, position, start, inbox))
            }),
            channel,
            cancel,
            geometry,
            marker: PhantomData,
        }
    }

    fn config_geometry(
        config: &ResourceConfig<S, B>,
    ) -> Result<(FrameCount, FrameCount), PlayError> {
        let worker = config.worker.as_ref().ok_or_else(|| {
            PlayError::Internal("ResourceConfig requires an explicit PlayWorker".into())
        })?;
        let audio = config.clone().build_file_config(worker, None);
        let track = config.build_track_config(audio);
        let ring_depth = geometry(track.warp(), track.audio_buffer_chunks())?;
        let rate = config.host_sample_rate.ok_or_else(|| {
            PlayError::Internal("lane geometry requires the prepared host sample rate".into())
        })?;
        let declick = (f64::from(rate.get())
            * f64::from(DeckMixerConfig::default().declick().smooth_seconds))
        .to_usize()
        .ok_or_else(|| PlayError::Internal("lane Jump ramp frame count overflow".into()))?
        .max(1);
        Ok((ring_depth, FrameCount::new(declick)))
    }

    async fn load(
        config: ResourceConfig<S, B>,
        observer: Box<dyn AudioObserver>,
        position: Duration,
        start: LaneStart,
        inbox: Inbox<LaneProtocol>,
    ) -> ResourceOpen {
        let src: Arc<str> = Arc::from(config.src.to_string());
        let source_type = SourceType::detect(&config.src)?;
        let worker = config.worker.clone().ok_or(DecodeError::InvalidData {
            detail: "ResourceConfig requires an explicit PlayWorker",
        })?;
        let cancel_link = config.cancel_link.clone();
        let cancel = config.cancel.clone();
        let (receiver, lane, latency) = match source_type {
            SourceType::RemoteFile(_) | SourceType::LocalFile(_) => {
                let audio = config.clone().build_file_config(&worker, Some(observer));
                let track = config.build_track_config(audio);
                let (receiver, lane, latency) = worker.load(track, position, start, inbox).await?;
                (
                    receiver,
                    ResourceLane::new(lane, cancel, cancel_link),
                    latency,
                )
            }
            SourceType::HlsStream(_) => {
                let audio = config.clone().build_hls_config(&worker, Some(observer))?;
                let track = config.build_track_config(audio);
                let (receiver, lane, latency) = worker.load(track, position, start, inbox).await?;
                (
                    receiver,
                    ResourceLane::new(lane, cancel, cancel_link),
                    latency,
                )
            }
        };
        Ok((
            OpenedTrack::new(receiver, src, worker.pools())?,
            lane,
            latency,
        ))
    }
}

/// The owner-facing receiver and source facts returned by a dispatcher load.
pub struct OpenedTrack {
    pub pcm: Box<PlayerResource>,
    pub duration: Option<Duration>,
    pub abr: Option<kithara_abr::AbrHandle>,
    pub metadata: TrackMetadata,
}

impl Debug for OpenedTrack {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenedTrack")
            .field("src", self.pcm.src())
            .field("duration", &self.duration)
            .finish_non_exhaustive()
    }
}

impl OpenedTrack {
    pub(super) fn new<S>(
        receiver: PcmReceiver,
        src: Arc<str>,
        pools: &PoolRegion<S>,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let duration = receiver.duration();
        let abr = receiver.abr_handle();
        let metadata = receiver.metadata().clone();
        let pcm = Box::new(PlayerResource::new(PcmConsumer::new(receiver), src, pools)?);
        Ok(Self {
            pcm,
            duration,
            abr,
            metadata,
        })
    }
}

impl<S, B> Open for ResourceLoad<S, B> {
    type Opened = OpenedTrack;
    type Lane = ResourceLane;

    /// Opens once with cancellation and concrete pool ownership captured at construction.
    async fn open(
        self,
        position: Duration,
        start: LaneStart,
        inbox: Inbox<LaneProtocol>,
    ) -> Result<(OpenedTrack, ResourceLane, FrameCount), LoadRefusal> {
        let open = (self.opener)(position, start, inbox);
        match self.cancel {
            None => open.await,
            Some(cancel) => match select(pin!(cancel.cancelled()), pin!(open)).await {
                Either::Left(((), _open)) => Err(LoadRefusal::Cancelled),
                Either::Right((opened, _cancel)) => opened,
            },
        }
    }
}

/// Transfer the directly owned reader to another off-RT consumer.
impl From<Resource> for Box<dyn AudioReader> {
    fn from(resource: Resource) -> Self {
        resource.reader
    }
}

#[cfg(test)]
mod tests {
    use kithara_warp::WarpCapabilities;

    /// Whether an available Warp rendering backend changes playback rate.
    #[must_use]
    const fn supports_playback_rate() -> bool {
        let backends = StretchKind::all();
        let mut index = 0;
        while index < backends.len() {
            if backends[index]
                .capabilities()
                .contains(WarpCapabilities::RATE)
            {
                return true;
            }
            index += 1;
        }
        false
    }

    use std::{
        num::{NonZeroU32, NonZeroUsize},
        sync::atomic::{AtomicU8, Ordering},
    };

    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_audio::{
        AudioControl, AudioObserverSlot, AudioRead, AudioSession, ReadOutcome, SeekOutcome,
    };
    use kithara_command::{Batch, Outcome, Seq, When};
    use kithara_decode::TrackMetadata;
    use kithara_platform::{CancelToken, sync::Arc};
    use kithara_render::{
        LaneCommand, LaneTask,
        bridge::{DeckPart, Fade, Slot, SlotState},
        rt::{DeckMixerConfig, StreamShape},
    };
    use kithara_signal::{AudioSpec, SegmentId, SessionFrame};
    use kithara_test_fixtures::play_fixtures::half;
    use kithara_test_utils::{TestTempDir, kithara};
    use kithara_warp::{SpeedCurve, StretchKind, WarpConfig};

    use super::*;
    use crate::{
        PlayWorker, PlayWorkerConfig, ResourceSrc, consts,
        test_pools::{TestPools, pools},
    };

    #[kithara::test(tokio)]
    async fn a_direct_resource_decodes_to_the_end_without_the_play_dispatcher() {
        let dir = TestTempDir::new();
        let path = dir.path().join("direct.wav");
        let spec = AudioSpec::new(2, crate::mock::SAMPLE_RATE);
        crate::mock::write_pcm_wav(&path, &vec![0.5; 4096 * 2], spec).expect("float WAV");
        let cancel = CancelToken::root();
        cancel.cancel();
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).cancel(cancel).build());
        let config: ResourceConfig<TestPools> = ResourceConfig::for_src(ResourceSrc::Path(path))
            .store(
                AssetStore::builder(pools())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .worker(worker)
            .build();
        let resource = Resource::open(config, kithara_worker::Wake::default())
            .await
            .expect("direct reader");
        let mut reader: Box<dyn AudioReader> = resource.into();
        assert_eq!(reader.spec().sample_rate, crate::mock::SAMPLE_RATE);
        let mut frames = 0;
        for _ in 0..8192 {
            match reader.next_chunk().expect("decoded chunk") {
                ChunkOutcome::Chunk(chunk) => frames += chunk.frames(),
                ChunkOutcome::Pending { .. } => kithara_platform::tokio::task::yield_now().await,
                ChunkOutcome::Eof { .. } => {
                    assert_eq!(frames, 4096);
                    return;
                }
            }
        }
        panic!("direct reader did not reach EOF");
    }

    #[kithara::test]
    #[case::configured(Some(64), 255)]
    #[case::uncapped(None, 1_023)]
    #[case::source_bound(Some(512), 1_023)]
    fn lane_geometry_bounds_the_held_packet_and_ring(
        #[case] quantum: Option<usize>,
        #[case] expected_frames: usize,
    ) {
        let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
            ResourceSrc::parse("https://example.com/song.wav").expect("valid source"),
        )
        .store(AssetStore::builder(pools()).build())
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .host_sample_rate(NonZeroU32::new(48_000).expect("nonzero rate"))
        .audio_buffer_chunks(NonZeroUsize::new(3).expect("nonzero ring"))
        .warp(
            WarpConfig::builder()
                .source_block_frames(NonZeroUsize::new(256).expect("nonzero block"))
                .maybe_render_quantum_frames(quantum.and_then(NonZeroUsize::new))
                .build(),
        )
        .build();
        let load = ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay()));

        let (ring_depth, _) = load.lane_geometry().expect("bounded lane geometry");

        assert_eq!(ring_depth, FrameCount::new(expected_frames));
    }

    #[kithara::test]
    #[case::packet_count(usize::MAX, 1)]
    #[case::frame_count(1, usize::MAX)]
    fn lane_geometry_refuses_overflow(#[case] chunks: usize, #[case] packet: usize) {
        let warp = WarpConfig::builder()
            .source_block_frames(NonZeroUsize::new(packet).expect("nonzero packet"))
            .build();

        assert!(matches!(
            geometry(&warp, NonZeroUsize::new(chunks).expect("nonzero ring")),
            Err(PlayError::Internal(detail)) if detail == "lane ring frame depth overflow"
        ));
    }

    #[kithara::test(tokio)]
    #[case::queue(true)]
    #[case::track(false)]
    async fn cancellation_precedes_in_flight_future_drop(#[case] cancel_queue: bool) {
        use futures::{channel::oneshot, future};

        struct Probe {
            cancel: CancelToken,
            state: Arc<AtomicU8>,
        }

        impl Drop for Probe {
            fn drop(&mut self) {
                self.state.store(
                    if self.cancel.is_cancelled() { 1 } else { 2 },
                    Ordering::SeqCst,
                );
            }
        }

        let owner = CancelToken::root();
        let queue_cancel = owner.child();
        let track_cancel = owner.child();
        let prep = crate::ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .cancel(queue_cancel.clone())
            .build();
        let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
            ResourceSrc::parse("https://example.com/song.mp3").expect("valid original source"),
        )
        .store(AssetStore::builder(pools()).build())
        .cancel(track_cancel.clone())
        .build();
        let prepared = prep
            .prepare(config, &crate::mock::output(None).get())
            .expect("unmeasured preparation");
        let cancel = prepared.cancel.clone().expect("per-track child");
        let mut load = ResourceLoad::new(prepared, Box::new(AudioObserverSlot::default().relay()));
        let source = load.opener;
        let state = Arc::new(AtomicU8::new(0));
        let probe = Probe {
            cancel,
            state: Arc::clone(&state),
        };
        let (started_tx, started_rx) = oneshot::channel();
        load.opener = Box::new(move |_, _, _| {
            Box::pin(async move {
                let _source = source;
                let _probe = probe;
                started_tx.send(()).expect("canceller waits for the future");
                future::pending::<()>().await;
                Err(LoadRefusal::Cancelled)
            })
        });
        let (_sender, inbox) = load.lane_channel().expect("configured lane");
        let opening = load.open(
            Duration::ZERO,
            crate::TrackSettings::default().lane_start(),
            inbox,
        );
        let canceller = async {
            started_rx.await.expect("the open started");
            if cancel_queue {
                queue_cancel.cancel();
            } else {
                track_cancel.cancel();
            }
        };
        let (opened, ()) = future::join(opening, canceller).await;

        assert!(matches!(opened, Err(LoadRefusal::Cancelled)));
        assert_eq!(state.load(Ordering::SeqCst), 1);
    }

    struct DropProbe {
        state: Arc<AtomicU8>,
        cancel: CancelToken,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let state = if self.cancel.is_cancelled() {
                consts::DROPPED_AFTER_CANCEL
            } else {
                consts::DROPPED_BEFORE_CANCEL
            };
            self.state.store(state, Ordering::SeqCst);
        }
    }

    struct EofReader {
        spec: AudioSpec,
        bus: EventBus,
        _drop_probe: Option<DropProbe>,
        meta: TrackMetadata,
        samples: Vec<f32>,
        position_frames: usize,
        total_frames: usize,
    }

    impl Default for EofReader {
        fn default() -> Self {
            Self {
                bus: EventBus::default(),
                meta: TrackMetadata::default(),
                spec: AudioSpec::new(
                    2,
                    NonZeroU32::new(consts::SAMPLE_RATE).expect("static rate"),
                ),
                position_frames: 0,
                total_frames: 0,
                samples: Vec::new(),
                _drop_probe: None,
            }
        }
    }

    impl EofReader {
        fn eof(&self) -> ReadOutcome {
            ReadOutcome::Eof {
                position: self.position_duration(),
            }
        }

        fn position_duration(&self) -> Duration {
            let frames = u32::try_from(self.position_frames).expect("test frame count fits u32");
            Duration::from_secs_f64(f64::from(frames) / f64::from(consts::SAMPLE_RATE))
        }

        fn take_frames(&mut self, capacity: usize) -> Option<NonZeroUsize> {
            let frames = capacity.min(self.total_frames - self.position_frames);
            self.position_frames += frames;
            NonZeroUsize::new(frames)
        }

        fn with_drop_probe(cancel: CancelToken, state: Arc<AtomicU8>) -> Self {
            Self {
                _drop_probe: Some(DropProbe { state, cancel }),
                ..Self::default()
            }
        }
    }

    impl AudioSession for EofReader {
        fn duration(&self) -> Option<Duration> {
            let frames = u32::try_from(self.total_frames).expect("test frame count fits u32");
            Some(Duration::from_secs_f64(
                f64::from(frames) / f64::from(consts::SAMPLE_RATE),
            ))
        }
        fn event_bus(&self) -> &EventBus {
            &self.bus
        }
        fn metadata(&self) -> &TrackMetadata {
            &self.meta
        }
    }

    impl AudioRead for EofReader {
        fn position(&self) -> Duration {
            self.position_duration()
        }
        fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, AudioReadError> {
            let Some(frames) = self.take_frames(buf.len() / 2) else {
                return Ok(self.eof());
            };
            let samples = frames.get() * 2;
            let end = self.position_frames * 2;
            buf[..samples].copy_from_slice(&self.samples[end - samples..end]);
            Ok(ReadOutcome::Frames {
                count: NonZeroUsize::new(samples).expect("non-zero stereo sample count"),
                position: self.position_duration(),
                source_span: None,
            })
        }
        fn read_planar<'a>(
            &mut self,
            output: &'a mut [&'a mut [f32]],
        ) -> Result<ReadOutcome, AudioReadError> {
            let capacity = output.first().map_or(0, |channel| channel.len());
            let Some(frames) = self.take_frames(capacity) else {
                return Ok(self.eof());
            };
            let start = self.position_frames - frames.get();
            for (index, channel) in output.iter_mut().enumerate() {
                for (offset, sample) in channel[..frames.get()].iter_mut().enumerate() {
                    *sample = self.samples[(start + offset) * 2 + index];
                }
            }
            Ok(ReadOutcome::Frames {
                count: frames,
                position: self.position_duration(),
                source_span: None,
            })
        }

        fn spec(&self) -> AudioSpec {
            self.spec
        }
    }

    impl AudioControl for EofReader {
        fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
            Ok(SeekOutcome::Landed {
                target: position,
                landed_at: position,
            })
        }
    }

    impl kithara_worker::Task for EofReader {
        fn tick(&mut self) -> kithara_worker::TickResult {
            kithara_worker::TickResult::Waiting
        }
    }

    impl LaneTask for EofReader {
        fn preload_status(&mut self) -> Result<bool, LoadRefusal> {
            Ok(true)
        }

        fn set_priority(&mut self, _class: kithara_render::ServiceClass) {}

        fn poll_commands(&mut self, _context: &mut std::task::Context<'_>) -> std::task::Poll<()> {
            std::task::Poll::Pending
        }
    }

    use crate::consts::{FILL_TICKS, RATE_RING_PACKETS};

    /// Drives `lane` as the lane dispatcher does, `poll_commands`, `recycle`, then
    /// `tick`, until it stops making progress.
    fn fill(lane: &mut impl LaneTask) {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        for _ in 0..FILL_TICKS {
            let _ = lane.poll_commands(&mut context);
            lane.recycle();
            if lane.tick() != kithara_worker::TickResult::Progress {
                return;
            }
        }
        panic!("the lane did not stop making progress within {FILL_TICKS} ticks");
    }

    async fn warped_player_resource(
        speed: f32,
        src: &str,
        samples: &[f32],
    ) -> (OpenedTrack, ResourceLane, Sender<LaneProtocol>, TestTempDir) {
        let dir = TestTempDir::new();
        let path = dir.path().join(format!("{src}.wav"));
        let sample_rate = NonZeroU32::new(consts::SAMPLE_RATE).expect("static rate");
        crate::mock::write_pcm_wav(&path, samples, AudioSpec::new(2, sample_rate))
            .expect("float WAV fixture");
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
        let warp = WarpConfig::builder().speed(speed).build();
        let load: ResourceLoad<TestPools> = ResourceLoad::new(
            ResourceConfig::for_src(ResourceSrc::Path(path))
                .store(AssetStore::builder(pools()).build())
                .worker(worker)
                .host_sample_rate(sample_rate)
                .audio_buffer_chunks(NonZeroUsize::new(RATE_RING_PACKETS).expect("nonzero ring"))
                .warp(warp)
                .build(),
            Box::new(AudioObserverSlot::default().relay()),
        );
        let (sender, inbox) = load.lane_channel().expect("explicit lane");
        let start = crate::TrackSettings::builder()
            .speed(speed)
            .build()
            .lane_start();
        let (opened, lane, _) = load
            .open(Duration::ZERO, start, inbox)
            .await
            .expect("URI opens");
        (opened, lane, sender, dir)
    }

    /// Host blocks a speed change may wait behind a full lane ring before it sounds.
    fn rate_change_window() -> usize {
        let ring_lead = geometry(
            &WarpConfig::builder().build(),
            NonZeroUsize::new(RATE_RING_PACKETS).expect("nonzero ring"),
        )
        .expect("bounded lane geometry");
        ring_lead.get() / consts::BLOCK_FRAMES + 2
    }

    fn rate_change_blocks(
        mixer: &mut crate::mock::MixerRig,
        lane: &mut ResourceLane,
        control: &mut Sender<LaneProtocol>,
        seq: Seq,
        built_frames: i64,
        applied_frames: i64,
    ) -> (SessionFrame, Vec<Seq>) {
        let block_frames =
            i64::try_from(consts::BLOCK_FRAMES).expect("block frames fit the session");
        let mut at = SessionFrame::new(block_frames);
        let mut applied_at = None;
        let mut receipts = Vec::new();
        let blocks = rate_change_window();
        let mut pcm = [[0.0; consts::BLOCK_FRAMES]; 2];
        for _ in 0..blocks {
            let before = mixer.ends.snapshot.read().slots[0].position;
            fill(lane);
            let [left, right] = &mut pcm;
            mixer
                .block(at, [left, right])
                .expect("rate-change host block");
            for receipt in control.receipts() {
                if let Outcome::Applied { at: lane_at, .. } = receipt.outcome() {
                    receipts.push(receipt.seq());
                    if receipt.seq() == seq {
                        applied_at = Some(*lane_at);
                    }
                }
            }
            let snapshot = mixer.ends.snapshot.read();
            let advance = snapshot.slots[0].position - before;
            let source_frames = (advance * f64::from(consts::SAMPLE_RATE))
                .round()
                .to_i64()
                .expect("fixture source advance fits i64");
            if !supports_playback_rate() {
                assert_eq!(
                    source_frames, block_frames,
                    "a fixed lane advances at unity speed"
                );
                return (at, receipts);
            }
            if let Some(lane_at) = applied_at {
                let mark = snapshot.slots[0]
                    .mark
                    .expect("a playing slot publishes its mark");
                assert_eq!(lane_at.segment, mark.lane.segment);
                let session_at = SessionFrame::new(
                    i64::from(mark.session)
                        + i64::try_from(lane_at.frame)
                            .expect("applied lane frame fits the session")
                        - i64::try_from(mark.lane.frame)
                            .expect("marked lane frame fits the session"),
                );
                if at + FrameCount::new(consts::BLOCK_FRAMES) <= session_at {
                    assert_eq!(
                        source_frames, built_frames,
                        "the lane lead keeps its built speed"
                    );
                }
                if at >= session_at {
                    assert_eq!(
                        source_frames, applied_frames,
                        "the observed speed matches the processor rate"
                    );
                    return (at, receipts);
                }
            } else {
                assert_eq!(
                    source_frames, built_frames,
                    "the lane has not applied its speed change"
                );
            }
            at = at + FrameCount::new(consts::BLOCK_FRAMES);
        }
        panic!("the SetSpeed frame was not reached within {blocks} host blocks");
    }

    #[kithara::test(native, tokio)]
    async fn playback_rate_reports_only_a_real_warp_control() {
        // Stereo, long enough to sound through the whole window at the fastest speed, 1.5.
        let samples = vec![0.5; (rate_change_window() + 1) * consts::BLOCK_FRAMES * 3 / 2 * 2];
        let mut fixed = Resource::from_reader(
            EofReader {
                samples: samples.clone(),
                total_frames: samples.len() / 2,
                ..EofReader::default()
            },
            None,
        );
        let mut buffer = vec![0.0; consts::BLOCK_FRAMES * 2];
        assert!(
            matches!(fixed.read(&mut buffer).expect("direct reader block"),
            ReadOutcome::Frames { count, .. } if count.get() == consts::BLOCK_FRAMES * 2)
        );
        let block_frames = u32::try_from(consts::BLOCK_FRAMES).expect("block size fits u32");
        let seconds = f64::from(block_frames) / f64::from(consts::SAMPLE_RATE);
        assert_eq!(fixed.position(), Duration::from_secs_f64(seconds));

        let pools = pools();
        let shape = StreamShape::new(
            NonZeroU32::new(u32::try_from(consts::BLOCK_FRAMES).expect("block size fits u32"))
                .expect("nonzero block"),
            NonZeroU32::new(consts::SAMPLE_RATE).expect("sample rate"),
        );
        let mut mixer = crate::mock::MixerRig::new(DeckMixerConfig::default(), shape, &pools)
            .expect("offline mixer");
        let (opened, mut lane, mut control, _dir) =
            warped_player_resource(1.25, "rate", &samples).await;
        assert_eq!(
            block_frames % 4,
            0,
            "chosen speeds advance whole source frames"
        );
        let (built_frames, applied_frames) = if supports_playback_rate() {
            (
                i64::from(block_frames) * 5 / 4,
                i64::from(block_frames) * 3 / 2,
            )
        } else {
            (i64::from(block_frames), i64::from(block_frames))
        };
        let slot = Slot::new(0);
        mixer
            .send(
                When::Next,
                DeckPart::Attach {
                    slot,
                    pcm: opened.pcm,
                    segment: SegmentId::FIRST,
                },
            )
            .expect("attach warped lane");
        mixer
            .send(
                When::Next,
                DeckPart::Start {
                    slot,
                    fade: Fade::Declick,
                },
            )
            .expect("start warped lane");
        fill(&mut lane);
        let mut pcm = [[0.0; consts::BLOCK_FRAMES]; 2];
        let [left, right] = &mut pcm;
        mixer
            .block(SessionFrame::new(0), [left, right])
            .expect("built-speed block");
        let before = mixer.ends.snapshot.read().slots[0].position;
        assert_eq!(
            (before * f64::from(consts::SAMPLE_RATE))
                .round()
                .to_i64()
                .expect("fixture source position fits i64"),
            built_frames
        );
        let seq = control
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![LaneCommand::SetSpeed(SpeedCurve::Constant(1.5))],
                },
            )
            .expect("rate command");
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let _ = lane.poll_commands(&mut context);
        let (_, receipts) = rate_change_blocks(
            &mut mixer,
            &mut lane,
            &mut control,
            seq,
            built_frames,
            applied_frames,
        );
        if supports_playback_rate() {
            assert_eq!(receipts, [seq]);
        } else {
            assert!(receipts.is_empty());
        }
        assert!(
            matches!(fixed.read(&mut buffer).expect("direct reader stays independent of warp commands"),
            ReadOutcome::Frames { count, .. } if count.get() == consts::BLOCK_FRAMES * 2)
        );
        assert_eq!(fixed.position(), Duration::from_secs_f64(seconds * 2.0));
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn a_loaded_track_takes_the_processor_rate(half: Vec<f32>) {
        let pools = pools();
        let effective_rate = if supports_playback_rate() { 1.5 } else { 1.0 };
        let shape = StreamShape {
            sample_rate: NonZeroU32::new(consts::SAMPLE_RATE).expect("static sample rate"),
            max_block_frames: NonZeroU32::new(
                u32::try_from(consts::BLOCK_FRAMES).expect("block size fits u32"),
            )
            .expect("static block size"),
        };
        let mut mixer = crate::mock::MixerRig::new(DeckMixerConfig::default(), shape, &pools)
            .expect("offline mixer");
        let first_slot = Slot::new(0);
        let next_slot = Slot::new(1);
        let (first, mut first_lane, mut first_control, _first_dir) =
            warped_player_resource(1.0, "first", &half).await;
        mixer
            .send(
                When::Next,
                DeckPart::Attach {
                    slot: first_slot,
                    pcm: first.pcm,
                    segment: SegmentId::FIRST,
                },
            )
            .expect("attach first");
        mixer
            .send(
                When::Next,
                DeckPart::Start {
                    slot: first_slot,
                    fade: Fade::Crossfade(crate::CrossfadeSettings::default()),
                },
            )
            .expect("start first");
        fill(&mut first_lane);
        let mut pcm = [[0.0; consts::BLOCK_FRAMES]; 2];
        let [left, right] = &mut pcm;
        mixer
            .block(SessionFrame::new(0), [left, right])
            .expect("first host block");
        let rate_seq = first_control
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![LaneCommand::SetSpeed(SpeedCurve::Constant(1.5))],
                },
            )
            .expect("speed goes to its lane");
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let _ = first_lane.poll_commands(&mut context);
        let block_frames = u32::try_from(consts::BLOCK_FRAMES).expect("block size fits u32");
        assert_eq!(
            block_frames % 2,
            0,
            "chosen speed advances whole source frames"
        );
        let expected_frames = if supports_playback_rate() {
            i64::from(block_frames) * 3 / 2
        } else {
            i64::from(block_frames)
        };
        let (at, notifications) = rate_change_blocks(
            &mut mixer,
            &mut first_lane,
            &mut first_control,
            rate_seq,
            i64::from(block_frames),
            expected_frames,
        );
        if supports_playback_rate() {
            assert_eq!(notifications, [rate_seq]);
        } else {
            assert!(notifications.is_empty());
        }

        let (next, mut next_lane, _next_control, _next_dir) =
            warped_player_resource(effective_rate, "next", &half).await;
        mixer
            .send(
                When::Next,
                DeckPart::Attach {
                    slot: next_slot,
                    pcm: next.pcm,
                    segment: SegmentId::FIRST,
                },
            )
            .expect("attach next");
        mixer
            .send(
                When::Next,
                DeckPart::Start {
                    slot: next_slot,
                    fade: Fade::Crossfade(crate::CrossfadeSettings::default()),
                },
            )
            .expect("start next");
        fill(&mut first_lane);
        fill(&mut next_lane);
        let first_position = mixer.ends.snapshot.read().slots[0].position;
        let [left, right] = &mut pcm;
        mixer
            .block(at + FrameCount::new(consts::BLOCK_FRAMES), [left, right])
            .expect("next-track block");
        let snapshot = mixer.ends.snapshot.read();
        assert_eq!(
            ((snapshot.slots[0].position - first_position) * f64::from(consts::SAMPLE_RATE))
                .round()
                .to_i64()
                .expect("fixture source advance fits i64"),
            expected_frames
        );
        assert_eq!(
            (snapshot.slots[1].position * f64::from(consts::SAMPLE_RATE))
                .round()
                .to_i64()
                .expect("fixture source position fits i64"),
            expected_frames
        );
        assert!(first_control.receipts().next().is_none());
        assert_eq!(snapshot.slots[1].state, SlotState::Playing);
    }

    /// Pin (W3 Task 3.3 (b)): a mid-session unload — i.e. dropping the
    /// `Resource` — cancels the whole per-track subtree, not just the `Audio`
    /// half. The per-track token `T` is passed by identity into both the inner
    /// stream (File/Hls) and the `Audio` config; under propagate-down both take
    /// `T.child()`, so `Audio::Drop` alone would only reach its own child and
    /// leave the stream-side fetch loops running. `Resource::Drop` must cancel
    /// `T` so the stream subtree (modelled here by `stream_sub`) is torn down.
    #[kithara::test(native, flash(false))]
    fn drop_cancels_whole_per_track_subtree_not_just_audio() {
        let track = CancelToken::never();
        let stream_sub = track.child(); // File/Hls subtree F = T.child()
        let audio_sub = track.child(); // Audio subtree A = T.child()

        let resource = ResourceLane::new(EofReader::default(), Some(track.clone()), None);

        assert!(!stream_sub.is_cancelled() && !audio_sub.is_cancelled());
        drop(resource);
        assert!(
            stream_sub.is_cancelled(),
            "unload must cancel the stream-side subtree, not only the Audio half"
        );
        assert!(audio_sub.is_cancelled());
        assert!(track.is_cancelled());
    }

    /// A load whose track is cancelled ends at once, refused `Cancelled`,
    /// without opening its source: whoever sent it does not wait on an open
    /// nobody wants.
    #[kithara::test(tokio)]
    async fn a_cancelled_load_is_refused_without_opening_its_source() {
        let pools = pools();
        let mut config: ResourceConfig<TestPools> =
            ResourceConfig::for_src(ResourceSrc::Path("/kithara/missing.mp3".into()))
                .store(
                    AssetStore::builder(pools.clone())
                        .backend(StorageBackend::Memory)
                        .build(),
                )
                .worker(PlayWorker::new(PlayWorkerConfig::builder(pools).build()))
                .build();
        let track = CancelToken::never().child();
        track.cancel();
        config.cancel = Some(track);

        let load = ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay()));
        let (_sender, inbox) = load.lane_channel().expect("configured worker lane channel");
        let refused = load
            .open(
                Duration::ZERO,
                LaneStart {
                    speed: SpeedCurve::Constant(1.0),
                    keylock: false,
                    backend: StretchKind::default(),
                },
                inbox,
            )
            .await
            .map(|(opened, _, _)| opened);

        assert!(
            matches!(refused, Err(LoadRefusal::Cancelled)),
            "a cancelled load opened its source: {refused:?}"
        );
    }

    /// An open in flight ends the moment its track is cancelled, refused
    /// `Cancelled`, and gives its worker slot back: the next load opens
    /// instead of finding the worker full.
    #[kithara::test(native, tokio)]
    async fn an_open_in_flight_ends_cancelled_and_frees_its_slot() {
        use axum::Router;
        use futures::future;
        use kithara_platform::{time, tokio::sync::mpsc::unbounded_channel};
        use kithara_test_utils::TestHttpServer;

        let (reached_tx, mut reached) = unbounded_channel();
        let server = TestHttpServer::new(Router::new().fallback(move || {
            let reached = reached_tx.clone();
            async move {
                let _ = reached.send(());
                future::pending::<()>().await;
            }
        }))
        .await;
        let pools = pools();
        let worker = PlayWorker::new(
            PlayWorkerConfig::builder(pools.clone())
                .capacity(NonZeroUsize::MIN)
                .build(),
        );
        let load = |src: ResourceSrc, track: CancelToken| {
            let mut config: ResourceConfig<TestPools> = ResourceConfig::for_src(src)
                .store(
                    AssetStore::builder(pools.clone())
                        .backend(StorageBackend::Memory)
                        .build(),
                )
                .worker(worker.clone())
                .build();
            config.cancel = Some(track);
            let load = ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay()));
            let (sender, inbox) = load.lane_channel().expect("configured worker lane channel");
            async move {
                let opened = load
                    .open(
                        Duration::ZERO,
                        LaneStart {
                            speed: SpeedCurve::Constant(1.0),
                            keylock: false,
                            backend: StretchKind::default(),
                        },
                        inbox,
                    )
                    .await
                    .map(|(opened, _, _)| opened);
                drop(sender);
                opened
            }
        };
        let track = CancelToken::never().child();
        let stalled = load(
            ResourceSrc::parse(server.url("/stalled.mp3").as_str()).expect("a test URL"),
            track.clone(),
        );
        let cancel_once_reached = async {
            reached.recv().await.expect("the server holds its sender");
            track.cancel();
        };

        let (refused, ()) = time::timeout(
            Duration::from_secs(2),
            future::join(stalled, cancel_once_reached),
        )
        .await
        .expect("a cancelled open ends");
        assert!(
            matches!(refused, Err(LoadRefusal::Cancelled)),
            "a cancelled open answered otherwise: {refused:?}"
        );

        let next = load(
            ResourceSrc::Path("/kithara/missing.mp3".into()),
            CancelToken::never().child(),
        )
        .await;
        assert!(
            matches!(next, Err(LoadRefusal::Open(_))),
            "the cancelled open kept its worker slot: {next:?}"
        );
    }

    /// A resource with no per-track cancel wired in (custom reader) drops
    /// without panicking and cancels nothing.
    #[kithara::test(native, flash(false))]
    fn drop_without_cancel_is_passive() {
        let resource = ResourceLane::new(EofReader::default(), None, None);
        drop(resource);
    }

    #[kithara::test(native, flash(false))]
    fn drop_cancels_before_inner_reader_teardown() {
        let track = CancelToken::never();
        let state = Arc::new(AtomicU8::new(consts::NOT_DROPPED));
        let reader = EofReader::with_drop_probe(track.clone(), Arc::clone(&state));
        let resource = ResourceLane::new(reader, Some(track), None);

        drop(resource);

        assert_eq!(state.load(Ordering::SeqCst), consts::DROPPED_AFTER_CANCEL);
    }

    #[kithara::test(native, flash(false))]
    fn reader_unwrap_disarms_resource_cancel() {
        let track = CancelToken::never();
        let state = Arc::new(AtomicU8::new(consts::NOT_DROPPED));
        let reader = EofReader::with_drop_probe(track.clone(), Arc::clone(&state));
        let resource = Resource::from_reader(reader, None);
        let lane = ResourceLane::new(EofReader::default(), Some(track.clone()), None);

        let reader: Box<dyn AudioReader> = resource.into();

        assert!(!track.is_cancelled());
        assert_eq!(state.load(Ordering::SeqCst), consts::NOT_DROPPED);

        drop(reader);

        assert!(!track.is_cancelled());
        assert_eq!(state.load(Ordering::SeqCst), consts::DROPPED_BEFORE_CANCEL);
        drop(lane);
        assert!(track.is_cancelled());
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn seek_withdraws_the_resident_warp_context(half: Vec<f32>) {
        let shape = StreamShape::new(
            NonZeroU32::MIN,
            NonZeroU32::new(consts::SAMPLE_RATE).expect("static sample rate"),
        );
        let mut mixer = crate::mock::MixerRig::new(DeckMixerConfig::default(), shape, &pools())
            .expect("offline mixer");
        let slot = Slot::new(0);
        let (opened, _lane, _control, _dir) = warped_player_resource(1.0, "seek", &half[..2]).await;
        mixer
            .send(
                When::Next,
                DeckPart::Attach {
                    slot,
                    pcm: opened.pcm,
                    segment: SegmentId::FIRST,
                },
            )
            .expect("attach first segment");
        mixer
            .send(
                When::Next,
                DeckPart::Start {
                    slot,
                    fade: Fade::Declick,
                },
            )
            .expect("start first segment");
        let mut pcm = [[0.0; 1]; 2];
        let [left, right] = &mut pcm;
        mixer
            .block(SessionFrame::new(0), [left, right])
            .expect("present first segment");
        assert!(mixer.ends.snapshot.read().slots[0].mark.is_some());

        mixer
            .send(
                When::Next,
                DeckPart::Adopt {
                    slot,
                    segment: SegmentId::FIRST.next(),
                },
            )
            .expect("adopt seek segment before its PCM arrives");
        let [left, right] = &mut pcm;
        mixer
            .block(SessionFrame::new(1), [left, right])
            .expect("withdraw the old segment");

        assert!(mixer.ends.snapshot.read().slots[0].mark.is_none());
    }
}
