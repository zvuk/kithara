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
use kithara_stream::{
    MediaInfo, OpenedReader, PlayheadRead, Stream, StreamType, VariantControl, WorkerWake,
};
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
    host_sample_rate: Arc<AtomicU32>,
    decoder: AudioDecoderConfig<B>,
    byte_pool: BytePool,
    pcm_pool: PcmPool,
}

impl<B> DecoderDeps<B>
where
    B: Default + ResamplerBackend,
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

    delegate::delegate! {
        to self.decoder {
            fn backend(&self) -> kithara_decode::DecoderBackend;
        }
    }

    fn playback_resampler_backend(&self) -> &'static str {
        self.decoder.resampler_backend_name()
    }

    fn resampler_config(&self) -> Option<DecoderResamplerConfig<B>> {
        let target_sample_rate = NonZeroU32::new(self.host_sample_rate.load(Ordering::Acquire));
        self.decoder.build_resampler_config(target_sample_rate)
    }
}

struct FactoryDeps<B> {
    epoch: Arc<AtomicU64>,
    decoder: DecoderDeps<B>,
    /// The caller's `MediaInfo` declaration, kept for the life of the track.
    /// Every decoder built for it resolves through the same precedence as the
    /// initial one — a per-variant plan describes the variant, not the bytes
    /// the caller told us to expect.
    user_media_info: Option<MediaInfo>,
}

impl<B> FactoryDeps<B>
where
    B: ResamplerBackend,
{
    fn new(
        decoder: &DecoderDeps<B>,
        epoch: &Arc<AtomicU64>,
        user_media_info: Option<MediaInfo>,
    ) -> Self {
        Self {
            user_media_info,
            decoder: DecoderDeps::clone(decoder),
            epoch: Arc::clone(epoch),
        }
    }
}

struct StreamSourceRegistration<'a, T: StreamType> {
    cancel: &'a CancelToken,
    playback_resampler_backend: &'static str,
    emit: Arc<kithara_events::DeferredBus<Event>>,
    epoch: Arc<AtomicU64>,
    host_sample_rate: Arc<AtomicU32>,
    decoder: Box<dyn Decoder>,
    decoder_backend: kithara_decode::DecoderBackend,
    gapless_mode: GaplessMode,
    preload_chunks: NonZeroUsize,
    engine_load: Option<Arc<EngineLoad>>,
    initial_media_info: Option<MediaInfo>,
    variant_control: Option<Arc<dyn VariantControl>>,
    worker: Option<AudioWorkerHandle>,
    runtime_handle: RuntimeHandle,
    shared_stream: SharedStream<T>,
    decoder_factory: StreamDecoderFactory,
    effects: Vec<Box<dyn AudioEffect>>,
    recreate_on_host_rate_change: bool,
    pcm_buffer_chunks: usize,
}

struct RegisteredStreamSource {
    epoch: Arc<AtomicU64>,
    host_sample_rate: Arc<AtomicU32>,
    preload_gate: Arc<super::PreloadGate>,
    reader_wake: Arc<ThreadWake>,
    service_class: Arc<AtomicServiceClass>,
    worker: AudioWorkerHandle,
    data_rx: super::Inlet<Fetch<PcmChunk>>,
    trash_tx: super::Outlet<PcmChunk>,
    track_id: super::TrackId,
    is_standalone_worker: bool,
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
        B: Default + ResamplerBackend,
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
            consumer_wake_mode,
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
        let playhead = stream.playhead_write();
        let seek = stream.seek_control();
        let seek_obs = stream.seek_observe();
        let initial_media_info =
            merge_user_and_stream_media_info(user_media_info.clone(), stream.media_info());
        debug!(?initial_media_info, "Initial MediaInfo from stream");

        let variant_control = stream.variant_control();
        let shared_stream = SharedStream::new(stream);
        let host_sample_rate = Arc::new(AtomicU32::new(config_host_sr.map_or(0, NonZeroU32::get)));
        warm_pcm_pool(
            &pcm_pool,
            warm_channels(initial_media_info.as_ref()),
            pcm_buffer_chunks,
        );

        let gapless_mode = decoder.gapless_mode();
        let deps = DecoderDeps::new(
            decoder,
            pcm_pool.clone(),
            byte_pool.clone(),
            &host_sample_rate,
        );
        let initial_reader = shared_stream.open_initial_reader();
        let decoder =
            create_initial_decoder(initial_reader, initial_media_info.clone(), hint, &deps).await;
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
        let seek_prepare = shared_stream.seek_prepare();
        let emit = AudioEvents::deferred(&bus);
        // Publish the initial decoder events before the worker holds the
        // decoder: a registered track may decode its first chunk and flush
        // `FormatDetected` immediately, and `DecoderChanged { Initial }`
        // must reach the bus first.
        publish_initial_decoder_events(
            &bus,
            &deps,
            &host_sample_rate,
            initial_media_info.as_ref(),
            initial_spec,
            &initial_track_info,
            total_duration,
        );
        let registered = register_stream_audio_source(StreamSourceRegistration {
            decoder,
            effects,
            engine_load,
            gapless_mode,
            pcm_buffer_chunks,
            preload_chunks,
            runtime_handle,
            shared_stream,
            variant_control,
            cancel: &cancel,
            decoder_backend: deps.backend(),
            decoder_factory: create_decoder_factory(&deps, &epoch, user_media_info),
            emit: Arc::clone(&emit),
            epoch: Arc::clone(&epoch),
            host_sample_rate: Arc::clone(&host_sample_rate),
            initial_media_info: initial_media_info.clone(),
            playback_resampler_backend: deps.playback_resampler_backend(),
            // A requested host rate always resolves to a resampler plan, so a
            // route change is decided by `ResumeCursor`'s rate guards alone.
            recreate_on_host_rate_change: true,
            worker: config_worker,
        });

        let ring = RingConsumer::new(RingParts {
            block_on_underrun,
            consumer_wake_mode,
            pcm_rx: registered.data_rx,
            trash_tx: registered.trash_tx,
            reader_wake: registered.reader_wake,
            epoch: registered.epoch,
        });
        Ok(Self::from(AudioParts {
            ring,
            pcm_pool,
            emit,
            lease: WorkerLease {
                cancel: Some(cancel),
                track_id: Some(registered.track_id),
                worker: Some(registered.worker),
                is_standalone: registered.is_standalone_worker,
            },
            session: Session {
                playhead,
                seek,
                seek_obs,
                metadata,
                abr_handle,
                peer_wake,
                seek_prepare,
                preload_gate: registered.preload_gate,
            },
            controls: Controls {
                playback_rate,
                stretch,
                host_sample_rate: registered.host_sample_rate,
                service_class: registered.service_class,
            },
            spec: initial_spec,
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
) where
    B: Default + ResamplerBackend,
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
    let resampler = deps.resampler_config();
    if let Some(event) = decoder_resampler_event(
        resampler.as_ref(),
        initial_spec,
        initial_media_info.and_then(|info| info.sample_rate),
    ) {
        bus.publish(event);
    }
    if let Some(host_rate) = NonZeroU32::new(host_sample_rate.load(Ordering::Acquire))
        && let Some(resampler) = resampler.as_ref()
        && let Some(event) = playback_resampler_event(
            &resampler.backend,
            host_rate.get(),
            initial_media_info.and_then(|info| info.sample_rate),
        )
    {
        bus.publish(event);
    }
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
        registration.variant_control,
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
        is_standalone_worker,
        preload_gate,
        reader_wake,
        service_class,
        track_id,
        trash_tx,
        worker,
        epoch: registration.epoch,
        host_sample_rate: registration.host_sample_rate,
    }
}

fn create_decoder_factory<B>(
    decoder: &DecoderDeps<B>,
    epoch: &Arc<AtomicU64>,
    user_media_info: Option<MediaInfo>,
) -> StreamDecoderFactory
where
    B: Default + ResamplerBackend,
{
    let configured_media_info = user_media_info.clone();
    let deps = FactoryDeps::new(decoder, epoch, user_media_info);
    StreamDecoderFactory::new(
        move |mut reader, info| {
            let byte_len = reader.byte_len().unwrap_or(0);
            let byte_len_handle = Arc::new(AtomicU64::new(byte_len));
            let config = DecoderConfig::builder()
                .backend(deps.decoder.decoder.backend())
                .byte_len_handle(byte_len_handle)
                .pcm_pool(deps.decoder.pcm_pool.clone())
                .byte_pool(deps.decoder.byte_pool.clone())
                .epoch(deps.epoch.load(Ordering::Acquire))
                .maybe_byte_map(reader.byte_map())
                .maybe_hooks(reader.take_event_sink())
                .maybe_resampler(deps.decoder.resampler_config())
                .build();
            let source = reader.into_inner();
            let info = match deps.user_media_info.clone() {
                Some(user) => merge_media_info(user, &info),
                None => info,
            };
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
        },
        configured_media_info,
    )
}

async fn create_initial_decoder<B>(
    mut reader: OpenedReader,
    media_info: Option<MediaInfo>,
    hint: Option<String>,
    deps: &DecoderDeps<B>,
) -> Result<Box<dyn Decoder>, DecodeError>
where
    B: Default + ResamplerBackend,
{
    let byte_len = reader.byte_len().unwrap_or(0);
    let construction_gate = reader.construction_gate();
    let config = DecoderConfig::builder()
        .backend(deps.decoder.backend())
        .byte_len_handle(Arc::new(AtomicU64::new(byte_len)))
        .pcm_pool(deps.pcm_pool.clone())
        .byte_pool(deps.byte_pool.clone())
        .maybe_byte_map(reader.byte_map())
        .maybe_hooks(reader.take_event_sink())
        .maybe_hint(hint.clone())
        .maybe_resampler(deps.resampler_config())
        .build();
    let source = reader.into_inner();
    if let Some(gate) = &construction_gate {
        gate.arm();
    }
    let built = spawn_blocking(move || {
        if let Some(info) = &media_info {
            DecoderFactory::create_from_media_info(source, info, config)
        } else {
            DecoderFactory::create_with_probe(source, hint.as_deref(), config)
        }
    })
    .await;
    if let Some(gate) = &construction_gate {
        gate.disarm();
    }
    built.map_err(|error| DecodeError::Io {
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

/// Fill the caller's unset fields from what the source reports. The caller's
/// declaration wins: they know the bytes, the source only knows what its
/// container or playlist claims about them.
const fn merge_media_info(mut user: MediaInfo, stream: &MediaInfo) -> MediaInfo {
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
    user
}

const fn merge_user_and_stream_media_info(
    user: Option<MediaInfo>,
    stream: Option<MediaInfo>,
) -> Option<MediaInfo> {
    match (user, stream) {
        (Some(user), Some(stream)) => Some(merge_media_info(user, &stream)),
        (Some(user), None) => Some(user),
        (None, stream) => stream,
    }
}
