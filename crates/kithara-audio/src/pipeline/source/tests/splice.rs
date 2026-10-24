use std::{
    num::{NonZeroU32, NonZeroUsize},
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use kithara_bufpool::PoolRegion;
use kithara_decode::{DecoderConfig, DecoderFactory as DecodeFactory, GaplessMode};
use kithara_hls::parse_media_playlist;
use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
};
use kithara_signal::AudioChunk;
use kithara_storage::WaitOutcome;
use kithara_stream::{
    Activity, ActivityWriter, AudioCodec, ByteMap, ContainerFormat, DeferredWake, MediaInfo,
    NotReadyCause, OpenedReader, PendingReason, PlayheadRead, PlayheadState, PlayheadWrite,
    ReadOutcome, ReaderProfile, SegmentDescriptor, Source, SourceError, SourcePhase, SourceProbe,
    SourceSeekAnchor, Stream, StreamError, StreamResult, StreamType, VariantControl,
    VariantPromotion, VariantReaderPlan, VariantReaderTake, VariantTransition,
    mock::NoopWorkerWake,
};
use kithara_test_utils::kithara;
use url::Url;

use crate::{
    consts,
    pipeline::{
        decode::{
            DecoderGeneration,
            core::{ActiveDecode, DecoderFactory},
        },
        fetch::Fetch,
        rebuild::{RecreateCause, RecreateState},
        source::{SourceDecoderConfig, StreamAudioSource},
        stream::shared::SharedStream,
        track::TrackStep,
    },
    test_pools::{TestPools, pools},
    traits::AudioSource,
};

fn produced_data(fetch: Fetch<AudioChunk>) -> AudioChunk {
    let Fetch::Data { data, .. } = fetch else {
        panic!("TrackStep::Produced must carry PCM data");
    };
    data
}

struct VariantLayout {
    init_range: Range<u64>,
    blob: Vec<u8>,
    segments: Vec<SegmentDescriptor>,
}

struct SpliceState {
    active: AtomicUsize,
    available: AtomicU64,
    cancelled: AtomicBool,
    estimated_extra: AtomicU64,
    media_info: Mutex<Option<MediaInfo>>,
    peer_wake: Arc<DeferredWake>,
    warmup_landing: Mutex<Option<SegmentDescriptor>>,
    waits: Mutex<Vec<Range<u64>>>,
    variants: Vec<VariantLayout>,
    #[cfg(all(feature = "resample-rubato", feature = "symphonia"))]
    native: std::sync::OnceLock<Arc<buffered_readiness::NativeReads>>,
}

impl SpliceState {
    fn new(variants: Vec<VariantLayout>) -> Self {
        Self {
            variants,
            active: AtomicUsize::new(consts::SLQ_VARIANT),
            available: AtomicU64::new(u64::MAX),
            cancelled: AtomicBool::new(false),
            estimated_extra: AtomicU64::new(0),
            media_info: Mutex::new(Some(media_info(consts::SLQ_VARIANT))),
            peer_wake: Arc::new(DeferredWake::default()),
            warmup_landing: Mutex::new(None),
            waits: Mutex::new(Vec::new()),
            #[cfg(all(feature = "resample-rubato", feature = "symphonia"))]
            native: std::sync::OnceLock::new(),
        }
    }

    fn active_index(&self) -> usize {
        self.active
            .load(Ordering::Acquire)
            .min(self.variants.len().saturating_sub(1))
    }

    fn active_layout(&self) -> &VariantLayout {
        &self.variants[self.active_index()]
    }

    fn switch_to(&self, variant: usize) {
        self.active.store(variant, Ordering::Release);
        *self.media_info.lock() = Some(media_info(variant));
    }

    fn warmup_landing(&self) -> Option<SegmentDescriptor> {
        self.warmup_landing.lock().clone()
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if self.cancelled.load(Ordering::Acquire) {
            SourcePhase::Cancelled
        } else if range.end <= self.available.load(Ordering::Acquire) {
            SourcePhase::Ready
        } else {
            SourcePhase::WaitingDemand
        }
    }

    #[cfg(all(feature = "resample-rubato", feature = "symphonia"))]
    fn record_cursor(&self, position: u64) {
        if let Some(native) = self.native.get() {
            native
                .state
                .lock()
                .events
                .push(buffered_readiness::InputEvent::Cursor(position));
        }
    }
}

impl VariantControl for SpliceState {
    fn bind_active_reader(&self, reader: OpenedReader) -> OpenedReader {
        reader
    }

    fn abort_variant(&self, _transition: VariantTransition) -> bool {
        false
    }

    fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        let range = self.active_layout().init_range.clone();
        if range.is_empty() {
            Err(StreamError::Source(SourceError::FormatChangeNotApplicable))
        } else {
            Ok(range)
        }
    }

    fn plan_variant_reader(
        &self,
        _landing: Option<Duration>,
    ) -> StreamResult<Option<VariantReaderPlan>> {
        Ok(None)
    }

    fn prepare_variant_reader(
        &self,
        _plan: VariantReaderPlan,
        _profile: ReaderProfile,
    ) -> StreamResult<Option<VariantTransition>> {
        Ok(None)
    }

    fn promote_variant(&self, _transition: VariantTransition) -> VariantPromotion {
        VariantPromotion::Stale
    }

    fn take_prepared_variant_reader(
        &self,
        _transition: VariantTransition,
    ) -> StreamResult<VariantReaderTake> {
        Ok(VariantReaderTake::Stale)
    }
}

impl ByteMap for SpliceState {
    fn anchor_at_time(&self, position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        Ok(self.segment_at_time(position).map(|segment| {
            SourceSeekAnchor::builder()
                .segment_start(segment.decode_time)
                .segment_end(segment.decode_time.saturating_add(segment.duration))
                .segment_index(segment.segment_index)
                .variant_index(segment.variant_index)
                .byte_offset(segment.byte_range.start)
                .build()
        }))
    }

    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        self.active_layout()
            .segments
            .iter()
            .find(|segment| segment.byte_range.start >= byte_offset)
            .cloned()
    }

    fn segment_at_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        self.active_layout()
            .segments
            .iter()
            .find(|segment| segment.byte_range.contains(&byte_offset))
            .cloned()
    }

    fn segment_at_index(&self, segment_index: u32) -> Option<SegmentDescriptor> {
        let mut segment = self
            .active_layout()
            .segments
            .get(usize::try_from(segment_index).ok()?)
            .cloned()?;
        if segment_index == 0 {
            segment.byte_range.end += self.estimated_extra.load(Ordering::Acquire);
        }
        Some(segment)
    }

    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        let found = self
            .active_layout()
            .segments
            .iter()
            .find(|segment| t < segment.decode_time.saturating_add(segment.duration))
            .or_else(|| self.active_layout().segments.last())
            .cloned();
        if self.active_index() == consts::SMQ_VARIANT
            && t < Duration::from_secs(120)
            && let Some(segment) = found.as_ref()
        {
            *self.warmup_landing.lock() = Some(segment.clone());
        }
        found
    }

    delegate::delegate! {
        to self {
            #[expr($.init_range.clone())]
            #[call(active_layout)]
            fn init_segment_range(&self) -> Range<u64>;
            #[expr(Some(u64::try_from($.blob.len()).expect("blob length fits u64")))]
            #[call(active_layout)]
            fn len(&self) -> Option<u64>;
            #[expr(u32::try_from($.segments.len()).ok())]
            #[call(active_layout)]
            fn segment_count(&self) -> Option<u32>;
        }
    }
}

struct SpliceSource {
    playhead: Arc<PlayheadState>,
    position: Arc<AtomicU64>,
    activity: Activity,
    writer: Option<ActivityWriter>,
    state: Arc<SpliceState>,
}

impl SpliceSource {
    fn new(state: Arc<SpliceState>) -> Self {
        let writer = ActivityWriter::new();
        Self {
            activity: writer.reader(),
            writer: Some(writer),
            state,
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
        }
    }
}

struct ReadyProbe {
    position: Arc<AtomicU64>,
    state: Arc<SpliceState>,
}

impl SourceProbe for ReadyProbe {
    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        Some(Arc::clone(&self.state) as Arc<dyn ByteMap>)
    }

    fn len(&self) -> Option<u64> {
        Some(u64::try_from(self.state.active_layout().blob.len()).expect("blob length fits u64"))
    }

    fn phase(&self) -> SourcePhase {
        let position = self.position.load(Ordering::Acquire);
        self.state.phase_at(position..position.saturating_add(1))
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        self.state.phase_at(range)
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn set_position(&self, pos: u64) {
        #[cfg(all(feature = "resample-rubato", feature = "symphonia"))]
        self.state.record_cursor(pos);
        self.position.store(pos, Ordering::Release);
    }
}

impl Source for SpliceSource {
    fn activity(&self) -> Activity {
        self.activity.clone()
    }

    fn take_activity_writer(&mut self) -> Option<ActivityWriter> {
        self.writer.take()
    }

    fn advance(&self, n: u64) {
        #[cfg(all(feature = "resample-rubato", feature = "symphonia"))]
        self.state
            .record_cursor(self.position.load(Ordering::Acquire).saturating_add(n));
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        Some(Arc::clone(&self.state) as Arc<dyn ByteMap>)
    }

    fn len(&self) -> Option<u64> {
        Some(u64::try_from(self.state.active_layout().blob.len()).expect("blob length fits u64"))
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.state.media_info.lock().clone()
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        self.state.phase_at(range)
    }

    fn peer_wake(&self) -> Option<Arc<DeferredWake>> {
        Some(Arc::clone(&self.state.peer_wake))
    }

    fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
    }

    fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn probe(&self) -> Arc<dyn SourceProbe> {
        Arc::new(ReadyProbe {
            position: Arc::clone(&self.position),
            state: Arc::clone(&self.state),
        })
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let blob = &self.state.active_layout().blob;
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        if start >= blob.len() {
            return Ok(ReadOutcome::Eof);
        }
        let available = usize::try_from(self.state.available.load(Ordering::Acquire))
            .unwrap_or(usize::MAX)
            .min(blob.len());
        if start >= available {
            return Ok(ReadOutcome::Pending(PendingReason::NotReady(
                NotReadyCause::WaitBudgetExhausted,
            )));
        }
        let n = (available - start).min(buf.len());
        buf[..n].copy_from_slice(&blob[start..start + n]);
        Ok(ReadOutcome::Bytes(
            NonZeroUsize::new(n).expect("non-empty read must produce nonzero bytes"),
        ))
    }

    fn set_position(&self, pos: u64) {
        #[cfg(all(feature = "resample-rubato", feature = "symphonia"))]
        self.state.record_cursor(pos);
        self.position.store(pos, Ordering::Release);
    }

    fn variant_control(&self) -> Option<Arc<dyn VariantControl>> {
        Some(Arc::clone(&self.state) as Arc<dyn VariantControl>)
    }

    fn wait_range(
        &mut self,
        range: Range<u64>,
        _timeout: Option<Duration>,
        _cancel: Option<&kithara_stream::ReaderControl>,
    ) -> StreamResult<WaitOutcome> {
        self.state.waits.lock().push(range.clone());
        #[cfg(all(feature = "resample-rubato", feature = "symphonia"))]
        if _timeout.is_none()
            && let Some(native) = self.state.native.get()
            && !matches!(self.state.phase_at(range.clone()), SourcePhase::Ready)
        {
            let cancel = _cancel.expect("native stream wait receives its actual reader control");
            let mut revision = cancel.revision();
            cancel.set_pending(PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted));
            native.record_wait(range.clone());
            while !cancel.is_cancelled()
                && !matches!(self.state.phase_at(range.clone()), SourcePhase::Ready)
            {
                if !cancel.wait(revision) {
                    break;
                }
                revision = cancel.revision();
            }
            if cancel.is_cancelled() {
                return Err(StreamError::Source(SourceError::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    PendingReason::SessionRetired,
                ))));
            }
        }
        match self.state.phase_at(range) {
            SourcePhase::Ready => Ok(WaitOutcome::Ready),
            _ => Err(StreamError::Source(SourceError::WaitBudgetExceeded)),
        }
    }
}

struct SpliceConfig {
    state: Arc<SpliceState>,
}

impl Default for SpliceConfig {
    fn default() -> Self {
        Self {
            state: Arc::new(SpliceState::new(vec![
                build_variant_layout("slq", consts::SLQ_VARIANT),
                build_variant_layout("smq", consts::SMQ_VARIANT),
            ])),
        }
    }
}

struct SpliceStream;

impl StreamType for SpliceStream {
    type Config = SpliceConfig;
    type Events = ();
    type Source = SpliceSource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        Ok(SpliceSource::new(config.state))
    }
}

fn asset_bytes(name: &str) -> Vec<u8> {
    let route = format!("/hls/{name}");
    let resource = kithara_test_fixtures::hls::long_plain()
        .get(&route)
        .unwrap_or_else(|| panic!("generated HLS fixture has no `{route}`"));
    std::fs::read(resource.path())
        .unwrap_or_else(|error| panic!("read {}: {error}", resource.path().display()))
}

fn build_variant_layout(label: &str, variant_index: usize) -> VariantLayout {
    let playlist_name = format!("index-{label}-a1.m3u8");
    let playlist_url = Url::parse(&format!("https://fixture.invalid/hls/{playlist_name}"))
        .expect("fixture playlist URL is valid");
    let playlist = parse_media_playlist(playlist_url, &asset_bytes(&playlist_name))
        .expect("generated HLS media playlist is valid");
    assert!(
        playlist.segments.len() >= consts::TOTAL_SEGMENTS,
        "generated HLS fixture must contain {} segments",
        consts::TOTAL_SEGMENTS,
    );
    let init = asset_bytes(&format!("init-{label}-a1.mp4"));
    let init_len = u64::try_from(init.len()).expect("init length fits u64");
    let mut blob = init;
    let mut byte_cursor = init_len;
    let mut decode_time = Duration::ZERO;
    let mut segments = Vec::new();
    for (segment_index, segment) in playlist
        .segments
        .into_iter()
        .take(consts::TOTAL_SEGMENTS)
        .enumerate()
    {
        let bytes = asset_bytes(&segment.uri);
        let start = byte_cursor;
        let end = start + u64::try_from(bytes.len()).expect("segment length fits u64");
        let segment_index = u32::try_from(segment_index).expect("segment index fits u32");
        segments.push(
            SegmentDescriptor::builder()
                .byte_range(start..end)
                .decode_time(decode_time)
                .duration(segment.duration)
                .segment_index(segment_index)
                .variant_index(variant_index)
                .build(),
        );
        blob.extend_from_slice(&bytes);
        byte_cursor = end;
        decode_time = decode_time.saturating_add(segment.duration);
    }
    VariantLayout {
        blob,
        segments,
        init_range: 0..init_len,
    }
}

#[kithara::fixture]
fn slq_layout() -> VariantLayout {
    build_variant_layout("slq", consts::SLQ_VARIANT)
}

#[kithara::fixture]
fn smq_layout() -> VariantLayout {
    build_variant_layout("smq", consts::SMQ_VARIANT)
}

fn media_info(variant: usize) -> MediaInfo {
    let mut info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::AacLc))
        .maybe_container(Some(ContainerFormat::Fmp4))
        .build();
    info.variant_index = Some(u32::try_from(variant).expect("variant fits u32"));
    info
}

fn decoder_backend() -> kithara_decode::DecoderBackend {
    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    {
        kithara_decode::DecoderBackend::Apple
    }
    #[cfg(not(all(feature = "apple", any(target_os = "macos", target_os = "ios"))))]
    {
        kithara_decode::DecoderBackend::default()
    }
}

fn decoder_config<T: StreamType>(
    stream: &SharedStream<T>,
    backend: kithara_decode::DecoderBackend,
    byte_len: Arc<AtomicU64>,
    pools: &PoolRegion<TestPools>,
) -> DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> {
    byte_len.store(stream.len().unwrap_or(0), Ordering::Release);
    DecoderConfig::builder()
        .backend(backend)
        .pools(pools.clone())
        .byte_len_handle(byte_len)
        .maybe_byte_map(stream.byte_map())
        .gapless(false)
        .build()
}

struct SpliceFixture {
    state: Arc<SpliceState>,
    source: StreamAudioSource<SpliceStream>,
}

async fn splice_source(variants: Vec<VariantLayout>) -> SpliceFixture {
    let state = Arc::new(SpliceState::new(variants));
    let stream = Stream::<SpliceStream>::new(SpliceConfig {
        state: state.clone(),
    })
    .await
    .expect("create in-memory splice stream");
    let shared_stream = SharedStream::new(stream);
    let backend = decoder_backend();
    let pools = pools();
    let initial_byte_len = Arc::new(AtomicU64::new(0));
    let initial_decoder = DecodeFactory::create_from_media_info(
        shared_stream.clone(),
        &media_info(consts::SLQ_VARIANT),
        decoder_config(&shared_stream, backend, initial_byte_len, &pools),
    )
    .expect("create initial slq fMP4 decoder");
    let factory_byte_len = Arc::new(AtomicU64::new(0));
    let factory_pools = pools.clone();
    let decoder_factory = DecoderFactory::new(
        move |mut reader, info, _rate| {
            let byte_len = reader.byte_len().unwrap_or(0);
            factory_byte_len.store(byte_len, Ordering::Release);
            let config: DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> =
                DecoderConfig::builder()
                    .backend(backend)
                    .pools(factory_pools.clone())
                    .byte_len_handle(factory_byte_len.clone())
                    .maybe_byte_map(reader.byte_map())
                    .maybe_hooks(reader.take_event_sink())
                    .gapless(false)
                    .build();
            let input = reader.into_inner();
            let info = info.expect("splice reader carries media metadata");
            let decoder = DecodeFactory::create_from_media_info(input, &info, config)?;
            decoder.update_byte_len(byte_len);
            Ok(decoder)
        },
        None,
    );
    let decode = ActiveDecode::new(
        DecoderGeneration::new(
            initial_decoder,
            Some(media_info(consts::SLQ_VARIANT)),
            0,
            None,
            None,
            GaplessMode::Disabled,
        ),
        GaplessMode::Disabled,
        None,
        &pools,
    )
    .expect("decode scratch fits test pools");
    let source = StreamAudioSource::new(
        shared_stream,
        decode,
        SourceDecoderConfig {
            runtime: kithara_platform::tokio::runtime::Handle::try_current()
                .expect("source fixture runtime"),
            factory: decoder_factory,
            host_rate: NonZeroU32::new(consts::SAMPLE_RATE),
            backend,
            playback_resampler_backend: "none",
        },
        Arc::new(kithara_events::DeferredBus::new(
            kithara_events::EventBus::default(),
            16,
        )),
        Arc::new(NoopWorkerWake),
    );
    SpliceFixture { state, source }
}

fn run_pending_rebuild_inline(source: &mut StreamAudioSource<SpliceStream>) {
    source.prepare_deferred();
    source.finish_deferred();
}

#[cfg(all(feature = "resample-rubato", feature = "symphonia"))]
mod buffered_readiness {
    use std::{
        io::{self, Read, Seek, SeekFrom},
        num::NonZeroU32,
    };

    use kithara_decode::{DecoderResamplerConfig, DecoderSeekOutcome};
    use kithara_resampler::{ResamplerOptions, rubato::RubatoBackend};
    use kithara_stream::{
        ReaderChunkSignal, ReaderControl, ReaderEventSink, ReaderSeekSignal, SessionReader,
    };

    use super::*;
    use crate::pipeline::{
        decode::DecoderGeneration,
        seek::{ResumeState, ResumeTarget},
        source::core::OwnerPhase,
        track::WaitingReason,
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum InputOperation {
        Read,
        Seek,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) enum InputEvent {
        Opened(usize),
        Started(usize, InputOperation),
        Waiting(usize, InputOperation, Range<u64>),
        Cancelled(usize),
        Dropped(usize),
        Cursor(u64),
    }

    #[derive(Default)]
    pub(super) struct NativeReadState {
        readers: Vec<(kithara_platform::CancelToken, Option<NonZeroU32>)>,
        pub(super) events: Vec<InputEvent>,
        active: Option<(usize, InputOperation)>,
    }

    #[derive(Default)]
    pub(super) struct NativeReads {
        pub(super) state: Mutex<NativeReadState>,
        changed: kithara_platform::sync::Notify,
    }

    impl kithara_stream::WorkerWake for NativeReads {
        delegate::delegate! {
            to self.changed {
                #[call(notify_one)]
                fn defer(&self);
                #[call(notify_one)]
                fn wake(&self);
            }
        }
    }

    impl NativeReads {
        pub(super) fn record_wait(&self, range: Range<u64>) {
            {
                let mut state = self.state.lock();
                let (reader, operation) =
                    state.active.expect("native input operation owns this wait");
                state
                    .events
                    .push(InputEvent::Waiting(reader, operation, range));
            }
            self.changed.notify_one();
        }
    }

    struct NativeInput {
        input: Option<Box<dyn SessionReader>>,
        reader: usize,
        native: Arc<NativeReads>,
        _cancel: kithara_platform::CancelWakerGuard,
    }

    impl NativeInput {
        fn start(&self, operation: InputOperation) {
            let mut native = self.native.state.lock();
            assert_eq!(native.active.replace((self.reader, operation)), None);
            native
                .events
                .push(InputEvent::Started(self.reader, operation));
        }

        fn finish(&self, operation: InputOperation) {
            assert_eq!(
                self.native.state.lock().active.take(),
                Some((self.reader, operation))
            );
        }
    }

    impl Read for NativeInput {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.start(InputOperation::Read);
            let result = self.input.as_mut().expect("live native input").read(bytes);
            self.finish(InputOperation::Read);
            result
        }
    }

    impl Seek for NativeInput {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.start(InputOperation::Seek);
            let result = self
                .input
                .as_mut()
                .expect("live native input")
                .seek(position);
            self.finish(InputOperation::Seek);
            result
        }
    }

    impl Drop for NativeInput {
        fn drop(&mut self) {
            drop(self.input.take());
            self.native
                .state
                .lock()
                .events
                .push(InputEvent::Dropped(self.reader));
            self.native.changed.notify_one();
        }
    }

    fn native_factory(native: Arc<NativeReads>) -> DecoderFactory {
        let pools = pools();
        DecoderFactory::new(
            move |mut reader, info, rate| {
                let control = reader.control();
                control.set_worker_wake(native.clone());
                let number = {
                    let mut native = native.state.lock();
                    let number = native.readers.len();
                    native.readers.push((control.cancel_token(), rate));
                    native.events.push(InputEvent::Opened(number));
                    number
                };
                // The source keeps its byte map; omit it here to exercise the native MP4 parser.
                let config = DecoderConfig::builder()
                    .backend(kithara_decode::DecoderBackend::Symphonia)
                    .pools(pools.clone())
                    .byte_len_handle(Arc::new(AtomicU64::new(reader.byte_len().unwrap_or(0))))
                    .maybe_construction_gate(reader.construction_gate())
                    .reader_control(control)
                    .maybe_hooks(reader.take_event_sink())
                    .maybe_resampler(rate.map(|target| {
                        DecoderResamplerConfig::builder()
                            .backend(RubatoBackend::default())
                            .target_sample_rate(target)
                            .options(ResamplerOptions::builder().chunk_size(256).build())
                            .build()
                    }))
                    .gapless(false)
                    .build();
                let cancelled_native = Arc::downgrade(&native);
                let cancel = reader.control().cancel_token().on_cancel(move || {
                    if let Some(native) = cancelled_native.upgrade() {
                        native
                            .state
                            .lock()
                            .events
                            .push(InputEvent::Cancelled(number));
                        native.changed.notify_one();
                    }
                });
                let input = NativeInput {
                    input: Some(reader.into_inner()),
                    reader: number,
                    native: native.clone(),
                    _cancel: cancel,
                };
                assert!(info.is_none(), "native input requires probing");
                DecodeFactory::create_with_probe(input, Some("mp4"), config)
            },
            None,
        )
    }

    struct PendingSignals(Arc<AtomicUsize>);

    impl ReaderEventSink for PendingSignals {
        fn on_chunk(&mut self, signal: ReaderChunkSignal) {
            if matches!(signal, ReaderChunkSignal::Pending(_)) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        fn on_seek(&mut self, _signal: ReaderSeekSignal) {}
    }

    async fn buffered_source(resampled: bool) -> SpliceFixture {
        buffered_source_with_signals(resampled, Arc::new(AtomicUsize::new(0))).await
    }

    async fn buffered_source_with_signals(
        resampled: bool,
        signals: Arc<AtomicUsize>,
    ) -> SpliceFixture {
        buffered_source_with_trim(resampled, signals, None).await
    }

    async fn buffered_source_with_trim(
        resampled: bool,
        signals: Arc<AtomicUsize>,
        resume: Option<ResumeState>,
    ) -> SpliceFixture {
        let mut fixture = splice_source(vec![slq_layout()]).await;
        let stream = &fixture.source.shared_stream;
        let source_rate = fixture.source.decode.active().decoder().spec().sample_rate;
        assert_ne!(
            source_rate.get(),
            48_000,
            "fixture must require a real rate conversion"
        );
        stream.set_position(0);
        let builder = DecoderConfig::builder()
            .backend(kithara_decode::DecoderBackend::Symphonia)
            .pools(pools())
            .maybe_byte_map(stream.byte_map())
            .hooks(Box::new(PendingSignals(signals)))
            .gapless(false);
        let decoder = if resampled {
            let config = builder
                .resampler(
                    DecoderResamplerConfig::builder()
                        .backend(RubatoBackend::default())
                        .target_sample_rate(NonZeroU32::new(48_000).expect("target rate"))
                        .options(ResamplerOptions::builder().chunk_size(256).build())
                        .build(),
                )
                .build();
            DecodeFactory::create_from_media_info(
                stream.clone(),
                &media_info(consts::SLQ_VARIANT),
                config,
            )
            .expect("mismatch-rate fMP4 decoder")
        } else {
            DecodeFactory::create_from_media_info(
                stream.clone(),
                &media_info(consts::SLQ_VARIANT),
                builder.build(),
            )
            .expect("source-rate fMP4 decoder")
        };
        assert_eq!(
            decoder.spec().sample_rate.get(),
            if resampled { 48_000 } else { source_rate.get() }
        );
        let previous = fixture.source.decode.replace_active(DecoderGeneration::new(
            decoder,
            Some(media_info(consts::SLQ_VARIANT)),
            0,
            None,
            resume,
            GaplessMode::Disabled,
        ));
        drop(previous);
        let boundary = fixture.state.active_layout().segments[0].byte_range.end;
        fixture.state.available.store(boundary, Ordering::Release);
        fixture.state.waits.lock().clear();
        fixture
    }

    fn buffered_pcm(fixture: &mut SpliceFixture) -> AudioChunk {
        for _ in 0..16 {
            fixture.source.prepare_deferred();
            match fixture.source.step_track() {
                TrackStep::Produced(fetch) => {
                    let chunk = produced_data(fetch);
                    assert!(chunk.frames() > 0);
                    assert!(chunk.samples.iter().any(|sample| sample.abs() > 0.001));
                    return chunk;
                }
                TrackStep::StateChanged => {}
                _ => panic!("retained current-segment PCM must be produced"),
            }
        }
        panic!("prepared current-segment PCM was not delivered");
    }

    #[kithara::test(tokio)]
    async fn a_transient_narrow_gate_resumes_current_pcm_without_following_input() {
        let mut fixture = buffered_source(false).await;
        fixture.source.prepare_deferred();
        let boundary = fixture.state.active_layout().segments[0].byte_range.end;
        assert_eq!(fixture.source.shared_stream.position(), boundary);
        fixture
            .state
            .available
            .store(boundary - 1, Ordering::Release);
        assert_eq!(
            fixture.source.shared_stream.phase(),
            SourcePhase::WaitingDemand
        );
        let first = fixture.source.step_track();
        fixture.state.available.store(boundary, Ordering::Release);
        fixture.state.waits.lock().clear();

        let chunk = match first {
            TrackStep::Produced(fetch) => produced_data(fetch),
            TrackStep::Blocked(WaitingReason::WaitingDemand) => {
                assert!(fixture.source.waiting_for_source);
                buffered_pcm(&mut fixture)
            }
            _ => panic!("retained PCM must remain reachable after a narrow gate rejection"),
        };

        assert!(chunk.frames() > 0);
        assert!(chunk.meta.timestamp < Duration::from_secs(6));
        assert_eq!(fixture.state.available.load(Ordering::Acquire), boundary);
        assert_eq!(fixture.source.shared_stream.position(), boundary);
    }

    #[kithara::test(tokio)]
    #[case(false)]
    #[case(true)]
    async fn actual_input_wait_resumes_pcm_prepared_before_following_input(
        #[case] resampled: bool,
    ) {
        let mut fixture = buffered_source(resampled).await;
        let init_end = fixture.state.active_layout().init_range.end;
        let boundary = fixture.state.active_layout().segments[0].byte_range.end;
        fixture.state.available.store(init_end, Ordering::Release);
        fixture.source.prepare_deferred();
        assert!(matches!(fixture.source.step_track(), TrackStep::Blocked(_)));
        assert!(fixture.source.waiting_for_source);
        fixture.state.available.store(boundary, Ordering::Release);
        fixture.source.prepare_deferred();
        assert_eq!(fixture.source.shared_stream.position(), boundary);

        let chunk = buffered_pcm(&mut fixture);

        assert!(chunk.meta.timestamp < Duration::from_secs(6));
        assert_eq!(fixture.state.available.load(Ordering::Acquire), boundary);
        assert_eq!(fixture.source.shared_stream.position(), boundary);
    }

    #[kithara::test(tokio)]
    #[case(false)]
    #[case(true)]
    async fn pending_preparation_refreshes_after_live_shrink_without_following_input(
        #[case] resampled: bool,
    ) {
        let mut fixture = buffered_source(resampled).await;
        let boundary = fixture.state.active_layout().segments[0].byte_range.end;
        fixture.state.estimated_extra.store(16, Ordering::Release);
        fixture.source.prepare_deferred();
        assert_eq!(fixture.source.shared_stream.position(), boundary);
        assert!(matches!(
            fixture.source.step_track(),
            TrackStep::Blocked(WaitingReason::WaitingDemand)
        ));
        fixture.source.prepare_deferred();
        assert_eq!(fixture.source.shared_stream.position(), boundary);
        assert!(matches!(fixture.source.step_track(), TrackStep::Blocked(_)));
        fixture.source.prepare_deferred();
        fixture.state.estimated_extra.store(0, Ordering::Release);

        let chunk = buffered_pcm(&mut fixture);

        assert!(chunk.meta.timestamp < Duration::from_secs(6));
        assert_eq!(fixture.state.available.load(Ordering::Acquire), boundary);
        assert_eq!(fixture.source.shared_stream.position(), boundary);
    }

    #[kithara::test(tokio, tracing("kithara_audio=debug,kithara_decode=debug"))]
    #[case(false)]
    #[case(true)]
    async fn persistent_decoder_pending_parks_with_demand_and_peer_wake(#[case] resampled: bool) {
        let signals = Arc::new(AtomicUsize::new(0));
        let mut fixture = buffered_source_with_signals(resampled, Arc::clone(&signals)).await;
        let boundary = fixture.state.active_layout().segments[0].byte_range.end;
        fixture.state.estimated_extra.store(16, Ordering::Release);
        let promised = fixture
            .state
            .segment_at_index(0)
            .expect("published current segment");
        let missing_suffix = boundary..promised.byte_range.end;
        assert!(missing_suffix.start < missing_suffix.end);
        assert!(missing_suffix.end < fixture.state.active_layout().segments[1].byte_range.end);
        fixture.source.prepare_deferred();
        assert!(matches!(fixture.source.step_track(), TrackStep::Blocked(_)));
        for _ in 0..3 {
            fixture.state.waits.lock().clear();
            fixture.source.prepare_deferred();
            let consumed = signals.load(Ordering::Relaxed);
            let demands = fixture.state.waits.lock().clone();
            assert_eq!(demands.as_slice(), std::slice::from_ref(&missing_suffix));
            assert!(matches!(fixture.source.step_track(), TrackStep::Blocked(_)));
            assert!(matches!(fixture.source.step_track(), TrackStep::Blocked(_)));
            assert_eq!(
                signals.load(Ordering::Relaxed),
                consumed,
                "checked parked steps must not consume another Pending"
            );
            assert_eq!(fixture.state.waits.lock().as_slice(), demands.as_slice());
            assert!(fixture.source.waiting_for_source);
            assert!(fixture.state.peer_wake.flush());
            fixture.source.finish_deferred();
            assert_eq!(fixture.state.waits.lock().last(), Some(&missing_suffix),);
            assert_eq!(fixture.source.shared_stream.position(), boundary);
        }
        fixture.state.estimated_extra.store(0, Ordering::Release);
        let chunk = buffered_pcm(&mut fixture);
        assert_eq!(chunk.meta.segment_index, Some(0));
        assert!(chunk.meta.timestamp < Duration::from_secs(6));
        assert_eq!(fixture.state.available.load(Ordering::Acquire), boundary);
        assert_eq!(fixture.source.shared_stream.position(), boundary);
        drop(fixture);

        let mut fixture = splice_source(vec![slq_layout()]).await;
        let mut reference = splice_source(vec![slq_layout()]).await;
        let native = Arc::new(NativeReads::default());
        let reference_native = Arc::new(NativeReads::default());
        let initial_rate = if resampled {
            NonZeroU32::new(48_000).expect("resampled rate")
        } else {
            fixture.source.decode.output_spec().sample_rate
        };
        let final_rate = NonZeroU32::new(96_000).expect("final host rate");
        let final_target = Duration::from_secs(1);
        let mut initial_control: Option<ReaderControl> = None;
        let mut reference_landing = None;
        for (fixture, native, rate, landing) in [
            (&mut fixture, &native, initial_rate, None),
            (
                &mut reference,
                &reference_native,
                final_rate,
                Some(ResumeTarget::Position(final_target)),
            ),
        ] {
            assert!(fixture.state.native.set(native.clone()).is_ok());
            *fixture.state.media_info.lock() = None;
            fixture.source.shared_stream.set_position(0);
            fixture.source.factory = native_factory(native.clone());
            fixture.source.wake = native.clone();
            fixture.source.host_rate = Some(rate);
            fixture.source.decoder_backend = kithara_decode::DecoderBackend::Symphonia;
            fixture.source.playback_resampler_backend = "rubato";
            let reader = fixture.source.shared_stream.open_initial_reader();
            if landing.is_none() {
                initial_control = Some(reader.control());
            }
            let factory = fixture.source.factory.clone();
            let (generation, outcome) = kithara_platform::tokio::task::spawn_blocking(move || {
                factory.build_generation(
                    reader,
                    None,
                    0,
                    landing,
                    Some(rate),
                    GaplessMode::Disabled,
                )
            })
            .await
            .expect("native generation task completes")
            .expect("native fMP4 generation");
            if landing.is_some() {
                let Some(DecoderSeekOutcome::Landed { landed_at, .. }) = outcome else {
                    panic!("independent final-seek reference must land: {outcome:?}");
                };
                reference_landing = Some(landed_at);
            }
            fixture
                .source
                .decode
                .prepare_replacement_profile(generation.spec());
            assert!(fixture.source.decode.take_stage_error().is_none());
            drop(fixture.source.decode.replace_active(generation));
            fixture.source.decode.reset();
            let boundary = fixture.state.active_layout().segments[0].byte_range.end;
            fixture.state.available.store(boundary, Ordering::Release);
        }
        let boundary = fixture.state.active_layout().segments[0].byte_range.end;
        kithara_platform::time::timeout(Duration::from_secs(5), async {
            loop {
                let changed = native.changed.notified();
                fixture.source.prepare_deferred();
                let step = fixture.source.step_track();
                assert!(!matches!(&step, TrackStep::Failed(_) | TrackStep::Eof));
                fixture.source.finish_deferred();
                if native.state.lock().events.iter().any(|event| matches!(
                    event,
                    InputEvent::Waiting(0, InputOperation::Read, range) if range.start >= boundary
                )) {
                    break;
                }
                if matches!(step, TrackStep::Blocked(_)) {
                    changed.await;
                }
            }
        })
        .await
        .expect("the actual native Read must park on withheld input");
        let initial_control = initial_control.expect("actual initial reader control");
        assert_eq!(
            initial_control.pending_reason(),
            Some(PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted))
        );
        let command_start = native.state.lock().events.len();
        let mut audio_events = fixture.source.emit.bus().subscribe::<crate::AudioEvent>();
        let first_target =
            fixture.state.active_layout().segments[1].decode_time + Duration::from_secs(1);
        assert!(matches!(
            fixture.source.seek(first_target),
            Ok(crate::SeekOutcome::Pending { target, .. }) if target == first_target
        ));
        assert!(initial_control.is_cancelled());
        kithara_platform::time::timeout(Duration::from_secs(5), async {
            loop {
                let changed = native.changed.notified();
                fixture.source.prepare_deferred();
                fixture.source.finish_deferred();
                assert!(!matches!(fixture.source.phase, OwnerPhase::Failed { .. }));
                if native.state.lock().events.iter().any(|event| {
                    matches!(
                        event,
                        InputEvent::Waiting(reader, _, range) if *reader > 0 && range.end > boundary
                    )
                }) {
                    break;
                }
                changed.await;
            }
        })
        .await
        .expect("the real replacement must reach withheld input");
        let blocked_control = native
            .state
            .lock()
            .readers
            .last()
            .expect("replacement input")
            .0
            .clone();
        assert!(!blocked_control.is_cancelled());
        fixture
            .source
            .set_host_sample_rate(NonZeroU32::new(32_000).expect("superseded host rate"));
        assert!(matches!(
            fixture.source.seek(final_target),
            Ok(crate::SeekOutcome::Pending { target, .. }) if target == final_target
        ));
        fixture.source.set_host_sample_rate(final_rate);
        assert!(blocked_control.is_cancelled());
        assert!(matches!(
            fixture.source.resume_target(),
            ResumeTarget::Position(target) if target == final_target
        ));
        let actual = kithara_platform::time::timeout(Duration::from_secs(5), async {
            loop {
                let changed = native.changed.notified();
                fixture.source.prepare_deferred();
                let step = fixture.source.step_track();
                fixture.source.finish_deferred();
                match step {
                    TrackStep::Produced(fetch) => break produced_data(fetch),
                    TrackStep::StateChanged | TrackStep::Blocked(_) => changed.await,
                    TrackStep::Failed(failure) => {
                        panic!("latest native seek/rate failed: {failure:?}")
                    }
                    TrackStep::Eof => panic!("latest native seek/rate reached EOF before PCM"),
                }
            }
        })
        .await
        .expect("latest command settles without releasing any withheld input");
        let expected = kithara_platform::time::timeout(Duration::from_secs(5), async {
            loop {
                let changed = reference_native.changed.notified();
                reference.source.prepare_deferred();
                match reference.source.step_track() {
                    TrackStep::Produced(fetch) => break produced_data(fetch),
                    TrackStep::StateChanged | TrackStep::Blocked(_) => changed.await,
                    _ => panic!("independent native reference must produce PCM"),
                }
            }
        })
        .await
        .expect("independent final-seek reference");
        assert_eq!(fixture.source.host_sample_rate(), Some(final_rate));
        assert_eq!(fixture.source.decode.output_spec().sample_rate, final_rate);
        assert_eq!(
            fixture.source.playhead.position(),
            reference_landing.expect("independent decoder landing")
        );
        assert_eq!(actual.spec(), expected.spec());
        assert_eq!(actual.spec().sample_rate, final_rate);
        assert_eq!(actual.meta.timestamp, final_target);
        assert_eq!(
            actual.meta.frame_offset,
            actual
                .spec()
                .frame_at(final_target)
                .expect("final PCM frame")
        );
        assert_eq!(actual.meta.timestamp, expected.meta.timestamp);
        assert_eq!(actual.meta.frame_offset, expected.meta.frame_offset);
        assert_eq!(actual.meta.segment_index, expected.meta.segment_index);
        assert_eq!(actual.samples.as_ref(), expected.samples.as_ref());
        assert!(actual.frames() > 0 && actual.samples.iter().any(|sample| sample.abs() > 0.001));
        assert_eq!(fixture.state.available.load(Ordering::Acquire), boundary);
        let mut stages = Vec::new();
        while let Ok(envelope) = audio_events.try_recv() {
            match envelope.event {
                crate::AudioEvent::SeekLifecycle { stage, .. } => stages.push(stage),
                crate::AudioEvent::SeekRejected { target } => {
                    panic!("native seek rejected: {target:?}")
                }
                _ => {}
            }
        }
        assert_eq!(
            stages,
            [
                crate::SeekLifecycleStage::SeekRequest,
                crate::SeekLifecycleStage::SeekRequest,
                crate::SeekLifecycleStage::SeekApplied,
            ]
        );
        let native = native.state.lock();
        assert!(native.readers.len() >= 3);
        assert_eq!(
            native.readers.last().expect("latest reader").1,
            Some(final_rate)
        );
        for reader in 0..native.readers.len() - 1 {
            assert!(native.readers[reader].0.is_cancelled());
            let cancelled = native
                .events
                .iter()
                .position(|event| *event == InputEvent::Cancelled(reader))
                .expect("old actual reader was cancelled");
            let retired = native
                .events
                .iter()
                .position(|event| *event == InputEvent::Dropped(reader))
                .expect("old actual input was dropped");
            let next_open = native
                .events
                .iter()
                .position(|event| *event == InputEvent::Opened(reader + 1))
                .expect("next actual input was opened");
            assert!(command_start <= cancelled && cancelled < retired && retired < next_open);
            let mutation = native
                .events
                .iter()
                .enumerate()
                .skip(cancelled + 1)
                .find(|(_, event)| matches!(event, InputEvent::Cursor(_)))
                .expect("next command changes the shared cursor")
                .0;
            assert!(
                retired < mutation,
                "old input drop precedes command cursor mutation"
            );
        }
        drop(native);
    }

    #[kithara::test(tokio)]
    #[case(false)]
    #[case(true)]
    async fn post_seek_pending_keeps_head_trim_until_buffered_pcm_resumes(#[case] resampled: bool) {
        let mut reference = buffered_source(resampled).await;
        let reference_chunk = buffered_pcm(&mut reference);
        let drop_frames = reference_chunk.frames() / 2;
        let drop_frames_u64 = u64::try_from(drop_frames).expect("fixture PCM frame count");
        let expected_frame = reference_chunk.meta.frame_offset + drop_frames_u64;
        let spec = reference_chunk.spec();
        let origin = reference_chunk.meta.timestamp
            - spec
                .duration_for(reference_chunk.meta.frame_offset)
                .expect("reference PCM coordinate");
        let target = origin
            + spec
                .duration_for(expected_frame)
                .expect("retained PCM coordinate");
        assert!(drop_frames > 0 && drop_frames < reference_chunk.frames());
        let mut fixture = buffered_source_with_trim(
            resampled,
            Arc::new(AtomicUsize::new(0)),
            Some(ResumeState {
                target: ResumeTarget::Position(target),
                trim_head: true,
            }),
        )
        .await;
        fixture.state.estimated_extra.store(16, Ordering::Release);
        fixture.source.prepare_deferred();
        assert!(matches!(fixture.source.step_track(), TrackStep::Blocked(_)));
        assert!(fixture.source.waiting_for_source);
        fixture.source.prepare_deferred();
        fixture.state.estimated_extra.store(0, Ordering::Release);

        let chunk = buffered_pcm(&mut fixture);

        assert_eq!(chunk.frames(), reference_chunk.frames() - drop_frames);
        assert_eq!(chunk.meta.frame_offset, expected_frame);
        assert_eq!(chunk.meta.timestamp, target);
        let drop_samples = drop_frames * usize::from(chunk.spec().channels);
        assert_eq!(
            chunk.samples.as_ref(),
            &reference_chunk.samples[drop_samples..]
        );
        assert_eq!(
            fixture.state.available.load(Ordering::Acquire),
            fixture.source.shared_stream.position()
        );
    }

    #[kithara::test(tokio)]
    async fn cancellation_preempts_retained_current_pcm() {
        let mut fixture = buffered_source(false).await;
        fixture.source.prepare_deferred();
        let boundary = fixture.state.active_layout().segments[0].byte_range.end;
        assert_eq!(fixture.source.shared_stream.position(), boundary);
        fixture.state.cancelled.store(true, Ordering::Release);

        assert!(matches!(
            fixture.source.step_track(),
            TrackStep::Failed(crate::TrackFailureKind::SourceCancelled)
        ));
        assert!(matches!(fixture.source.phase, OwnerPhase::Failed { .. }));
    }
}

fn append_left_channel(left: &mut Vec<f32>, chunk: &AudioChunk) {
    let channels = usize::from(chunk.meta.spec.channels);
    assert_eq!(
        channels,
        consts::SPLICE_CHANNELS,
        "AAC fixture should be stereo"
    );
    for frame in 0..chunk.frames() {
        left.push(chunk.samples[frame * channels]);
    }
}

fn peak_first_diff(left: &[f32], center: usize, half: usize) -> f32 {
    assert!(
        (1..left.len()).contains(&center),
        "first-difference center must be in 1..{}, got {center}",
        left.len(),
    );
    let start = center.saturating_sub(half).max(1);
    let end = center.saturating_add(half).min(left.len() - 1);
    let mut peak = 0.0_f32;
    for i in start..=end {
        let diff = (left[i] - left[i - 1]).abs();
        peak = peak.max(diff);
    }
    peak
}

fn segment_boundary_frame(state: &SpliceState, segment: usize) -> usize {
    let boundary = state
        .active_layout()
        .segments
        .get(segment)
        .expect("segment boundary exists in fixture");
    let frames = boundary.decode_time.as_secs_f64() * f64::from(consts::SAMPLE_RATE);
    num_traits::cast(frames.round()).expect("segment boundary frame fits usize")
}

fn segment_end(state: &SpliceState, segment: u32) -> Duration {
    let index = usize::try_from(segment).expect("segment index fits usize");
    let descriptor = state
        .active_layout()
        .segments
        .get(index)
        .expect("splice segment exists in fixture");
    descriptor.decode_time.saturating_add(descriptor.duration)
}

/// splice-continuity contract: RED = audible click on variant switch
#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn hls_aac_lc_abr_variant_switch_splice_continuity_metric(
    slq_layout: VariantLayout,
    smq_layout: VariantLayout,
) {
    let SpliceFixture { mut source, state } = splice_source(vec![slq_layout, smq_layout]).await;
    let splice_time = segment_end(&state, consts::SPLICE_SEGMENT - 1);
    let capture_frames = segment_boundary_frame(&state, consts::CAPTURE_END_SEGMENT);
    let mut left = Vec::with_capacity(capture_frames);
    let mut switched = false;
    let mut splice_frame = None;
    let mut last_segment = None;
    let mut last_end = Duration::ZERO;

    while left.len() < capture_frames {
        hang_tick!();
        run_pending_rebuild_inline(&mut source);
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                last_segment = chunk.meta.segment_index;
                last_end = chunk.meta.end_timestamp;
                append_left_channel(&mut left, &chunk);
                if chunk.frames() != 0 {
                    hang_reset!();
                }
                source
                    .playhead
                    .advance(&crate::audio::chunk_position(&chunk.meta));
                if !switched && chunk.meta.end_timestamp >= splice_time {
                    state.switch_to(consts::SMQ_VARIANT);
                    switched = true;
                    splice_frame = Some(left.len());
                }
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            TrackStep::Eof => break,
            TrackStep::Failed(_) => panic!("splice source failed before metric collection"),
        }
    }

    assert!(
        switched,
        "test must trigger the slq -> smq splice: rendered={} last_segment={last_segment:?} last_end={last_end:?}",
        left.len(),
    );
    let splice_frame = splice_frame.expect("splice frame should be captured");
    if let Some(landing) = state.warmup_landing() {
        println!(
            "SPLICE_WARMUP variant={} segment={}",
            landing.variant_index, landing.segment_index
        );
    }
    let switch_peak = peak_first_diff(&left, splice_frame, 64);
    let mut control_peak = 0.0_f32;
    let mut control_count = 0usize;
    for k in 1..consts::TOTAL_SEGMENTS {
        let boundary = segment_boundary_frame(&state, k);
        if boundary >= left.len() || boundary.abs_diff(splice_frame) <= 4096 {
            continue;
        }
        control_peak = control_peak.max(peak_first_diff(&left, boundary, 64));
        control_count += 1;
    }
    assert!(
        control_count >= 2,
        "splice continuity metric needs at least two same-variant control boundaries, got {control_count}",
    );
    let ratio = switch_peak / control_peak.max(f32::EPSILON);
    println!(
        "SPLICE_CONTINUITY switch_peak={switch_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
    );
    assert!(
        ratio < 3.0,
        "variant-switch stitch discontinuity {switch_peak:.6} is {ratio:.1}x the worst same-variant segment boundary {control_peak:.6} — audible click at the splice",
    );
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn hls_aac_lc_same_variant_recreate_continuity_metric(slq_layout: VariantLayout) {
    let SpliceFixture { mut source, state } = splice_source(vec![slq_layout]).await;
    let recreate_after = segment_end(&state, consts::SPLICE_SEGMENT - 1);
    let capture_frames = segment_boundary_frame(&state, consts::CAPTURE_END_SEGMENT);
    let mut left = Vec::with_capacity(capture_frames);
    let mut recreated = false;
    let mut recreate_frame = None;
    let mut last_segment = None;
    let mut last_end = Duration::ZERO;

    while left.len() < capture_frames {
        hang_tick!();
        run_pending_rebuild_inline(&mut source);
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                last_segment = chunk.meta.segment_index;
                last_end = chunk.meta.end_timestamp;
                append_left_channel(&mut left, &chunk);
                if chunk.frames() != 0 {
                    hang_reset!();
                }
                source
                    .playhead
                    .advance(&crate::audio::chunk_position(&chunk.meta));
                if !recreated && chunk.meta.end_timestamp >= recreate_after {
                    let active = state.active_index();
                    assert_eq!(
                        active,
                        consts::SLQ_VARIANT,
                        "same-variant recreate test must stay on the SLQ variant",
                    );
                    source
                        .install_replacement(
                            RecreateState {
                                cause: RecreateCause::VariantSwitch,
                                media_info: Some(media_info(active)),
                                offset: state.active_layout().init_range.start,
                            },
                            Some(crate::pipeline::seek::ResumeTarget::Source(
                                crate::SourceEnd::new(
                                    chunk.meta.frame_offset + u64::from(chunk.meta.frames),
                                    chunk.meta.spec.sample_rate,
                                ),
                            )),
                        )
                        .expect("same-variant replacement");
                    recreated = true;
                    recreate_frame = Some(left.len());
                }
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            TrackStep::Eof => break,
            TrackStep::Failed(_) => {
                panic!("same-variant recreate source failed before metric collection");
            }
        }
    }

    assert!(
        recreated,
        "test must trigger the same-variant recreate: rendered={} last_segment={last_segment:?} last_end={last_end:?}",
        left.len(),
    );
    assert_eq!(
        state.active_index(),
        consts::SLQ_VARIANT,
        "same-variant recreate must not change the active variant",
    );
    let recreate_frame = recreate_frame.expect("recreate frame should be captured");
    let recreate_peak = peak_first_diff(&left, recreate_frame, 64);
    let mut control_peak = 0.0_f32;
    let mut control_count = 0usize;
    for k in 1..consts::TOTAL_SEGMENTS {
        let boundary = segment_boundary_frame(&state, k);
        if boundary >= left.len() || boundary.abs_diff(recreate_frame) <= 4096 {
            continue;
        }
        control_peak = control_peak.max(peak_first_diff(&left, boundary, 64));
        control_count += 1;
    }
    assert!(
        control_count >= 2,
        "recreate continuity metric needs at least two same-content control boundaries, got {control_count}",
    );
    let ratio = recreate_peak / control_peak.max(f32::EPSILON);
    println!(
        "RECREATE_CONTINUITY recreate_peak={recreate_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
    );
    assert!(
        ratio < 3.0,
        "same-variant recreate discontinuity {recreate_peak:.6} is {ratio:.1}x the worst same-content segment boundary {control_peak:.6}",
    );
}
