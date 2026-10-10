use std::{
    io::{Error as IoError, Seek, SeekFrom},
    num::NonZeroU32,
    sync::atomic::AtomicU64,
};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_decode::{Decoder, DecoderConfig, DecoderFactory, DecoderResamplerConfig};
use kithara_events::{DeferredBus, EventBus, EventReceiver, EventSet};
use kithara_platform::{CancelScope, sync::Arc, time::Duration, tokio::task::spawn_blocking};
use kithara_resampler::ResamplerBackend;
use kithara_signal::AudioSpec;
use kithara_stream::{MediaInfo, OpenedReader, Stream, StreamType, WorkerWake};
use kithara_test_utils::kithara;

use super::{
    core::{Audio, AudioContext},
    event::{
        DecoderChangedEventData, decoder_changed_event, decoder_gapless_event,
        decoder_resampler_event, playback_resampler_event,
    },
};
use crate::{
    AudioConfig, AudioDecoderConfig, AudioSession, DecodeError, DecoderChangeCause, FrameDomain,
    pipeline::{
        decode::{
            DecoderGeneration,
            core::{ActiveDecode, DecoderFactory as StreamDecoderFactory},
        },
        gapless::visible_duration,
        source::{SourceDecoderConfig, StreamAudioSource},
        stream::shared::SharedStream,
    },
};

#[derive_where::derive_where(Clone; B: Clone)]
struct DecoderDeps<B, S> {
    decoder: AudioDecoderConfig<B>,
    pools: PoolRegion<S>,
}
impl<B: Default + ResamplerBackend, S> DecoderDeps<B, S> {
    fn resampler_config(&self, rate: Option<NonZeroU32>) -> Option<DecoderResamplerConfig<B>> {
        self.decoder.build_resampler_config(rate)
    }
    fn publish_initial_events(
        &self,
        bus: &EventBus,
        info: Option<&MediaInfo>,
        spec: AudioSpec,
        track_info: &kithara_decode::DecoderTrackInfo,
        duration: Option<Duration>,
        rate: Option<NonZeroU32>,
    ) {
        bus.publish(decoder_changed_event(DecoderChangedEventData {
            media_info: info,
            spec,
            track_info,
            duration,
            backend: self.decoder.backend(),
            cause: DecoderChangeCause::Initial,
            base_offset: 0,
        }));
        if let Some(event) = decoder_gapless_event(info, spec, track_info, FrameDomain::Output) {
            bus.publish(event);
        }
        let resampler = self.resampler_config(rate);
        if let Some(event) = decoder_resampler_event(
            resampler.as_ref(),
            spec,
            info.and_then(|info| info.sample_rate),
        ) {
            bus.publish(event);
        }
        if let (Some(host_rate), Some(resampler)) = (rate, resampler.as_ref())
            && let Some(event) = playback_resampler_event(
                &resampler.backend,
                host_rate.get(),
                info.and_then(|info| info.sample_rate),
            )
        {
            bus.publish(event);
        }
    }
}
impl<T: StreamType<Events = EventBus>> Audio<Stream<T>> {
    /// Returns the unified source and decoder event bus.
    #[must_use]
    pub fn event_bus(&self) -> &EventBus {
        AudioSession::event_bus(self)
    }
    /// Subscribes to source and decoder events.
    #[must_use]
    pub fn events<E: EventSet>(&self) -> EventReceiver<E> {
        self.event_bus().subscribe()
    }
    /// Open the complete decoded source for transfer to its owning lane.
    ///
    /// # Errors
    /// Returns a source, decoder, or buffer-pool setup failure.
    #[kithara::measure(label = "audio.prepare")]
    pub async fn prepare<B, S>(
        config: AudioConfig<T, B>,
        wake: Arc<dyn WorkerWake>,
        pools: PoolRegion<S>,
    ) -> Result<Self, DecodeError>
    where
        B: Default + ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        let AudioConfig {
            hint,
            media_info: user_info,
            observer,
            decoder,
            host_sample_rate: rate,
            stream: stream_config,
            bus: configured_bus,
            cancel: configured_cancel,
            ..
        } = config;
        let cancel = CancelScope::new(configured_cancel).token();
        let bus = resolve_event_bus::<T>(&stream_config, configured_bus);
        let mut stream = create_stream_with_probe::<T>(stream_config).await?;
        stream.set_worker_wake(Arc::clone(&wake));
        let activity = stream.activity();
        let writer = stream.take_activity_writer();
        let playhead = stream.playhead_write();
        let info = merge_user_and_stream_media_info(user_info.clone(), stream.media_info());
        let shared = SharedStream::new(stream);
        let deps = DecoderDeps {
            decoder,
            pools: pools.clone(),
        };
        let reader = shared.open_initial_reader();
        let gate = reader.construction_gate();
        let factory = create_decoder_factory(&deps, user_info, hint);
        let initial = create_initial_decoder(reader, info.clone(), factory.clone(), rate).await?;
        let spec = initial.spec();
        let track_info = initial.track_info();
        let metadata = initial.metadata();
        let codec = info.as_ref().and_then(|info| info.codec);
        let duration = visible_duration(
            initial.duration(),
            initial.gapless_profile(codec),
            deps.decoder.gapless_mode(),
        )
        .or_else(|| playhead.duration());
        playhead.set_duration(duration);
        deps.publish_initial_events(&bus, info.as_ref(), spec, &track_info, duration, rate);
        let generation =
            DecoderGeneration::new(initial, info, 0, gate, None, deps.decoder.gapless_mode());
        let decode = ActiveDecode::new(generation, deps.decoder.gapless_mode(), observer, &pools)
            .map_err(|error| DecodeError::Io {
            source: IoError::other(error),
        })?;
        let abr = shared.abr_handle();
        let emit = Arc::new(DeferredBus::new(
            bus.clone(),
            crate::consts::AUDIO_EVENT_CAPACITY,
        ));
        let source = StreamAudioSource::new(
            shared,
            decode,
            SourceDecoderConfig {
                factory,
                host_rate: rate,
                backend: deps.decoder.backend(),
                playback_resampler_backend: deps.decoder.resampler_backend_name(),
            },
            Arc::clone(&emit),
            wake,
        );
        Ok(Self::new(
            Box::new(source),
            AudioContext {
                playhead,
                emit,
                metadata,
                abr,
                activity,
                activity_writer: writer,
                cancel,
            },
            spec,
        ))
    }
}
fn create_decoder_factory<B, S>(
    deps: &DecoderDeps<B, S>,
    user_info: Option<MediaInfo>,
    hint: Option<String>,
) -> StreamDecoderFactory
where
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let configured = user_info.clone();
    let deps = deps.clone();
    StreamDecoderFactory::new(
        move |mut reader, info, rate| {
            let byte_len = reader.byte_len().unwrap_or(0);
            let config = DecoderConfig::builder()
                .backend(deps.decoder.backend())
                .byte_len_handle(Arc::new(AtomicU64::new(byte_len)))
                .pools(deps.pools.clone())
                .maybe_byte_map(reader.byte_map())
                .maybe_hooks(reader.take_event_sink())
                .maybe_resampler(deps.resampler_config(rate))
                .build();
            let info = merge_user_and_stream_media_info(user_info.clone(), info);
            let source = reader.into_inner();
            let decoder = match info {
                Some(info) => DecoderFactory::create_from_media_info(source, &info, config),
                None => DecoderFactory::create_with_probe(source, hint.as_deref(), config),
            }?;
            decoder.update_byte_len(byte_len);
            Ok(decoder)
        },
        configured,
    )
}
async fn create_initial_decoder(
    reader: OpenedReader,
    info: Option<MediaInfo>,
    factory: StreamDecoderFactory,
    rate: Option<NonZeroU32>,
) -> Result<Box<dyn Decoder>, DecodeError> {
    let gate = reader.construction_gate();
    if let Some(gate) = &gate {
        gate.arm();
    }
    let built = spawn_blocking(move || factory.create(reader, info, rate)).await;
    if let Some(gate) = &gate {
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
    // Payload, not text: callers classify the transport failure behind a failed open.
    let stream = Stream::<T>::new(stream_config)
        .await
        .map_err(|error| DecodeError::Io {
            source: IoError::other(error),
        })?;
    kithara::probe_event!(source_opened);
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
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_file::{File, FileConfig, FileSrc};
    use kithara_resampler::NoResamplerBackend;
    use kithara_stream::mock::NoopWorkerWake;
    use kithara_test_fixtures::assets;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{TestPools, pools};

    #[kithara::test(native, tokio)]
    async fn prepares_source_registration_without_worker_activity() {
        let pools = pools();
        let path = assets::audio_wav_frames_44100()
            .path()
            .expect("native WAV fixture");
        let stream = FileConfig::for_src(FileSrc::Local(path.to_owned()))
            .store(
                AssetStore::builder(pools.clone())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .pools(pools.clone())
            .build();
        let config = AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(stream).build();
        let mut audio = Audio::prepare(config, Arc::new(NoopWorkerWake), pools)
            .await
            .expect("prepare decoded source without a decoder worker");
        let activity = audio.activity();
        let mut writer = audio
            .take_activity_writer()
            .expect("source hands off its sole writer");
        writer.set_playing(true);
        assert!(activity.is_playing());
        assert!(audio.take_activity_writer().is_none());
    }
}
