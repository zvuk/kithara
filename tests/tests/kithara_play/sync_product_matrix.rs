#![cfg(not(target_arch = "wasm32"))]

use std::{env, io, num::NonZeroU32, path::Path};

use kithara::{
    assets::{AssetResource, AssetResourceState, AssetSource, AssetStore, ReadSide, ResourceKey},
    encode::EncodeConfig,
    events::TrackStatus,
    hls::AbrMode,
    host::{Host, HostConfig, HostOwned, testing::HostProbe},
    output::{OfflineRenderRequest, OfflineRenderer, RenderSink, RenderSinkError},
    platform::{
        CancelScope,
        sync::Arc,
        time::{self, Duration, Instant},
    },
    play::{
        PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc, Tempo,
    },
    queue::{Queue, QueueConfig, TrackSource, Transition},
    record::{RecordingConfig, RecordingCore, RecordingSink},
    signal::AudioSpec,
    warp::{
        AlignmentSource, LoadGeneration, PresentationFrontier, SessionFrame, SyncAdmission,
        SyncGroup, SyncIntent, SyncOperation,
    },
};
use kithara_app::recording::AssetPartSink;
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    bufpool_ext::{TestPools, pools},
    cochlea::{marked_synchronization_failures, synchronization_failures},
    fixture_protocol::EncryptionRequest,
    hls_fixture::{aes128_iv, aes128_key_bytes},
    kithara, memory_asset_store,
};
use kithara_test_fixtures::{
    asset::Asset,
    assets::{
        by_name, rhythm_fmp4_init_deck_a_120bpm_48k, rhythm_fmp4_media_deck_a_120bpm_48k,
        rhythm_mp3_deck_a_120bpm_48k, rhythm_mp3_deck_b_120bpm_48k, rhythm_wav_deck_a_120bpm_48k,
        rhythm_wav_deck_b_120bpm_48k, rhythm_wav_deck_c_120bpm_48k, rhythm_wav_deck_d_120bpm_48k,
        signal_mp3_sweep_up_60s,
    },
};

pub(super) const BLOCK_FRAMES: usize = 512;
pub(super) const CHANNELS: u16 = 2;
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const START_BPM: f64 = 120.0;

#[derive(Clone, Copy, Debug)]
enum Operation {
    Play,
    Seek,
    Sync,
}

#[derive(Clone, Copy, Debug)]
enum OperationOrder {
    PlaySyncSeek,
    PlaySeekSync,
    SeekPlaySync,
    SeekSyncPlay,
    SyncPlaySeek,
    SyncSeekPlay,
    SequentialSync,
}

impl OperationOrder {
    const fn operations(self) -> &'static [Operation] {
        match self {
            Self::PlaySyncSeek | Self::SequentialSync => {
                &[Operation::Play, Operation::Sync, Operation::Seek]
            }
            Self::PlaySeekSync => &[Operation::Play, Operation::Seek, Operation::Sync],
            Self::SeekPlaySync => &[Operation::Seek, Operation::Play, Operation::Sync],
            Self::SeekSyncPlay => &[Operation::Seek, Operation::Sync, Operation::Play],
            Self::SyncPlaySeek => &[Operation::Sync, Operation::Play, Operation::Seek],
            Self::SyncSeekPlay => &[Operation::Sync, Operation::Seek, Operation::Play],
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum TempoRide {
    Down,
    Hold(f64),
    Triangle,
    Up,
}

impl TempoRide {
    const fn points(self) -> &'static [f64] {
        match self {
            Self::Down => &[116.0, 112.0, 108.0],
            Self::Hold(_) => &[],
            Self::Triangle => &[116.0, 112.0, 116.0, 120.0],
            Self::Up => &[122.0, 125.0, 127.0],
        }
    }

    const fn start_bpm(self) -> f64 {
        match self {
            Self::Hold(bpm) => bpm,
            Self::Down | Self::Triangle | Self::Up => START_BPM,
        }
    }

    const fn final_bpm(self) -> f64 {
        match self {
            Self::Down => 108.0,
            Self::Hold(bpm) => bpm,
            Self::Triangle => 120.0,
            Self::Up => 127.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SyncCase {
    id: &'static str,
    decks: usize,
    pub(super) sample_rate: u32,
    order: OperationOrder,
    paused: bool,
    ride: TempoRide,
    updates_hz: u32,
}

impl SyncCase {
    const fn running(
        id: &'static str,
        decks: usize,
        sample_rate: u32,
        order: OperationOrder,
    ) -> Self {
        Self {
            id,
            decks,
            sample_rate,
            order,
            paused: false,
            ride: TempoRide::Triangle,
            updates_hz: 60,
        }
    }

    const fn paused(mut self) -> Self {
        self.paused = true;
        self
    }

    const fn ride(mut self, ride: TempoRide, updates_hz: u32) -> Self {
        self.ride = ride;
        self.updates_hz = updates_hz;
        self
    }

    const fn hold(mut self, bpm: f64) -> Self {
        self.ride = TempoRide::Hold(bpm);
        self
    }

    pub(super) const fn decks(self) -> usize {
        self.decks
    }

    pub(super) const fn id(self) -> &'static str {
        self.id
    }

    const fn start_bpm(self) -> f64 {
        self.ride.start_bpm()
    }

    pub(super) const fn final_bpm(self) -> f64 {
        self.ride.final_bpm()
    }
}

const PLAY_SYNC_SEEK: SyncCase =
    SyncCase::running("play-sync-seek", 2, 48_000, OperationOrder::PlaySyncSeek);
const PLAY_SEEK_SYNC: SyncCase =
    SyncCase::running("play-seek-sync", 2, 44_100, OperationOrder::PlaySeekSync)
        .ride(TempoRide::Up, 30);
const SEEK_PLAY_SYNC: SyncCase =
    SyncCase::running("seek-play-sync", 2, 48_000, OperationOrder::SeekPlaySync)
        .ride(TempoRide::Down, 60);
const SEEK_SYNC_PLAY: SyncCase =
    SyncCase::running("seek-sync-play", 2, 44_100, OperationOrder::SeekSyncPlay)
        .ride(TempoRide::Triangle, 30);
const SYNC_PLAY_SEEK: SyncCase =
    SyncCase::running("sync-play-seek", 2, 48_000, OperationOrder::SyncPlaySeek)
        .ride(TempoRide::Up, 60);
const SYNC_SEEK_PLAY: SyncCase =
    SyncCase::running("sync-seek-play", 2, 44_100, OperationOrder::SyncSeekPlay)
        .ride(TempoRide::Down, 120);
pub(super) const SEQUENTIAL_SYNC: SyncCase =
    SyncCase::running("sequential-sync", 2, 48_000, OperationOrder::SequentialSync);
const PAUSED_SYNC: SyncCase = SyncCase::running(
    "paused-sync-then-play",
    2,
    48_000,
    OperationOrder::SyncPlaySeek,
)
.paused();
const FOUR_DECK_SYNC: SyncCase = SyncCase::running(
    "four-deck-sequential-sync",
    4,
    48_000,
    OperationOrder::SequentialSync,
);
const TEMPO_UP_120: SyncCase =
    SyncCase::running("tempo-up-120hz", 2, 48_000, OperationOrder::PlaySyncSeek)
        .ride(TempoRide::Up, 120);
const TEMPO_DOWN_30: SyncCase =
    SyncCase::running("tempo-down-30hz", 2, 44_100, OperationOrder::PlaySyncSeek)
        .ride(TempoRide::Down, 30);
pub(super) const ONE_DECK: SyncCase =
    SyncCase::running("one-deck-runtime", 1, 48_000, OperationOrder::PlaySyncSeek);
pub(super) const SHARED_DEADLINE: SyncCase = SyncCase::running(
    "shared-worker-deadline",
    4,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.ride(TempoRide::Up, 120);
pub(super) const SHARED_DEADLINE_CONTROL: SyncCase = SyncCase::running(
    "shared-worker-control",
    1,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.ride(TempoRide::Up, 120);

pub(super) const AMBIENT_TRIP_HOP_SYNC: SyncCase = SyncCase::running(
    "ambient-dub-62-to-trip-hop-74",
    2,
    48_000,
    OperationOrder::SequentialSync,
)
.hold(74.0);
pub(super) const DOWNTEMPO_HOUSE_SYNC: SyncCase = SyncCase::running(
    "downtempo-96-to-house-124",
    2,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.hold(124.0);
pub(super) const TECHNO_BREAKBEAT_SYNC: SyncCase = SyncCase::running(
    "techno-132-to-breakbeat-140",
    2,
    48_000,
    OperationOrder::SeekPlaySync,
)
.hold(132.0);
pub(super) const CROSS_STYLE_SYNC: SyncCase = SyncCase::running(
    "cross-style-four-deck-124",
    4,
    48_000,
    OperationOrder::SequentialSync,
)
.hold(124.0);

const AMBIENT_TRIP_HOP: &[&str] = &[
    "rhythm_wav_ambient_dub_62_aligned",
    "rhythm_wav_trip_hop_74_aligned",
];
const DOWNTEMPO_HOUSE: &[&str] = &[
    "rhythm_wav_downtempo_96_aligned",
    "rhythm_wav_house_124_aligned",
];
const TECHNO_BREAKBEAT: &[&str] = &[
    "rhythm_wav_techno_132_aligned",
    "rhythm_wav_breakbeat_140_aligned",
];
const CROSS_STYLE: &[&str] = &[
    "rhythm_wav_ambient_dub_62_aligned",
    "rhythm_wav_downtempo_96_aligned",
    "rhythm_wav_house_124_aligned",
    "rhythm_wav_breakbeat_140_aligned",
];

#[derive(Clone, Copy, Debug)]
pub(super) enum Provider {
    Synthetic,
    Rhythm(&'static [&'static str]),
    HlsSame(HlsProtection),
    Library,
    Mp3Same,
    Mp3Distinct,
    HlsMp3(HlsProtection),
    Sweep,
}

pub(super) const AMBIENT_TRIP_HOP_PROVIDER: Provider = Provider::Rhythm(AMBIENT_TRIP_HOP);
pub(super) const DOWNTEMPO_HOUSE_PROVIDER: Provider = Provider::Rhythm(DOWNTEMPO_HOUSE);
pub(super) const TECHNO_BREAKBEAT_PROVIDER: Provider = Provider::Rhythm(TECHNO_BREAKBEAT);
pub(super) const CROSS_STYLE_PROVIDER: Provider = Provider::Rhythm(CROSS_STYLE);

impl Provider {
    const fn has_score_markers(self) -> bool {
        matches!(self, Self::Rhythm(_))
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum HlsProtection {
    Plain,
    Drm,
}

pub(super) struct ProductHarness {
    pub(super) decks: Vec<HostOwned<Queue<TestPools>>>,
    pub(super) failures: Vec<String>,
    block_frames: usize,
    host: Host<TestPools>,
    spec: AudioSpec,
    output_frames: u64,
    paced: bool,
}

#[derive(Default)]
struct VecRenderSink {
    samples: Vec<f32>,
}

impl RenderSink for VecRenderSink {
    fn write(&mut self, samples: &[f32]) -> Result<(), RenderSinkError> {
        self.samples.extend_from_slice(samples);
        Ok(())
    }
}

struct FailingPartSink(AssetPartSink<TestPools>);

impl RecordingSink for FailingPartSink {
    type Error = io::Error;
    type Output = ();

    fn write_at(&mut self, _offset: u64, _bytes: &[u8]) -> Result<(), Self::Error> {
        Err(io::Error::other("injected recording sink failure"))
    }

    fn commit(&mut self, _final_len: u64) -> Result<Self::Output, Self::Error> {
        Err(io::Error::other("injected recording sink commit"))
    }

    fn abort(&mut self) {
        self.0.abort();
    }
}

struct OfflineRecordingArtifact;

fn recording_key(store: &AssetStore<TestPools>, name: &str) -> ResourceKey {
    let source = AssetSource::Local {
        path: env::temp_dir().join("kithara-offline-rendering-test"),
    };
    store
        .scope::<OfflineRecordingArtifact>(&source)
        .and_then(|scope| {
            scope.key(&AssetResource::Named {
                namespace: "offline-rendering".to_owned(),
                name: name.to_owned(),
            })
        })
        .unwrap_or_else(|error| panic!("offline recording key: {error}"))
}

fn recording_config(sample_rate: u32, packet_frames: usize) -> RecordingConfig {
    RecordingConfig::builder()
        .encode(
            EncodeConfig::builder()
                .sample_rate(sample_rate)
                .channels(CHANNELS)
                .packet_frames(packet_frames)
                .build(),
        )
        .build()
}

fn offline_render(sample_rate: NonZeroU32, frames: u64) -> (Host<TestPools>, OfflineRenderRequest) {
    let spec = AudioSpec::new(CHANNELS, sample_rate);
    let session = HostConfig::offline(pools())
        .sample_rate(sample_rate)
        .build();
    let host = Host::new(session).unwrap_or_else(|error| panic!("create offline Host: {error}"));
    let request = OfflineRenderRequest::builder()
        .spec(spec)
        .frames(0..frames)
        .build();
    (host, request)
}

impl ProductHarness {
    pub(super) async fn new(case: SyncCase, provider: Provider, audible_deck: usize) -> Self {
        Self::build(case, provider, audible_deck, BLOCK_FRAMES, false).await
    }

    pub(super) async fn new_for_block(
        case: SyncCase,
        provider: Provider,
        audible_deck: usize,
        block_frames: usize,
    ) -> Self {
        Self::build(case, provider, audible_deck, block_frames, true).await
    }

    async fn build(
        case: SyncCase,
        provider: Provider,
        audible_deck: usize,
        block_frames: usize,
        paced: bool,
    ) -> Self {
        let server = TestServerHelper::new().await;
        let sources = sources(provider, case.decks, &server).await;
        let pools = pools();
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
        let sample_rate = NonZeroU32::new(case.sample_rate).expect("fixture sample rate");
        let render_block_frames = NonZeroU32::new(
            u32::try_from(block_frames).expect("offline render block count fits u32"),
        )
        .expect("offline render block count is non-zero");
        let spec = AudioSpec::new(CHANNELS, sample_rate);
        let session = HostConfig::offline(pools)
            .sample_rate(sample_rate)
            .max_block_frames(render_block_frames)
            .build();
        let mut host = Host::new(session)
            .unwrap_or_else(|error| panic!("{}: create offline Host: {error}", case.id));
        let mut decks = Vec::with_capacity(sources.len());
        let mut ids = Vec::with_capacity(sources.len());
        for (index, source) in sources.into_iter().enumerate() {
            let player = PlayerImpl::new(
                PlayerConfig::builder()
                    .worker(worker.clone())
                    .sample_rate(sample_rate)
                    .crossfade_duration(0.0)
                    .build(),
            );
            let queue = Queue::new(QueueConfig::builder().player(player).build());
            queue.set_muted(index != audible_deck);
            let deck = host
                .insert(queue)
                .unwrap_or_else(|error| panic!("{}: insert deck {index}: {error}", case.id));
            let config = ResourceConfig::for_src(
                ResourceSrc::parse(&source)
                    .unwrap_or_else(|error| panic!("{}: parse source {source}: {error}", case.id)),
            )
            .store(memory_asset_store())
            .initial_abr_mode(AbrMode::manual(0))
            .discriminator(format!("{}-{provider:?}-{index}", case.id))
            .build();
            let id = deck
                .append(TrackSource::Config(Box::new(config)))
                .unwrap_or_else(|error| panic!("{}: append deck {index}: {error}", case.id));
            decks.push(deck);
            ids.push(id);
        }
        let mut harness = Self {
            decks,
            failures: Vec::new(),
            block_frames,
            host,
            output_frames: 0,
            paced,
            spec,
        };
        harness.wait_loaded(case, &ids).await;
        for (index, (deck, id)) in harness.decks.iter().zip(ids).enumerate() {
            deck.select(id, Transition::None)
                .unwrap_or_else(|error| panic!("{}: select deck {index}: {error}", case.id));
        }
        harness.set_tempo(case, case.start_bpm(), true);
        let _ = harness.render(case, harness.block_frames).await;
        if !case.paused {
            harness.start_staggered(case).await;
        }
        harness
    }

    async fn wait_loaded(&mut self, case: SyncCase, ids: &[kithara::events::TrackId]) {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        loop {
            self.tick_all(case);
            let mut loaded = true;
            for (index, (deck, id)) in self.decks.iter().zip(ids).enumerate() {
                match deck.track(*id).map(|track| track.status) {
                    Some(TrackStatus::Loaded) => {}
                    Some(TrackStatus::Failed(error)) => {
                        panic!("{}: deck {index} failed to load: {error}", case.id)
                    }
                    Some(_) | None => loaded = false,
                }
            }
            if loaded {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{}: deck load timed out",
                case.id
            );
            time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn tick_all(&self, case: SyncCase) {
        for (index, deck) in self.decks.iter().enumerate() {
            deck.tick()
                .unwrap_or_else(|error| panic!("{}: tick deck {index}: {error}", case.id));
        }
    }

    pub(super) async fn render(&mut self, case: SyncCase, frames: usize) -> Vec<f32> {
        let started = Instant::now();
        self.tick_all(case);
        let start = self.output_frames;
        let end = start
            .checked_add(u64::try_from(frames).expect("render frame count fits u64"))
            .expect("offline render timeline fits u64");
        let request = OfflineRenderRequest::builder()
            .spec(self.spec)
            .frames(start..end)
            .build();
        let cancel = CancelScope::new(None);
        let mut sink = VecRenderSink::default();
        let report = self
            .host
            .render(&request, &cancel.token(), &mut sink)
            .unwrap_or_else(|error| panic!("{}: render offline Host: {error}", case.id));
        assert_eq!(
            report.frames,
            u64::try_from(frames).expect("frame count fits u64")
        );
        self.tick_all(case);
        self.output_frames = end;
        let delay = if self.paced {
            Duration::from_secs_f64(frames as f64 / f64::from(case.sample_rate))
                .saturating_sub(started.elapsed())
        } else {
            Duration::from_millis(1)
        };
        time::sleep(delay).await;
        sink.samples
    }

    pub(super) async fn settle(&mut self, case: SyncCase, blocks: usize) {
        for _ in 0..blocks {
            let _ = self.render(case, self.block_frames).await;
        }
    }

    fn play_all(&self) {
        for deck in &self.decks {
            deck.play();
        }
    }

    async fn start_staggered(&mut self, case: SyncCase) {
        let stagger_frames =
            (f64::from(case.sample_rate) * 3.0 / 8.0 * 60.0 / case.start_bpm()).round() as usize;
        for index in 0..self.decks.len() {
            self.decks[index].play();
            if index + 1 < self.decks.len() {
                let _ = self.render(case, stagger_frames).await;
            }
        }
        self.settle(case, 2).await;
    }

    async fn seek_staggered(&mut self, case: SyncCase) {
        let stagger_seconds = 3.0 / 8.0 * 60.0 / case.start_bpm();
        for (index, deck) in self.decks.iter().enumerate() {
            deck.seek(5.25 + index as f64 * stagger_seconds)
                .unwrap_or_else(|error| panic!("{}: seek deck {index}: {error}", case.id));
        }
        self.settle(case, 96).await;
    }

    pub(super) fn set_tempo(&mut self, case: SyncCase, bpm: f64, required: bool) {
        let tempo = Tempo::new(bpm).expect("fixture tempo");
        match self.host.set_tempo(tempo) {
            Ok(()) => {}
            Err(error) if !required => self.record_tempo_failure(format!(
                "tempo request {bpm:.6} BPM could not reach Host: {error}"
            )),
            Err(error) => panic!("{}: set initial tempo: {error}", case.id),
        }
    }

    fn record_tempo_failure(&mut self, failure: String) {
        if !self
            .failures
            .iter()
            .any(|failure| failure.starts_with("tempo request"))
        {
            self.failures.push(failure);
        }
    }

    fn transport_revision(&self, case: SyncCase) -> kithara::warp::TransportRevision {
        self.host
            .transport_revision()
            .unwrap_or_else(|error| panic!("{}: query Host transport: {error}", case.id))
    }

    pub(super) async fn request_sync(&mut self, case: SyncCase) {
        self.request_sync_intent(case, SyncIntent::Enable).await;
    }

    pub(super) async fn request_sync_intent(&mut self, case: SyncCase, intent: SyncIntent) {
        let transport = self.transport_revision(case);
        for index in 0..self.decks.len() {
            {
                let deck = &self.decks[index];
                let playback = deck.playback_view();
                let position = playback.position.unwrap_or(0.0);
                let source = if playback.playing {
                    AlignmentSource::Audible(
                        PresentationFrontier::builder()
                            .source((position * f64::from(case.sample_rate)).max(0.0) as u64)
                            .output(SessionFrame::new(
                                i64::try_from(self.output_frames).unwrap_or(i64::MAX),
                            ))
                            .build(),
                    )
                } else {
                    AlignmentSource::Prepared
                };
                let admission = self
                    .host
                    .transact(SyncOperation::Sync {
                        target: deck.id(),
                        load: LoadGeneration::first(),
                        transport,
                        source,
                        activation: SessionFrame::new(
                            i64::try_from(self.output_frames).unwrap_or(i64::MAX),
                        ),
                        intent,
                    })
                    .unwrap_or_else(|rejected| {
                        panic!("{}: sync deck {index}: {rejected}", case.id)
                    });
                if let SyncAdmission::Unavailable { capability, .. } = admission {
                    self.failures.push(format!(
                        "sync deck {index} admitted unavailable capability {capability:?}"
                    ));
                }
            }
            if matches!(case.order, OperationOrder::SequentialSync) {
                let _ = self.render(case, self.block_frames).await;
            }
        }
    }

    pub(super) async fn run_operations(&mut self, case: SyncCase) {
        for operation in case.order.operations() {
            match operation {
                Operation::Play => {
                    self.play_all();
                    self.settle(case, 2).await;
                }
                Operation::Seek => self.seek_staggered(case).await,
                Operation::Sync => self.request_sync(case).await,
            }
        }
    }

    pub(super) async fn ride_tempo(&mut self, case: SyncCase) {
        let steps_per_leg = (case.updates_hz / 2).max(1);
        let mut start = case.start_bpm();
        let mut rendered = 0_u64;
        let mut update = 0_u64;
        for &target in case.ride.points() {
            for step in 1..=steps_per_leg {
                let fraction = f64::from(step) / f64::from(steps_per_leg);
                let bpm = start + (target - start) * fraction;
                self.set_tempo(case, bpm, false);
                update += 1;
                let deadline = update * u64::from(case.sample_rate) / u64::from(case.updates_hz);
                let frames = deadline.saturating_sub(rendered);
                rendered = deadline;
                if frames > 0 {
                    let frames = usize::try_from(frames).expect("tempo interval fits usize");
                    let _ = self.render(case, frames).await;
                }
            }
            start = target;
        }
        self.settle(case, 4).await;
    }

    async fn capture(&mut self, case: SyncCase) -> Vec<f32> {
        self.play_all();
        self.settle(case, 4).await;
        let capture_frames =
            (f64::from(case.sample_rate) * 60.0 / case.ride.final_bpm() * 6.0).round() as usize;
        self.capture_frames(case, capture_frames, self.block_frames)
            .await
    }

    pub(super) async fn capture_frames(
        &mut self,
        case: SyncCase,
        capture_frames: usize,
        block_frames: usize,
    ) -> Vec<f32> {
        self.capture_blocks(case, capture_frames, block_frames, false)
            .await
    }

    pub(super) async fn capture_paced(
        &mut self,
        case: SyncCase,
        capture_frames: usize,
    ) -> Vec<f32> {
        self.capture_blocks(case, capture_frames, self.block_frames, true)
            .await
    }

    async fn capture_blocks(
        &mut self,
        case: SyncCase,
        capture_frames: usize,
        block_frames: usize,
        paced: bool,
    ) -> Vec<f32> {
        let capture_kind = if paced { "paced" } else { "product" };
        let mut pcm = Vec::with_capacity(capture_frames * usize::from(CHANNELS));
        while pcm.len() < capture_frames * usize::from(CHANNELS) {
            let remaining_frames =
                (capture_frames * usize::from(CHANNELS) - pcm.len()) / usize::from(CHANNELS);
            let frames = remaining_frames.min(block_frames);
            let started = paced.then(Instant::now);
            let block = self.render(case, frames).await;
            assert!(
                !block.is_empty(),
                "{}: {capture_kind} capture stopped making PCM progress",
                case.id,
            );
            pcm.extend(block);
            if let Some(started) = started {
                let period = Duration::from_secs_f64(frames as f64 / f64::from(case.sample_rate));
                time::sleep(period.saturating_sub(started.elapsed())).await;
            }
        }
        pcm
    }
}

#[kithara::test(native, timeout(Duration::from_secs(10)))]
fn offline_renderer_publishes_only_complete_recordings() {
    let sample_rate = NonZeroU32::new(48_000).expect("test sample rate");
    let frames = 8;
    let store = memory_asset_store();
    let config = recording_config(sample_rate.get(), 1);
    let success_key = recording_key(&store, "success.wav");
    let cancelled_key = recording_key(&store, "cancelled.wav");
    let failed_key = recording_key(&store, "failed.wav");

    let (mut success_host, success_request) = offline_render(sample_rate, frames);
    let success_sink = AssetPartSink::acquire(&store, &success_key)
        .unwrap_or_else(|error| panic!("acquire success sink: {error}"));
    let mut success = RecordingCore::new(&config, success_sink, Some(frames))
        .unwrap_or_else(|error| panic!("open success recording: {error}"));
    let success_cancel = CancelScope::new(None);
    let report = success_host
        .render(&success_request, &success_cancel.token(), &mut success)
        .unwrap_or_else(|error| panic!("render success recording: {error}"));
    assert_eq!(report.frames, frames);
    let reader = success
        .finish()
        .unwrap_or_else(|error| panic!("finish success recording: {error}"));
    let expected_len = 44 + frames * u64::from(CHANNELS) * 4;
    assert_eq!(reader.len(), Some(expected_len));
    let mut header = [0_u8; 44];
    let read = reader
        .read_at(0, &mut header)
        .unwrap_or_else(|error| panic!("read success recording: {error}"));
    assert_eq!(read, header.len());
    assert_eq!(&header[0..4], b"RIFF");
    assert_eq!(&header[8..12], b"WAVE");
    assert_eq!(u16::from_le_bytes([header[20], header[21]]), 3);
    assert!(matches!(
        store.resource_state(&success_key),
        Ok(AssetResourceState::Committed {
            final_len: Some(len)
        }) if len == expected_len
    ));

    let (mut cancelled_host, cancelled_request) = offline_render(sample_rate, frames);
    let cancelled_sink = AssetPartSink::acquire(&store, &cancelled_key)
        .unwrap_or_else(|error| panic!("acquire cancelled sink: {error}"));
    let mut cancelled = RecordingCore::new(&config, cancelled_sink, Some(frames))
        .unwrap_or_else(|error| panic!("open cancelled recording: {error}"));
    let cancelled_scope = CancelScope::new(None);
    cancelled_scope.cancel();
    assert!(matches!(
        cancelled_host.render(&cancelled_request, &cancelled_scope.token(), &mut cancelled),
        Err(kithara::output::OfflineRenderError::Cancelled { rendered_frames: 0 })
    ));
    drop(cancelled);
    assert_eq!(
        store
            .resource_state(&cancelled_key)
            .unwrap_or_else(|error| panic!("cancelled resource state: {error}")),
        AssetResourceState::Missing
    );

    let (mut failed_host, failed_request) = offline_render(sample_rate, frames);
    let failed_sink = AssetPartSink::acquire(&store, &failed_key)
        .map(FailingPartSink)
        .unwrap_or_else(|error| panic!("acquire failing sink: {error}"));
    let mut failed = RecordingCore::new(&config, failed_sink, Some(frames))
        .unwrap_or_else(|error| panic!("open failing recording: {error}"));
    assert!(matches!(
        failed_host.render(
            &failed_request,
            &CancelScope::new(None).token(),
            &mut failed
        ),
        Err(kithara::output::OfflineRenderError::Sink {
            rendered_frames: 0,
            ..
        })
    ));
    assert_eq!(
        store
            .resource_state(&failed_key)
            .unwrap_or_else(|error| panic!("failed resource state: {error}")),
        AssetResourceState::Missing
    );
}

async fn sources(provider: Provider, decks: usize, server: &TestServerHelper) -> Vec<String> {
    match provider {
        Provider::Synthetic => cycle_paths(
            &[
                rhythm_wav_deck_a_120bpm_48k(),
                rhythm_wav_deck_b_120bpm_48k(),
                rhythm_wav_deck_c_120bpm_48k(),
                rhythm_wav_deck_d_120bpm_48k(),
            ],
            decks,
        ),
        Provider::Rhythm(assets) => assets
            .iter()
            .cycle()
            .take(decks)
            .map(|name| {
                asset_path(
                    by_name(name).unwrap_or_else(|| panic!("missing rhythm fixture `{name}`")),
                )
            })
            .collect(),
        Provider::Library => cycle_paths_from_strings(&library_paths(), decks),
        Provider::Mp3Same => cycle_paths(&[rhythm_mp3_deck_a_120bpm_48k()], decks),
        Provider::Sweep => cycle_paths(&[signal_mp3_sweep_up_60s()], decks),
        Provider::Mp3Distinct => cycle_paths(
            &[
                rhythm_mp3_deck_a_120bpm_48k(),
                rhythm_mp3_deck_b_120bpm_48k(),
            ],
            decks,
        ),
        Provider::HlsSame(protection) => {
            let url = hls(
                server,
                rhythm_fmp4_init_deck_a_120bpm_48k(),
                rhythm_fmp4_media_deck_a_120bpm_48k(),
                protection,
            )
            .await;
            vec![url; decks]
        }
        Provider::HlsMp3(protection) => {
            let hls = hls(
                server,
                rhythm_fmp4_init_deck_a_120bpm_48k(),
                rhythm_fmp4_media_deck_a_120bpm_48k(),
                protection,
            )
            .await;
            let mp3 = asset_path(rhythm_mp3_deck_b_120bpm_48k());
            (0..decks)
                .map(|index| {
                    if index.is_multiple_of(2) {
                        hls.clone()
                    } else {
                        mp3.clone()
                    }
                })
                .collect()
        }
    }
}

fn cycle_paths(assets: &[Asset], count: usize) -> Vec<String> {
    assets
        .iter()
        .cycle()
        .take(count)
        .map(|asset| {
            asset
                .path()
                .expect("native product fixture is materialized on disk")
                .to_str()
                .expect("fixture path is UTF-8")
                .to_owned()
        })
        .collect()
}

fn cycle_paths_from_strings(paths: &[String], count: usize) -> Vec<String> {
    paths.iter().cycle().take(count).cloned().collect()
}

fn library_paths() -> [String; 2] {
    let root = env::var_os("KITHARA_SYNC_LIBRARY").unwrap_or_else(|| {
        panic!("BLOCKED_FIXTURE: KITHARA_SYNC_LIBRARY must name the opt-in music library root")
    });
    let root = Path::new(&root);
    let track = |name: &str| {
        let relative = env::var_os(name).unwrap_or_else(|| {
            panic!("BLOCKED_FIXTURE: {name} must name a track under KITHARA_SYNC_LIBRARY")
        });
        let path = root.join(relative);
        assert!(
            path.is_file(),
            "BLOCKED_FIXTURE: {name} does not resolve to a file under KITHARA_SYNC_LIBRARY: {}",
            path.display(),
        );
        path.to_string_lossy().into_owned()
    };
    [
        track("KITHARA_SYNC_LIBRARY_TRACK_A"),
        track("KITHARA_SYNC_LIBRARY_TRACK_B"),
    ]
}

fn asset_path(asset: Asset) -> String {
    asset
        .path()
        .expect("native product fixture is materialized on disk")
        .to_str()
        .expect("fixture path is UTF-8")
        .to_owned()
}

async fn hls(
    server: &TestServerHelper,
    init: Asset,
    media: Asset,
    protection: HlsProtection,
) -> String {
    let mut builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(1)
        .segment_duration_secs(12.0)
        .segment_size(media.bytes().len())
        .codecs("fLaC".to_owned())
        .init_data_per_variant(vec![Arc::new(init.bytes().to_vec())])
        .custom_data(Arc::new(media.bytes().to_vec()));
    if matches!(protection, HlsProtection::Drm) {
        builder = builder.encryption(EncryptionRequest {
            key_hex: hex::encode(aes128_key_bytes()),
            iv_hex: Some(hex::encode(aes128_iv())),
        });
    }
    server
        .create_hls(builder)
        .await
        .expect("register build-time rhythmic fMP4 as HLS")
        .master_url()
        .to_string()
}

async fn run(case: SyncCase, provider: Provider) {
    let expected_samples = (f64::from(case.sample_rate) * 60.0 / case.ride.final_bpm() * 6.0)
        .round() as usize
        * usize::from(CHANNELS);
    let mut tracks = Vec::with_capacity(case.decks);
    let mut request_failures = Vec::new();
    for audible_deck in 0..case.decks {
        let mut harness = ProductHarness::new(case, provider, audible_deck).await;
        harness.run_operations(case).await;
        harness.ride_tempo(case).await;
        let pcm = harness.capture(case).await;
        assert_eq!(
            pcm.len(),
            expected_samples,
            "{} {provider:?}: deck {audible_deck} capture must contain six complete beats",
            case.id,
        );
        tracks.push(pcm);
        request_failures.extend(harness.failures);
    }
    let track_slices = tracks.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let label = format!("{} {provider:?}", case.id);
    let mut failures = if provider.has_score_markers() {
        marked_synchronization_failures(
            &label,
            &track_slices,
            CHANNELS,
            case.sample_rate,
            case.ride.final_bpm(),
        )
    } else {
        synchronization_failures(
            &label,
            &track_slices,
            CHANNELS,
            case.sample_rate,
            case.ride.final_bpm(),
        )
    };
    failures.extend(request_failures);
    assert!(
        failures.is_empty(),
        "ignored-red product synchronization assertion failed for {} {provider:?}:\n{}",
        case.id,
        failures.join("\n"),
    );
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(60))
)]
#[case::mp3(Provider::Mp3Same)]
#[case::drm(Provider::HlsSame(HlsProtection::Drm))]
async fn encoded_rhythmic_controls_reach_the_pcm_oracle(#[case] provider: Provider) {
    let mut harness = ProductHarness::new(ONE_DECK, provider, 0).await;
    let pcm = harness.capture(ONE_DECK).await;
    let mut failures = synchronization_failures(
        &format!("encoded rhythmic control {provider:?}"),
        &[pcm.as_slice()],
        CHANNELS,
        ONE_DECK.sample_rate,
        START_BPM,
    );
    failures.extend(harness.failures);
    assert!(
        failures.is_empty(),
        "encoded rhythmic control {provider:?} failed:\n{}",
        failures.join("\n"),
    );
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(600))
)]
#[ignore = "ignored-red: product Warp alignment is not implemented"]
#[case::play_sync_seek(PLAY_SYNC_SEEK, Provider::Synthetic)]
#[case::play_seek_sync(PLAY_SEEK_SYNC, Provider::Synthetic)]
#[case::seek_play_sync(SEEK_PLAY_SYNC, Provider::Synthetic)]
#[case::seek_sync_play(SEEK_SYNC_PLAY, Provider::Synthetic)]
#[case::sync_play_seek(SYNC_PLAY_SEEK, Provider::Synthetic)]
#[case::sync_seek_play(SYNC_SEEK_PLAY, Provider::Synthetic)]
#[case::sequential_sync(SEQUENTIAL_SYNC, Provider::Synthetic)]
#[case::paused_sync_then_play(PAUSED_SYNC, Provider::Synthetic)]
#[case::four_deck_sequential_sync(FOUR_DECK_SYNC, Provider::Synthetic)]
#[case::tempo_up_120hz(TEMPO_UP_120, Provider::Synthetic)]
#[case::tempo_down_30hz(TEMPO_DOWN_30, Provider::Synthetic)]
#[case::ambient_trip_hop(AMBIENT_TRIP_HOP_SYNC, AMBIENT_TRIP_HOP_PROVIDER)]
#[case::downtempo_house(DOWNTEMPO_HOUSE_SYNC, DOWNTEMPO_HOUSE_PROVIDER)]
#[case::techno_breakbeat(TECHNO_BREAKBEAT_SYNC, TECHNO_BREAKBEAT_PROVIDER)]
#[case::cross_style_four_deck(CROSS_STYLE_SYNC, CROSS_STYLE_PROVIDER)]
async fn wav_product_rows_reach_the_pcm_oracle(#[case] case: SyncCase, #[case] provider: Provider) {
    run(case, provider).await;
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(600))
)]
#[ignore = "ignored-red: product Warp alignment is not implemented"]
#[case::hls_same_play_sync_seek(Provider::HlsSame(HlsProtection::Plain), PLAY_SYNC_SEEK)]
#[case::hls_same_play_seek_sync(Provider::HlsSame(HlsProtection::Plain), PLAY_SEEK_SYNC)]
#[case::hls_same_seek_play_sync(Provider::HlsSame(HlsProtection::Plain), SEEK_PLAY_SYNC)]
#[case::hls_same_seek_sync_play(Provider::HlsSame(HlsProtection::Plain), SEEK_SYNC_PLAY)]
#[case::hls_same_sync_play_seek(Provider::HlsSame(HlsProtection::Plain), SYNC_PLAY_SEEK)]
#[case::hls_same_sync_seek_play(Provider::HlsSame(HlsProtection::Plain), SYNC_SEEK_PLAY)]
#[case::hls_same_sequential_sync(Provider::HlsSame(HlsProtection::Plain), SEQUENTIAL_SYNC)]
#[case::hls_same_paused_sync_then_play(Provider::HlsSame(HlsProtection::Plain), PAUSED_SYNC)]
#[case::hls_same_four_deck_sequential_sync(Provider::HlsSame(HlsProtection::Plain), FOUR_DECK_SYNC)]
#[case::hls_same_tempo_up_120hz(Provider::HlsSame(HlsProtection::Plain), TEMPO_UP_120)]
#[case::hls_same_tempo_down_30hz(Provider::HlsSame(HlsProtection::Plain), TEMPO_DOWN_30)]
#[case::drm_same_play_sync_seek(Provider::HlsSame(HlsProtection::Drm), PLAY_SYNC_SEEK)]
#[case::drm_same_play_seek_sync(Provider::HlsSame(HlsProtection::Drm), PLAY_SEEK_SYNC)]
#[case::drm_same_seek_play_sync(Provider::HlsSame(HlsProtection::Drm), SEEK_PLAY_SYNC)]
#[case::drm_same_seek_sync_play(Provider::HlsSame(HlsProtection::Drm), SEEK_SYNC_PLAY)]
#[case::drm_same_sync_play_seek(Provider::HlsSame(HlsProtection::Drm), SYNC_PLAY_SEEK)]
#[case::drm_same_sync_seek_play(Provider::HlsSame(HlsProtection::Drm), SYNC_SEEK_PLAY)]
#[case::drm_same_sequential_sync(Provider::HlsSame(HlsProtection::Drm), SEQUENTIAL_SYNC)]
#[case::drm_same_paused_sync_then_play(Provider::HlsSame(HlsProtection::Drm), PAUSED_SYNC)]
#[case::drm_same_four_deck_sequential_sync(Provider::HlsSame(HlsProtection::Drm), FOUR_DECK_SYNC)]
#[case::drm_same_tempo_up_120hz(Provider::HlsSame(HlsProtection::Drm), TEMPO_UP_120)]
#[case::drm_same_tempo_down_30hz(Provider::HlsSame(HlsProtection::Drm), TEMPO_DOWN_30)]
#[case::mp3_same_play_sync_seek(Provider::Mp3Same, PLAY_SYNC_SEEK)]
#[case::mp3_same_play_seek_sync(Provider::Mp3Same, PLAY_SEEK_SYNC)]
#[case::mp3_same_seek_play_sync(Provider::Mp3Same, SEEK_PLAY_SYNC)]
#[case::mp3_same_seek_sync_play(Provider::Mp3Same, SEEK_SYNC_PLAY)]
#[case::mp3_same_sync_play_seek(Provider::Mp3Same, SYNC_PLAY_SEEK)]
#[case::mp3_same_sync_seek_play(Provider::Mp3Same, SYNC_SEEK_PLAY)]
#[case::mp3_same_sequential_sync(Provider::Mp3Same, SEQUENTIAL_SYNC)]
#[case::mp3_same_paused_sync_then_play(Provider::Mp3Same, PAUSED_SYNC)]
#[case::mp3_same_four_deck_sequential_sync(Provider::Mp3Same, FOUR_DECK_SYNC)]
#[case::mp3_same_tempo_up_120hz(Provider::Mp3Same, TEMPO_UP_120)]
#[case::mp3_same_tempo_down_30hz(Provider::Mp3Same, TEMPO_DOWN_30)]
#[case::mp3_distinct_play_sync_seek(Provider::Mp3Distinct, PLAY_SYNC_SEEK)]
#[case::mp3_distinct_play_seek_sync(Provider::Mp3Distinct, PLAY_SEEK_SYNC)]
#[case::mp3_distinct_seek_play_sync(Provider::Mp3Distinct, SEEK_PLAY_SYNC)]
#[case::mp3_distinct_seek_sync_play(Provider::Mp3Distinct, SEEK_SYNC_PLAY)]
#[case::mp3_distinct_sync_play_seek(Provider::Mp3Distinct, SYNC_PLAY_SEEK)]
#[case::mp3_distinct_sync_seek_play(Provider::Mp3Distinct, SYNC_SEEK_PLAY)]
#[case::mp3_distinct_sequential_sync(Provider::Mp3Distinct, SEQUENTIAL_SYNC)]
#[case::mp3_distinct_paused_sync_then_play(Provider::Mp3Distinct, PAUSED_SYNC)]
#[case::mp3_distinct_four_deck_sequential_sync(Provider::Mp3Distinct, FOUR_DECK_SYNC)]
#[case::mp3_distinct_tempo_up_120hz(Provider::Mp3Distinct, TEMPO_UP_120)]
#[case::mp3_distinct_tempo_down_30hz(Provider::Mp3Distinct, TEMPO_DOWN_30)]
#[case::hls_mp3_play_sync_seek(Provider::HlsMp3(HlsProtection::Plain), PLAY_SYNC_SEEK)]
#[case::hls_mp3_play_seek_sync(Provider::HlsMp3(HlsProtection::Plain), PLAY_SEEK_SYNC)]
#[case::hls_mp3_seek_play_sync(Provider::HlsMp3(HlsProtection::Plain), SEEK_PLAY_SYNC)]
#[case::hls_mp3_seek_sync_play(Provider::HlsMp3(HlsProtection::Plain), SEEK_SYNC_PLAY)]
#[case::hls_mp3_sync_play_seek(Provider::HlsMp3(HlsProtection::Plain), SYNC_PLAY_SEEK)]
#[case::hls_mp3_sync_seek_play(Provider::HlsMp3(HlsProtection::Plain), SYNC_SEEK_PLAY)]
#[case::hls_mp3_sequential_sync(Provider::HlsMp3(HlsProtection::Plain), SEQUENTIAL_SYNC)]
#[case::hls_mp3_paused_sync_then_play(Provider::HlsMp3(HlsProtection::Plain), PAUSED_SYNC)]
#[case::hls_mp3_four_deck_sequential_sync(Provider::HlsMp3(HlsProtection::Plain), FOUR_DECK_SYNC)]
#[case::hls_mp3_tempo_up_120hz(Provider::HlsMp3(HlsProtection::Plain), TEMPO_UP_120)]
#[case::hls_mp3_tempo_down_30hz(Provider::HlsMp3(HlsProtection::Plain), TEMPO_DOWN_30)]
#[case::drm_mp3_play_sync_seek(Provider::HlsMp3(HlsProtection::Drm), PLAY_SYNC_SEEK)]
#[case::drm_mp3_play_seek_sync(Provider::HlsMp3(HlsProtection::Drm), PLAY_SEEK_SYNC)]
#[case::drm_mp3_seek_play_sync(Provider::HlsMp3(HlsProtection::Drm), SEEK_PLAY_SYNC)]
#[case::drm_mp3_seek_sync_play(Provider::HlsMp3(HlsProtection::Drm), SEEK_SYNC_PLAY)]
#[case::drm_mp3_sync_play_seek(Provider::HlsMp3(HlsProtection::Drm), SYNC_PLAY_SEEK)]
#[case::drm_mp3_sync_seek_play(Provider::HlsMp3(HlsProtection::Drm), SYNC_SEEK_PLAY)]
#[case::drm_mp3_sequential_sync(Provider::HlsMp3(HlsProtection::Drm), SEQUENTIAL_SYNC)]
#[case::drm_mp3_paused_sync_then_play(Provider::HlsMp3(HlsProtection::Drm), PAUSED_SYNC)]
#[case::drm_mp3_four_deck_sequential_sync(Provider::HlsMp3(HlsProtection::Drm), FOUR_DECK_SYNC)]
#[case::drm_mp3_tempo_up_120hz(Provider::HlsMp3(HlsProtection::Drm), TEMPO_UP_120)]
#[case::drm_mp3_tempo_down_30hz(Provider::HlsMp3(HlsProtection::Drm), TEMPO_DOWN_30)]
async fn real_media_product_rows_reach_the_pcm_oracle(
    #[case] provider: Provider,
    #[case] case: SyncCase,
) {
    run(case, provider).await;
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(600))
)]
#[ignore = "ignored-red: requires KITHARA_SYNC_LIBRARY and product Warp alignment"]
#[case::play_sync_seek(PLAY_SYNC_SEEK)]
#[case::play_seek_sync(PLAY_SEEK_SYNC)]
#[case::seek_play_sync(SEEK_PLAY_SYNC)]
#[case::seek_sync_play(SEEK_SYNC_PLAY)]
#[case::sync_play_seek(SYNC_PLAY_SEEK)]
#[case::sync_seek_play(SYNC_SEEK_PLAY)]
#[case::sequential_sync(SEQUENTIAL_SYNC)]
#[case::paused_sync_then_play(PAUSED_SYNC)]
#[case::four_deck_sequential_sync(FOUR_DECK_SYNC)]
#[case::tempo_up_120hz(TEMPO_UP_120)]
#[case::tempo_down_30hz(TEMPO_DOWN_30)]
async fn opt_in_library_product_rows_reach_the_pcm_oracle(#[case] case: SyncCase) {
    run(case, Provider::Library).await;
}
