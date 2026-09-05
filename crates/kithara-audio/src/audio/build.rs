use std::{
    io::{Error as IoError, Seek, SeekFrom},
    marker::PhantomData,
    num::NonZeroU32,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_decode::{Decoder, DecoderConfig, DecoderFactory, DecoderResamplerConfig};
use kithara_events::{DecoderChangeCause, Event, EventBus, FrameDomain};
use kithara_platform::{
    CancelScope,
    sync::Arc,
    tokio::{runtime::Handle as RuntimeHandle, task::spawn_blocking},
};
use kithara_resampler::ResamplerBackend;
use kithara_signal::{AudioChunk, AudioSpec};
use kithara_stream::{MediaInfo, OpenedReader, PlayheadWrite, Stream, StreamType, WorkerWake};
use kithara_test_utils::kithara;
use tracing::{debug, info, warn};

use super::{
    AudioConfig, AudioDecoderConfig, AudioSession, ConsumerWakeMode, DecodeError, DecodeInit,
    PreparedAudioLane, ProducerPort, RebuildRuntime, SharedStream, SourceParts, StreamAudioSource,
    StreamDecoderFactory, ThreadWake,
    core::{Audio, AudioParts, AudioRuntime, Controls, PreparedAudio, Session},
    cursor::ChunkCursor,
    event::{
        AudioEvents, DecoderChangedEventData, decoder_changed_event, decoder_gapless_event,
        decoder_resampler_event, playback_resampler_event,
    },
    ring::{RingConsumer, RingParts, create_channels, create_trash_channel},
};

struct DecoderDeps<B, S> {
    host_sample_rate: Arc<AtomicU32>,
    decoder: AudioDecoderConfig<B>,
    pools: PoolRegion<S>,
}

impl<B, S> Clone for DecoderDeps<B, S>
where
    B: Clone,
{
    fn clone(&self) -> Self {
        Self {
            host_sample_rate: Arc::clone(&self.host_sample_rate),
            decoder: self.decoder.clone(),
            pools: self.pools.clone(),
        }
    }
}

impl<B, S> DecoderDeps<B, S>
where
    B: Default + ResamplerBackend,
{
    fn new(
        decoder: AudioDecoderConfig<B>,
        pools: PoolRegion<S>,
        host_sample_rate: &Arc<AtomicU32>,
    ) -> Self {
        Self {
            decoder,
            pools,
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

    fn decode_init(
        &self,
        decoder: Box<dyn Decoder>,
        decoder_factory: StreamDecoderFactory,
        media_info: Option<MediaInfo>,
    ) -> DecodeInit<S> {
        DecodeInit {
            decoder,
            decoder_factory,
            decoder_backend: self.backend(),
            gapless_mode: self.decoder.gapless_mode(),
            host_sample_rate: Arc::clone(&self.host_sample_rate),
            media_info,
            pools: self.pools.clone(),
            playback_resampler_backend: self.playback_resampler_backend(),
            // A requested host rate always resolves to a resampler plan, so a
            // route change is decided by `ResumeCursor`'s rate guards alone.
            recreate_on_host_rate_change: true,
        }
    }

    fn resampler_config(&self) -> Option<DecoderResamplerConfig<B>> {
        let target_sample_rate = NonZeroU32::new(self.host_sample_rate.load(Ordering::Acquire));
        self.decoder.build_resampler_config(target_sample_rate)
    }

    /// Publish the initial decoder state before the source becomes registrable.
    /// A registered source may emit `FormatDetected` on its first worker pass.
    fn publish_initial_events(
        &self,
        bus: &EventBus,
        media_info: Option<&MediaInfo>,
        spec: AudioSpec,
        track_info: &kithara_decode::DecoderTrackInfo,
        duration: Option<kithara_platform::time::Duration>,
    ) {
        bus.publish(decoder_changed_event(DecoderChangedEventData {
            backend: self.backend(),
            media_info,
            spec,
            track_info,
            epoch: 0,
            cause: DecoderChangeCause::Initial,
            base_offset: 0,
            duration,
        }));
        if let Some(event) =
            decoder_gapless_event(media_info, spec, track_info, FrameDomain::Output)
        {
            bus.publish(event);
        }
        let resampler = self.resampler_config();
        if let Some(event) = decoder_resampler_event(
            resampler.as_ref(),
            spec,
            media_info.and_then(|info| info.sample_rate),
        ) {
            bus.publish(event);
        }
        if let Some(host_rate) = NonZeroU32::new(self.host_sample_rate.load(Ordering::Acquire))
            && let Some(resampler) = resampler.as_ref()
            && let Some(event) = playback_resampler_event(
                &resampler.backend,
                host_rate.get(),
                media_info.and_then(|info| info.sample_rate),
            )
        {
            bus.publish(event);
        }
    }
}

struct FactoryDeps<B, S> {
    epoch: Arc<AtomicU64>,
    decoder: DecoderDeps<B, S>,
    /// The caller's `MediaInfo` declaration, kept for the life of the track.
    /// Every decoder built for it resolves through the same precedence as the
    /// initial one — a per-variant plan describes the variant, not the bytes
    /// the caller told us to expect.
    user_media_info: Option<MediaInfo>,
}

impl<B, S> FactoryDeps<B, S>
where
    B: ResamplerBackend,
{
    fn new(
        decoder: &DecoderDeps<B, S>,
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

struct StreamSourceRegistration<S> {
    preload_gate: Arc<super::PreloadGate>,
    ring: RingParts,
    lane: PreparedAudioLane<S>,
}

struct PreparedStreamSource<S> {
    preload_gate: Arc<super::PreloadGate>,
    ring: RingParts,
    lane: PreparedAudioLane<S>,
}

impl<T> Audio<Stream<T>>
where
    T: StreamType<Events = EventBus>,
{
    /// Prepare a stream-backed audio reader and its concrete producer lane.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] when stream, probe, decoder, or runtime setup fails.
    #[doc(hidden)]
    #[kithara::measure(label = "audio.prepare")]
    pub async fn prepare<B, S>(
        config: AudioConfig<T, B>,
        wake: Arc<dyn WorkerWake>,
        pools: PoolRegion<S>,
    ) -> Result<PreparedAudio<Self, impl crate::AudioSource<Chunk = AudioChunk>>, DecodeError>
    where
        B: Default + ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        let AudioConfig {
            hint,
            media_info: user_media_info,
            observer,
            decoder,
            preload_chunks,
            host_sample_rate: config_host_sr,
            block_on_underrun,
            consumer_wake_mode,
            audio_buffer_chunks,
            stream: stream_config,
            bus: config_bus,
            cancel: config_cancel,
        } = config;
        let cancel = CancelScope::new(config_cancel).token();
        let runtime_handle = current_runtime_handle()?;

        let bus = resolve_event_bus::<T>(&stream_config, config_bus);
        let stream = create_stream_with_probe::<T>(stream_config).await?;
        let playhead = stream.playhead_write();
        let seek = stream.seek_control();
        let seek_obs = stream.seek_observe();
        let initial_media_info =
            merge_user_and_stream_media_info(user_media_info.clone(), stream.media_info());
        debug!(?initial_media_info, "Initial MediaInfo from stream");

        let variant_control = stream.variant_control();
        let shared_stream = SharedStream::new(stream);
        let host_sample_rate = Arc::new(AtomicU32::new(config_host_sr.map_or(0, NonZeroU32::get)));
        let deps = DecoderDeps::new(decoder, pools.clone(), &host_sample_rate);
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
        log_pipeline_ready(initial_spec, &host_sample_rate);

        let abr_handle = shared_stream.abr_handle();
        let peer_wake = shared_stream.peer_wake();
        let seek_prepare = shared_stream.seek_prepare();
        let emit = AudioEvents::deferred(&bus);
        deps.publish_initial_events(
            &bus,
            initial_media_info.as_ref(),
            initial_spec,
            &initial_track_info,
            total_duration,
        );
        let wake_stream = shared_stream.clone();
        let preload_gate = Arc::new(super::PreloadGate::default());
        let (port, ring) = prepare_pcm_ring(
            audio_buffer_chunks.max(preload_chunks.get()),
            &emit,
            &epoch,
            block_on_underrun,
            consumer_wake_mode,
        );
        let decode = deps
            .decode_init(
                decoder,
                create_decoder_factory(&deps, &epoch, user_media_info),
                initial_media_info.clone(),
            )
            .into_parts(observer, shared_stream.seek_observe().epoch())?;
        let parts = SourceParts::new(
            &shared_stream,
            decode,
            Arc::clone(&epoch),
            RebuildRuntime {
                handle: runtime_handle,
                wake: Arc::clone(&wake),
            },
            variant_control,
        );
        let source = StreamAudioSource::new(shared_stream, parts).with_emit(Arc::clone(&emit));
        let lane = PreparedAudioLane {
            source,
            port,
            preload_gate: Arc::clone(&preload_gate),
            preload_chunks: preload_chunks.get(),
            playhead: Arc::clone(&playhead) as Arc<dyn PlayheadWrite>,
            emit: Arc::clone(&emit),
        };
        let registration = prepare_stream_source_registration(lane, ring, preload_gate);
        let prepared = prepare_stream_audio_source(registration, &wake_stream, Arc::clone(&wake));

        let audio = Self::from(AudioParts {
            ring: RingConsumer::new(prepared.ring),
            cursor: ChunkCursor::new(&pools, initial_spec).map_err(DecodeError::backend)?,
            emit,
            runtime: AudioRuntime { cancel, wake },
            session: Session {
                playhead,
                seek,
                seek_obs,
                metadata,
                abr_handle,
                peer_wake,
                seek_prepare,
                preload_gate: prepared.preload_gate,
            },
            controls: Controls { host_sample_rate },
            marker: PhantomData,
        });
        Ok(PreparedAudio::new(audio, prepared.lane))
    }

    #[must_use]
    /// Returns the unified event bus used by the stream and audio pipeline.
    pub fn event_bus(&self) -> &EventBus {
        AudioSession::event_bus(self)
    }

    #[must_use]
    /// Subscribes to unified stream and audio events.
    pub fn events(&self) -> kithara_events::EventReceiver {
        self.event_bus().subscribe()
    }
}

fn current_runtime_handle() -> Result<RuntimeHandle, DecodeError> {
    RuntimeHandle::try_current().map_err(|error| DecodeError::Io {
        source: IoError::other(format!(
            "audio stream construction requires a tokio runtime: {error}"
        )),
    })
}

/// The output ring is never shallower than the preload gate wants: a ring that
/// cannot hold the chunks preload waits for never signals readiness.
fn prepare_pcm_ring(
    audio_buffer_chunks: usize,
    emit: &Arc<kithara_events::DeferredBus<Event>>,
    epoch: &Arc<AtomicU64>,
    block_on_underrun: bool,
    consumer_wake_mode: ConsumerWakeMode,
) -> (ProducerPort, RingParts) {
    let reader_wake = Arc::new(ThreadWake::default());
    let (data_tx, data_rx) = create_channels(audio_buffer_chunks, Arc::clone(emit), &reader_wake);
    let (trash_tx, trash_inlet) = create_trash_channel(audio_buffer_chunks);
    let ring = RingParts {
        block_on_underrun,
        consumer_wake_mode,
        audio_rx: data_rx,
        trash_tx,
        reader_wake,
        epoch: Arc::clone(epoch),
    };
    (ProducerPort::new(data_tx, trash_inlet), ring)
}

fn prepare_stream_source_registration<S>(
    lane: PreparedAudioLane<S>,
    ring: RingParts,
    preload_gate: Arc<super::PreloadGate>,
) -> StreamSourceRegistration<S> {
    StreamSourceRegistration {
        preload_gate,
        ring,
        lane,
    }
}

fn prepare_stream_audio_source<T, S>(
    registration: StreamSourceRegistration<S>,
    shared_stream: &SharedStream<T>,
    worker_wake: Arc<dyn WorkerWake>,
) -> PreparedStreamSource<S>
where
    T: StreamType,
{
    shared_stream.set_worker_wake(worker_wake);

    PreparedStreamSource {
        preload_gate: registration.preload_gate,
        ring: registration.ring,
        lane: registration.lane,
    }
}

fn create_decoder_factory<B, S>(
    decoder: &DecoderDeps<B, S>,
    epoch: &Arc<AtomicU64>,
    user_media_info: Option<MediaInfo>,
) -> StreamDecoderFactory
where
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
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
                .pools(deps.decoder.pools.clone())
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

async fn create_initial_decoder<B, S>(
    mut reader: OpenedReader,
    media_info: Option<MediaInfo>,
    hint: Option<String>,
    deps: &DecoderDeps<B, S>,
) -> Result<Box<dyn Decoder>, DecodeError>
where
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let byte_len = reader.byte_len().unwrap_or(0);
    let construction_gate = reader.construction_gate();
    let config = DecoderConfig::builder()
        .backend(deps.decoder.backend())
        .byte_len_handle(Arc::new(AtomicU64::new(byte_len)))
        .pools(deps.pools.clone())
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

async fn create_stream_with_probe<T>(stream_config: T::Config) -> Result<Stream<T>, DecodeError>
where
    T: StreamType,
{
    let stream = Stream::<T>::new(stream_config)
        .await
        .map_err(|error| DecodeError::Io {
            source: IoError::other(error.to_string()),
        })?;
    probe(stream).await
}

#[cfg(not(target_arch = "wasm32"))]
async fn probe<T>(stream: Stream<T>) -> Result<Stream<T>, DecodeError>
where
    T: StreamType,
{
    spawn_blocking(move || probe_blocking(stream))
        .await
        .map_err(|error| DecodeError::Io {
            source: IoError::other(format!("probe task panicked: {error}")),
        })?
}

#[cfg(target_arch = "wasm32")]
async fn probe<T>(stream: Stream<T>) -> Result<Stream<T>, DecodeError>
where
    T: StreamType,
{
    probe_blocking(stream)
}

fn probe_blocking<T>(mut stream: Stream<T>) -> Result<Stream<T>, DecodeError>
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

fn log_pipeline_ready(spec: AudioSpec, host_sample_rate: &Arc<AtomicU32>) {
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

#[cfg(test)]
mod tests {
    use kithara_events::DeferredBus;
    use kithara_signal::AudioChunk;
    use kithara_stream::PlayheadState;
    use kithara_test_utils::kithara;
    use unimock::Unimock;

    use super::*;
    use crate::{ConsumerWakeMode, traits::AudioSource};

    #[kithara::test]
    fn prepares_source_registration_without_worker_activity() {
        let emit = Arc::new(DeferredBus::new(EventBus::new(8), 8));
        let epoch = Arc::new(AtomicU64::new(0));
        let (port, ring) =
            prepare_pcm_ring(1, &emit, &epoch, false, ConsumerWakeMode::RealtimeDeferred);
        let preload_gate = Arc::new(super::super::PreloadGate::default());
        let source: Box<dyn AudioSource<Chunk = AudioChunk>> = Box::new(Unimock::new(()));
        let lane = PreparedAudioLane {
            source,
            port,
            preload_gate: Arc::clone(&preload_gate),
            preload_chunks: 1,
            playhead: Arc::new(PlayheadState::new()) as Arc<dyn PlayheadWrite>,
            emit,
        };

        let registration =
            prepare_stream_source_registration(lane, ring, Arc::clone(&preload_gate));

        assert!(Arc::ptr_eq(&registration.preload_gate, &preload_gate));
    }
}
