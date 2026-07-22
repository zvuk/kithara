use std::{
    io::{Error as IoError, Seek, SeekFrom},
    marker::PhantomData,
    num::{NonZeroU32, NonZeroUsize},
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::{
    Decoder, DecoderConfig, DecoderFactory, DecoderResamplerConfig, GaplessMode, PcmChunk, PcmSpec,
};
use kithara_events::{DecoderChangeCause, Event, EventBus, FrameDomain};
use kithara_platform::{
    CancelScope, CancelToken,
    sync::Arc,
    tokio::{runtime::Handle as RuntimeHandle, task::spawn_blocking},
};
use kithara_resampler::ResamplerBackend;
use kithara_stream::{MediaInfo, PlayheadRead, Stream, StreamType, WorkerWake};
use portable_atomic::AtomicF32;
use tracing::{debug, info, warn};

use super::{
    AtomicServiceClass, AudioConfig, AudioDecoderConfig, AudioEffect, AudioWorkerHandle,
    DecodeError, DecodeInit, EngineLoad, Fetch, PcmSession, RebuildRuntime, ServiceClass,
    SharedStream, SourceParts, StreamAudioSource, StreamDecoderFactory, ThreadWake,
    TrackRegistration, WorkerWakeBridge,
    core::{Audio, AudioParts, Controls, Session, WorkerLease},
    create_effects,
    event::{
        AudioEvents, DecoderChangedEventData, decoder_changed_event, decoder_gapless_event,
        decoder_resampler_event, playback_resampler_event,
    },
    ring::{RingConsumer, RingParts, create_channels, create_trash_channel},
};

const WARM_DECODE_FRAMES: usize = 4608;

#[derive(Clone)]
struct DecoderDeps<B> {
    byte_pool: BytePool,
    decoder: AudioDecoderConfig<B>,
    pcm_pool: PcmPool,
    host_sample_rate: Arc<AtomicU32>,
}

impl<B> DecoderDeps<B>
where
    B: ResamplerBackend,
{
    fn new(
        decoder: AudioDecoderConfig<B>,
        pcm_pool: PcmPool,
        byte_pool: BytePool,
        host_sample_rate: &Arc<AtomicU32>,
    ) -> Self {
        Self {
            byte_pool,
            decoder,
            pcm_pool,
            host_sample_rate: Arc::clone(host_sample_rate),
        }
    }

    fn recreates_on_host_rate_change(&self) -> bool {
        self.decoder.recreates_on_host_rate_change()
    }

    fn resampler_config(&self) -> Result<Option<DecoderResamplerConfig<B>>, DecodeError> {
        let target_sample_rate = NonZeroU32::new(self.host_sample_rate.load(Ordering::Acquire));
        self.decoder.build_resampler_config(target_sample_rate)
    }

    fn backend(&self) -> kithara_decode::DecoderBackend {
        self.decoder.backend()
    }

    fn playback_resampler_backend(&self) -> &'static str {
        self.decoder
            .resampler()
            .map_or("none", |resampler| resampler.backend().name())
    }
}

struct FactoryDeps<B> {
    decoder: DecoderDeps<B>,
    epoch: Arc<AtomicU64>,
    byte_len: Arc<AtomicU64>,
}

impl<B> FactoryDeps<B>
where
    B: ResamplerBackend,
{
    fn new(decoder: &DecoderDeps<B>, epoch: &Arc<AtomicU64>, byte_len: &Arc<AtomicU64>) -> Self {
        Self {
            decoder: DecoderDeps::clone(decoder),
            epoch: Arc::clone(epoch),
            byte_len: Arc::clone(byte_len),
        }
    }
}

struct StreamSourceRegistration<'a, T: StreamType> {
    cancel: &'a CancelToken,
    decoder: Box<dyn Decoder>,
    decoder_backend: kithara_decode::DecoderBackend,
    decoder_factory: StreamDecoderFactory<T>,
    effects: Vec<Box<dyn AudioEffect>>,
    emit: Arc<kithara_events::DeferredBus<Event>>,
    engine_load: Option<Arc<EngineLoad>>,
    epoch: Arc<AtomicU64>,
    gapless_mode: GaplessMode,
    pcm_pool: PcmPool,
    host_sample_rate: Arc<AtomicU32>,
    initial_media_info: Option<MediaInfo>,
    playback_resampler_backend: &'static str,
    pcm_buffer_chunks: usize,
    preload_chunks: NonZeroUsize,
    recreate_on_host_rate_change: bool,
    runtime_handle: RuntimeHandle,
    shared_stream: SharedStream<T>,
    worker: Option<AudioWorkerHandle>,
}

struct RegisteredStreamSource {
    data_rx: super::Inlet<Fetch<PcmChunk>>,
    epoch: Arc<AtomicU64>,
    host_sample_rate: Arc<AtomicU32>,
    is_standalone_worker: bool,
    preload_gate: Arc<super::PreloadGate>,
    reader_wake: Arc<ThreadWake>,
    service_class: Arc<AtomicServiceClass>,
    track_id: super::TrackId,
    trash_tx: super::Outlet<PcmChunk>,
    worker: AudioWorkerHandle,
}

impl<T> Audio<Stream<T>>
where
    T: StreamType<Events = EventBus>,
{
    /// Creates a stream-backed audio pipeline and registers its renderer node.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] when stream, probe, decoder, or runtime setup fails.
    pub async fn new<B>(config: AudioConfig<T, B>) -> Result<Self, DecodeError>
    where
        B: ResamplerBackend,
    {
        let AudioConfig {
            byte_pool,
            hint,
            host_sample_rate: config_host_sr,
            media_info: user_media_info,
            pcm_buffer_chunks,
            pcm_pool,
            playback_rate: config_playback_rate,
            stretch,
            engine_load,
            decoder,
            preload_chunks,
            block_on_underrun,
            stream: stream_config,
            bus: config_bus,
            effects: custom_effects,
            worker: config_worker,
            cancel: config_cancel,
        } = config;
        let cancel = CancelScope::new(config_cancel).token();
        let runtime_handle = RuntimeHandle::try_current().map_err(|error| DecodeError::Io {
            source: IoError::other(format!(
                "audio stream construction requires a tokio runtime: {error}"
            )),
        })?;

        let bus = resolve_event_bus::<T>(&stream_config, config_bus);
        let stream = create_stream_with_probe::<T>(stream_config, byte_pool.clone()).await?;
        let initial_byte_len = stream.len().unwrap_or(0);
        let playhead = stream.playhead_write();
        let seek = stream.seek_control();
        let seek_obs = stream.seek_observe();
        let initial_media_info =
            merge_user_and_stream_media_info(user_media_info, stream.media_info());
        debug!(?initial_media_info, "Initial MediaInfo from stream");

        let shared_stream = SharedStream::new(stream);
        let byte_len = Arc::new(AtomicU64::new(initial_byte_len));
        let host_sample_rate = Arc::new(AtomicU32::new(config_host_sr.map_or(0, NonZeroU32::get)));
        warm_pcm_pool(
            &pcm_pool,
            warm_channels(initial_media_info.as_ref()),
            pcm_buffer_chunks,
        );

        shared_stream.set_blocking(true);
        let gapless_mode = decoder.gapless_mode();
        let deps = DecoderDeps::new(
            decoder,
            pcm_pool.clone(),
            byte_pool.clone(),
            &host_sample_rate,
        );
        let decoder = create_initial_decoder(
            shared_stream.clone(),
            initial_media_info.clone(),
            hint,
            &deps,
        )
        .await;
        shared_stream.set_blocking(false);
        let decoder = decoder?;

        let initial_spec = decoder.spec();
        let initial_track_info = decoder.track_info();
        let total_duration = decoder.duration().or_else(|| playhead.duration());
        playhead.set_duration(total_duration);
        let metadata = decoder.metadata();
        let epoch = Arc::new(AtomicU64::new(0));
        let playback_rate = config_playback_rate.unwrap_or_else(|| Arc::new(AtomicF32::new(1.0)));
        let effects = create_effects(initial_spec, stretch.as_ref(), &pcm_pool, custom_effects);
        log_pipeline_ready(initial_spec, &host_sample_rate);

        let abr_handle = shared_stream.abr_handle();
        let peer_wake = shared_stream.peer_wake();
        let emit = AudioEvents::deferred(&bus);
        let registered = register_stream_audio_source(StreamSourceRegistration {
            cancel: &cancel,
            decoder,
            decoder_backend: deps.backend(),
            decoder_factory: create_decoder_factory(&deps, &epoch, &byte_len),
            effects,
            emit: Arc::clone(&emit),
            engine_load,
            epoch: Arc::clone(&epoch),
            gapless_mode,
            pcm_pool: pcm_pool.clone(),
            host_sample_rate: Arc::clone(&host_sample_rate),
            initial_media_info: initial_media_info.clone(),
            playback_resampler_backend: deps.playback_resampler_backend(),
            pcm_buffer_chunks,
            preload_chunks,
            recreate_on_host_rate_change: deps.recreates_on_host_rate_change(),
            runtime_handle,
            shared_stream,
            worker: config_worker,
        });
        publish_initial_decoder_events(
            &bus,
            &deps,
            &host_sample_rate,
            initial_media_info.as_ref(),
            initial_spec,
            &initial_track_info,
            total_duration,
        )?;

        let ring = RingConsumer::new(RingParts {
            pcm_rx: registered.data_rx,
            trash_tx: registered.trash_tx,
            reader_wake: registered.reader_wake,
            epoch: registered.epoch,
            block_on_underrun,
        });
        Ok(Self::from(AudioParts {
            lease: WorkerLease {
                cancel: Some(cancel),
                track_id: Some(registered.track_id),
                worker: Some(registered.worker),
                is_standalone: registered.is_standalone_worker,
            },
            ring,
            session: Session {
                playhead,
                preload_gate: registered.preload_gate,
                seek,
                seek_obs,
                metadata,
                abr_handle,
                peer_wake,
            },
            controls: Controls {
                host_sample_rate: registered.host_sample_rate,
                playback_rate,
                stretch,
                service_class: registered.service_class,
            },
            pcm_pool,
            spec: initial_spec,
            emit,
            marker: PhantomData,
        }))
    }

    #[must_use]
    /// Returns the unified event bus used by the stream and audio pipeline.
    pub fn event_bus(&self) -> &EventBus {
        PcmSession::event_bus(self)
    }

    #[must_use]
    /// Subscribes to unified stream and audio events.
    pub fn events(&self) -> kithara_events::EventReceiver {
        self.event_bus().subscribe()
    }
}

fn publish_initial_decoder_events<B>(
    bus: &EventBus,
    deps: &DecoderDeps<B>,
    host_sample_rate: &Arc<AtomicU32>,
    initial_media_info: Option<&MediaInfo>,
    initial_spec: PcmSpec,
    initial_track_info: &kithara_decode::DecoderTrackInfo,
    total_duration: Option<kithara_platform::time::Duration>,
) -> Result<(), DecodeError>
where
    B: ResamplerBackend,
{
    bus.publish(decoder_changed_event(DecoderChangedEventData {
        backend: deps.backend(),
        media_info: initial_media_info,
        spec: initial_spec,
        track_info: initial_track_info,
        epoch: 0,
        cause: DecoderChangeCause::Initial,
        base_offset: 0,
        duration: total_duration,
    }));
    if let Some(event) = decoder_gapless_event(
        initial_media_info,
        initial_spec,
        initial_track_info,
        FrameDomain::Output,
    ) {
        bus.publish(event);
    }
    if let Some(event) = decoder_resampler_event(
        deps.resampler_config()?.as_ref(),
        initial_spec,
        initial_media_info.and_then(|info| info.sample_rate),
    ) {
        bus.publish(event);
    }
    if let Some(host_rate) = NonZeroU32::new(host_sample_rate.load(Ordering::Acquire))
        && let Some(resampler) = deps.decoder.resampler()
        && let Some(event) = playback_resampler_event(
            resampler.backend(),
            host_rate.get(),
            initial_media_info.and_then(|info| info.sample_rate),
        )
    {
        bus.publish(event);
    }
    Ok(())
}

fn register_stream_audio_source<T>(
    registration: StreamSourceRegistration<'_, T>,
) -> RegisteredStreamSource
where
    T: StreamType,
{
    let wake_stream = registration.shared_stream.clone();
    let playhead = registration.shared_stream.playhead_write();
    let preload_gate = Arc::new(super::PreloadGate::default());
    let reader_wake = Arc::new(ThreadWake::default());
    let (data_tx, data_rx) = create_channels(
        registration.pcm_buffer_chunks,
        Arc::clone(&registration.emit),
        &reader_wake,
    );
    let (trash_tx, trash_inlet) = create_trash_channel(registration.pcm_buffer_chunks);
    let (worker, is_standalone_worker) = registration.worker.map_or_else(
        || {
            (
                AudioWorkerHandle::with_cancel(registration.cancel.child()),
                true,
            )
        },
        |worker| (worker, false),
    );
    let worker_wake: Arc<dyn WorkerWake> = Arc::new(WorkerWakeBridge(worker.clone()));
    let decode = DecodeInit {
        decoder: registration.decoder,
        decoder_factory: registration.decoder_factory,
        decoder_backend: registration.decoder_backend,
        gapless_mode: registration.gapless_mode,
        pcm_pool: registration.pcm_pool,
        host_sample_rate: registration.host_sample_rate.clone(),
        media_info: registration.initial_media_info,
        playback_resampler_backend: registration.playback_resampler_backend,
        recreate_on_host_rate_change: registration.recreate_on_host_rate_change,
    }
    .into_parts(
        registration.effects,
        registration.shared_stream.seek_observe().epoch(),
    );
    let parts = SourceParts::new(
        &registration.shared_stream,
        decode,
        registration.epoch.clone(),
        RebuildRuntime {
            handle: registration.runtime_handle,
            wake: worker_wake.clone(),
        },
    );
    let source = StreamAudioSource::new(registration.shared_stream, parts)
        .with_emit(Arc::clone(&registration.emit));

    let service_class = Arc::new(AtomicServiceClass::new(ServiceClass::default()));
    let track_id = worker.register_track(TrackRegistration {
        trash_inlet,
        source: Box::new(source),
        outlet: data_tx,
        preload_gate: preload_gate.clone(),
        preload_chunks: registration.preload_chunks.get(),
        playhead: playhead as Arc<dyn PlayheadRead>,
        emit: registration.emit,
        service_class: service_class.clone(),
        engine_load: registration.engine_load,
    });
    wake_stream.set_worker_wake(worker_wake);

    RegisteredStreamSource {
        data_rx,
        epoch: registration.epoch,
        host_sample_rate: registration.host_sample_rate,
        is_standalone_worker,
        preload_gate,
        reader_wake,
        service_class,
        track_id,
        trash_tx,
        worker,
    }
}

fn create_decoder_factory<T, B>(
    decoder: &DecoderDeps<B>,
    epoch: &Arc<AtomicU64>,
    byte_len: &Arc<AtomicU64>,
) -> StreamDecoderFactory<T>
where
    T: StreamType,
    B: ResamplerBackend,
{
    let deps = FactoryDeps::new(decoder, epoch, byte_len);
    Arc::new(move |stream, info, base_offset| {
        let byte_len = stream
            .len()
            .map_or(0, |length| length.saturating_sub(base_offset));
        deps.byte_len.store(byte_len, Ordering::Release);
        let config = DecoderConfig::builder()
            .backend(deps.decoder.decoder.backend())
            .byte_len_handle(Arc::clone(&deps.byte_len))
            .pcm_pool(deps.decoder.pcm_pool.clone())
            .byte_pool(deps.decoder.byte_pool.clone())
            .blend_duration(deps.decoder.decoder.blend_duration())
            .epoch(deps.epoch.load(Ordering::Acquire))
            .maybe_byte_map(stream.byte_map())
            .maybe_hooks(stream.take_reader_event_sink())
            .maybe_resampler(deps.decoder.resampler_config()?)
            .build();
        let source = super::OffsetReader::new(stream, base_offset);
        match DecoderFactory::create_from_media_info(source, &info, config) {
            Ok(decoder) => {
                decoder.update_byte_len(byte_len);
                Ok(decoder)
            }
            Err(error) => {
                warn!(?error, "failed to recreate decoder");
                Err(error)
            }
        }
    })
}

async fn create_initial_decoder<T, B>(
    shared_stream: SharedStream<T>,
    media_info: Option<MediaInfo>,
    hint: Option<String>,
    deps: &DecoderDeps<B>,
) -> Result<Box<dyn Decoder>, DecodeError>
where
    T: StreamType,
    B: ResamplerBackend,
{
    let config = DecoderConfig::builder()
        .backend(deps.decoder.backend())
        .byte_len_handle(Arc::new(AtomicU64::new(shared_stream.len().unwrap_or(0))))
        .pcm_pool(deps.pcm_pool.clone())
        .byte_pool(deps.byte_pool.clone())
        .blend_duration(deps.decoder.blend_duration())
        .maybe_byte_map(shared_stream.byte_map())
        .maybe_hooks(shared_stream.take_reader_event_sink())
        .maybe_hint(hint.clone())
        .maybe_resampler(deps.resampler_config()?)
        .build();
    spawn_blocking(move || {
        if let Some(info) = &media_info {
            DecoderFactory::create_from_media_info(shared_stream, info, config)
        } else {
            DecoderFactory::create_with_probe(shared_stream, hint.as_deref(), config)
        }
    })
    .await
    .map_err(|error| DecodeError::Io {
        source: IoError::other(format!("decoder task panicked: {error}")),
    })?
}

async fn create_stream_with_probe<T>(
    stream_config: T::Config,
    byte_pool: BytePool,
) -> Result<Stream<T>, DecodeError>
where
    T: StreamType,
{
    let stream = Stream::<T>::new(stream_config)
        .await
        .map_err(|error| DecodeError::Io {
            source: IoError::other(error.to_string()),
        })?;
    probe(stream, byte_pool).await
}

#[cfg(not(target_arch = "wasm32"))]
async fn probe<T>(stream: Stream<T>, byte_pool: BytePool) -> Result<Stream<T>, DecodeError>
where
    T: StreamType,
{
    spawn_blocking(move || probe_blocking(stream, &byte_pool))
        .await
        .map_err(|error| DecodeError::Io {
            source: IoError::other(format!("probe task panicked: {error}")),
        })?
}

#[cfg(target_arch = "wasm32")]
async fn probe<T>(stream: Stream<T>, byte_pool: BytePool) -> Result<Stream<T>, DecodeError>
where
    T: StreamType,
{
    probe_blocking(stream, &byte_pool)
}

fn probe_blocking<T>(mut stream: Stream<T>, _byte_pool: &BytePool) -> Result<Stream<T>, DecodeError>
where
    T: StreamType,
{
    stream
        .seek(SeekFrom::Start(0))
        .map_err(|source| DecodeError::Io { source })?;
    Ok(stream)
}

fn resolve_event_bus<T>(stream_config: &T::Config, configured: Option<EventBus>) -> EventBus
where
    T: StreamType<Events = EventBus>,
{
    T::event_bus(stream_config)
        .or(configured)
        .unwrap_or_default()
}

fn warm_channels(info: Option<&MediaInfo>) -> usize {
    info.and_then(|info| info.channels).map_or(2, usize::from)
}

fn warm_pcm_pool(pool: &PcmPool, channels: usize, chunks: usize) {
    if pool.allocated_bytes() != 0 {
        return;
    }
    let capacity = WARM_DECODE_FRAMES * channels.max(1);
    pool.pre_warm(chunks.saturating_mul(2).max(1), |buffer| {
        buffer.clear();
        buffer.resize(capacity, 0.0);
    });
}

fn log_pipeline_ready(spec: PcmSpec, host_sample_rate: &Arc<AtomicU32>) {
    info!(
        ?spec,
        host_sr = host_sample_rate.load(Ordering::Relaxed),
        "Audio pipeline created"
    );
}

fn merge_user_and_stream_media_info(
    user: Option<MediaInfo>,
    stream: Option<MediaInfo>,
) -> Option<MediaInfo> {
    match (user, stream) {
        (Some(mut user), Some(stream)) => {
            if user.codec.is_none() {
                user.codec = stream.codec;
            }
            if user.container.is_none() {
                user.container = stream.container;
            }
            if user.channels.is_none() {
                user.channels = stream.channels;
            }
            if user.sample_rate.is_none() {
                user.sample_rate = stream.sample_rate;
            }
            if user.variant_index.is_none() {
                user.variant_index = stream.variant_index;
            }
            Some(user)
        }
        (Some(user), None) => Some(user),
        (None, stream) => stream,
    }
}
