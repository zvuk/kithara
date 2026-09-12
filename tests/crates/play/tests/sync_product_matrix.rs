#![cfg(not(target_arch = "wasm32"))]

use std::{
    env, io,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara::{
    analysis::{AnalysisFile, AnalysisFingerprint, BeatArtifact},
    assets::{AssetResource, AssetResourceState, AssetSource, AssetStore, ReadSide, ResourceKey},
    encode::EncodeConfig,
    events::TrackId,
    hls::AbrMode,
    host::{Host, HostConfig, HostOwned},
    output::{OfflineRenderRequest, OfflineRenderer},
    platform::{
        CancelScope,
        sync::Arc,
        time::{self, Duration, Instant},
    },
    play::{
        PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig,
        ResourceSrc, Tempo,
        player::{PlayerControl, PlayerControlSource},
    },
    queue::{CueIn, Queue, QueueConfig, TrackSource, TrackStatus, Transition},
    record::{RecordingConfig, RecordingCore, RecordingSink},
    signal::AudioSpec,
    warp::{
        AlignmentSource, AssetAxis, AssetFrame, Beat, BeatGridId, BeatGridQuery, BeatGridRevision,
        BeatGridSnapshot, BeatGridState, LoadGeneration, MapPoint, MapPosition, Meter,
        PresentationFrontier, SegmentSet, SessionFrame, StretchControls, SyncAdmission, SyncGroup,
        SyncIntent, SyncOperation, WarpConfig,
    },
};
use kithara_app::recording::AssetPartSink;
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    audio_artifact::{AudioArtifactTap, artifact_label},
    bufpool_ext::{TestPools, pools},
    cochlea::{
        CochleaReport, marked_rhythm_markers, marked_synchronization_failures,
        synchronization_failures,
    },
    fixture_protocol::EncryptionRequest,
    grid::segment_set,
    hls_fixture::{aes128_iv, aes128_key_bytes},
    kithara, memory_asset_store,
    offline::OfflineHostHarness,
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
use kithara_test_utils::probe::capture as probe_capture;
use num_traits::ToPrimitive;

pub(super) const BLOCK_FRAMES: usize = 128;
pub(super) const CHANNELS: u16 = 2;
const RENDER_QUANTUM_FRAMES: usize = 32;
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const START_BPM: f64 = 120.0;

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
    keylock: bool,
    start_seconds: Option<&'static [f64]>,
    ride: TempoRide,
    updates_hz: u32,
    pub(super) tracks_per_deck: usize,
    pub(super) crossfade_secs: f32,
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
            keylock: false,
            start_seconds: None,
            ride: TempoRide::Triangle,
            updates_hz: 60,
            tracks_per_deck: 1,
            crossfade_secs: 0.0,
        }
    }

    pub(super) const fn queued(
        id: &'static str,
        sample_rate: u32,
        tracks_per_deck: usize,
        crossfade_secs: f32,
    ) -> Self {
        let mut case = Self::running(id, 1, sample_rate, OperationOrder::PlaySyncSeek);
        case.tracks_per_deck = tracks_per_deck;
        case.crossfade_secs = crossfade_secs;
        case
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

    const fn keylocked(mut self) -> Self {
        self.keylock = true;
        self
    }

    const fn starts(mut self, seconds: &'static [f64]) -> Self {
        self.start_seconds = Some(seconds);
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

    pub(super) const fn keylock(self) -> bool {
        self.keylock
    }

    pub(super) const fn start_seconds(self) -> Option<&'static [f64]> {
        self.start_seconds
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
pub(super) const LIBRARY_SYNC: SyncCase = SyncCase::running(
    "library-song2-slowtechno",
    2,
    48_000,
    OperationOrder::SequentialSync,
)
.hold(120.0)
.keylocked()
.starts(&[60.0, 60.0]);
pub(super) const STRAIGHT_LIBRARY_SYNC: SyncCase = SyncCase::running(
    "library-c343-g242-keylock",
    2,
    48_000,
    OperationOrder::SequentialSync,
)
.hold(132.0)
.keylocked()
.starts(&[100.0, 50.0]);
pub(super) const STRAIGHT_LIBRARY_ALT_SYNC: SyncCase = SyncCase::running(
    "library-song1-track05-keylock",
    2,
    48_000,
    OperationOrder::SequentialSync,
)
.hold(132.0)
.keylocked()
.starts(&[98.0, 58.0]);
pub(super) const TECHNO_LIBRARY_SYNC: SyncCase = SyncCase::running(
    "library-newtechno-ryabina-keylock",
    2,
    48_000,
    OperationOrder::SequentialSync,
)
.hold(140.0)
.keylocked()
.starts(&[40.0, 82.0]);

const AMBIENT_TRIP_HOP: &[&str] = &[
    "rhythm_wav_ambient_dub_62_aligned",
    "rhythm_wav_trip_hop_74_aligned",
];
const DOWNTEMPO_HOUSE: &[&str] = &[
    "rhythm_wav_downtempo_96_aligned",
    "rhythm_wav_house_124_aligned",
];
const SCENARIO_1_DOWNTEMPO_HOUSE: &[&str] = &[
    "rhythm_wav_scenario_1_downtempo_96_left_only",
    "rhythm_wav_scenario_1_house_124_right_only",
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
pub(super) const LIBRARY: &[&str] = &["library_flac_song2", "library_flac_slowtechno"];
pub(super) const STRAIGHT_LIBRARY: &[&str] = &["library_flac_c343", "library_flac_g242"];
pub(super) const STRAIGHT_LIBRARY_ALT: &[&str] = &["library_flac_song1", "library_flac_track05"];
pub(super) const TECHNO_LIBRARY: &[&str] = &["library_flac_newtechno", "library_flac_ryabina"];

#[derive(Clone, Copy, Debug)]
pub(super) enum Provider {
    Synthetic,
    Rhythm(&'static [&'static str]),
    HlsSame(HlsProtection),
    Library(&'static [&'static str]),
    Mp3Same,
    Mp3Distinct,
    HlsMp3(HlsProtection),
    Sweep,
}

pub(super) const AMBIENT_TRIP_HOP_PROVIDER: Provider = Provider::Rhythm(AMBIENT_TRIP_HOP);
pub(super) const DOWNTEMPO_HOUSE_PROVIDER: Provider = Provider::Rhythm(DOWNTEMPO_HOUSE);
const SCENARIO_1_DOWNTEMPO_HOUSE_PROVIDER: Provider = Provider::Rhythm(SCENARIO_1_DOWNTEMPO_HOUSE);
pub(super) const TECHNO_BREAKBEAT_PROVIDER: Provider = Provider::Rhythm(TECHNO_BREAKBEAT);
pub(super) const CROSS_STYLE_PROVIDER: Provider = Provider::Rhythm(CROSS_STYLE);

impl Provider {
    pub(super) const ALL: &[Provider] = &[
        Self::Synthetic,
        Self::Rhythm(CROSS_STYLE),
        Self::HlsSame(HlsProtection::Plain),
        Self::HlsSame(HlsProtection::Drm),
        Self::Library(LIBRARY),
        Self::Mp3Same,
        Self::Mp3Distinct,
        Self::HlsMp3(HlsProtection::Plain),
        Self::HlsMp3(HlsProtection::Drm),
        Self::Sweep,
    ];

    const fn has_score_markers(self) -> bool {
        matches!(
            self,
            Self::Synthetic
                | Self::Rhythm(_)
                | Self::HlsSame(_)
                | Self::Mp3Same
                | Self::Mp3Distinct
                | Self::HlsMp3(_)
        )
    }

    const fn uniform_bpm(self) -> Option<f64> {
        match self {
            Self::Synthetic
            | Self::HlsSame(_)
            | Self::Mp3Same
            | Self::Mp3Distinct
            | Self::HlsMp3(_)
            | Self::Sweep => Some(START_BPM),
            Self::Rhythm(_) | Self::Library(_) => None,
        }
    }
}

fn uniform_grid(bpm: f64) -> SegmentSet {
    const SAMPLE_RATE: u32 = 48_000;
    const SECONDS: u64 = 60;
    let beat_frames = (f64::from(SAMPLE_RATE) * 60.0 / bpm).round() as u64;
    let beat_count = SAMPLE_RATE as u64 * SECONDS / beat_frames;
    let beats = (0..=beat_count)
        .map(|beat| (beat * beat_frames, Some(1.0)))
        .collect::<Vec<_>>();
    let downbeats = beats.iter().step_by(4).copied().collect();
    let artifact = BeatArtifact::new(bpm, beats, downbeats);
    segment_set(
        &artifact,
        AssetAxis::new(
            NonZeroU32::new(SAMPLE_RATE).expect("fixture sample rate"),
            SAMPLE_RATE as u64 * SECONDS,
        ),
    )
    .expect("uniform fixture grid")
}

#[derive(Clone, Copy, Debug)]
pub(super) enum HlsProtection {
    Plain,
    Drm,
}

pub(super) struct ProductHarness {
    pub(super) decks: Vec<HostOwned<Queue<TestPools>>>,
    pub(super) player_controls: Vec<PlayerControl<TestPools>>,
    pub(super) failures: Vec<String>,
    pub(super) ids: Vec<Vec<TrackId>>,
    pub(super) block_frames: usize,
    pub(super) host: OfflineHostHarness<TestPools>,
    pub(super) rendered_frames: u64,
    host_grid: Option<BeatGridSnapshot>,
    sync_requested: bool,
    sync_activation: Option<u64>,
    tap: Option<AudioArtifactTap>,
    server: Option<TestServerHelper>,
    paced: bool,
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

pub(super) type PreparedSources = (Provider, TestServerHelper, Vec<String>);

pub(super) async fn prepared_sources(provider: Provider) -> PreparedSources {
    let server = TestServerHelper::new().await;
    let paths = sources(provider, 4, &server).await;
    (provider, server, paths)
}

impl ProductHarness {
    pub(super) fn underrun_failures(&self) -> Vec<String> {
        self.player_controls
            .iter()
            .enumerate()
            .filter_map(|(deck, control)| {
                let count = control.rt_metrics()?.underruns();
                (count > 0).then(|| format!("deck {deck} rendered with {count} PCM underruns"))
            })
            .collect()
    }

    pub(super) async fn new(
        case: SyncCase,
        prepared: &PreparedSources,
        audible_deck: usize,
    ) -> Self {
        Self::build(
            case,
            prepared,
            audible_deck,
            BLOCK_FRAMES,
            false,
            CueIn::FirstDownbeat,
        )
        .await
    }

    pub(super) async fn new_for_provider(
        case: SyncCase,
        provider: Provider,
        audible_deck: usize,
    ) -> Self {
        let prepared = prepared_sources(provider).await;
        let mut harness = Self::build(
            case,
            &prepared,
            audible_deck,
            BLOCK_FRAMES,
            true,
            CueIn::FirstDownbeat,
        )
        .await;
        let (_, server, _) = prepared;
        harness.server = Some(server);
        harness
    }

    pub(super) async fn new_for_block(
        case: SyncCase,
        prepared: &PreparedSources,
        audible_deck: usize,
        block_frames: usize,
    ) -> Self {
        Self::build(
            case,
            prepared,
            audible_deck,
            block_frames,
            true,
            CueIn::FirstDownbeat,
        )
        .await
    }

    pub(super) async fn new_track_start(
        case: SyncCase,
        prepared: &PreparedSources,
        audible_deck: usize,
    ) -> Self {
        Self::build(
            case,
            prepared,
            audible_deck,
            BLOCK_FRAMES,
            true,
            CueIn::TrackStart,
        )
        .await
    }

    async fn build(
        case: SyncCase,
        prepared: &PreparedSources,
        audible_deck: usize,
        block_frames: usize,
        paced: bool,
        cue_in: CueIn,
    ) -> Self {
        let provider = prepared.0;
        let sources: Vec<_> = prepared
            .2
            .iter()
            .cycle()
            .take(case.decks * case.tracks_per_deck)
            .cloned()
            .collect();
        let pools = pools();
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
        let sample_rate = NonZeroU32::new(case.sample_rate).expect("fixture sample rate");
        let max_block_frames =
            NonZeroU32::new(u32::try_from(block_frames).expect("fixture block size fits u32"))
                .expect("fixture block size is non-zero");
        let response_budget_frames = block_frames
            .div_ceil(RENDER_QUANTUM_FRAMES)
            .checked_add(2)
            .and_then(|chunks| chunks.checked_mul(RENDER_QUANTUM_FRAMES))
            .and_then(|frames| frames.checked_sub(1))
            .and_then(NonZeroUsize::new)
            .expect("fixture response geometry fits usize");
        let render_quantum_frames =
            NonZeroUsize::new(RENDER_QUANTUM_FRAMES).expect("fixture quantum is non-zero");
        let session = HostConfig::offline(pools)
            .sample_rate(sample_rate)
            .max_block_frames(max_block_frames)
            .build();
        let host = OfflineHostHarness::new(session)
            .await
            .unwrap_or_else(|error| panic!("{}: create offline Host: {error}", case.id));
        let mut decks = Vec::with_capacity(case.decks);
        let mut player_controls = Vec::with_capacity(case.decks);
        let mut ids = Vec::with_capacity(case.decks);
        for (index, deck_sources) in sources.chunks(case.tracks_per_deck).enumerate() {
            let stretch = StretchControls::new(1.0);
            stretch.set_keylock(case.keylock);
            let player = PlayerImpl::new(
                PlayerConfig::builder()
                    .worker(worker.clone())
                    .sample_rate(sample_rate)
                    .response_budget_frames(response_budget_frames)
                    .crossfade_duration(case.crossfade_secs)
                    .warp(
                        WarpConfig::builder()
                            .stretch(stretch)
                            .render_quantum_frames(render_quantum_frames)
                            .build(),
                    )
                    .build(),
            );
            player_controls.push(player.control());
            let queue = Queue::new(
                QueueConfig::builder()
                    .player(player)
                    .cue_in(cue_in)
                    .should_autoplay(false)
                    .build(),
            );
            queue.set_muted(index != audible_deck);
            let deck = host
                .insert(queue)
                .await
                .unwrap_or_else(|error| panic!("{}: insert deck {index}: {error}", case.id));
            let mut deck_ids = Vec::with_capacity(deck_sources.len());
            for (track, source) in deck_sources.iter().enumerate() {
                let config =
                    ResourceConfig::for_src(ResourceSrc::parse(source).unwrap_or_else(|error| {
                        panic!("{}: parse source {source}: {error}", case.id)
                    }))
                    .store(memory_asset_store())
                    .initial_abr_mode(AbrMode::manual(0))
                    .discriminator(format!("{}-{provider:?}-{index}-{track}", case.id))
                    .build();
                let control = deck.control().clone();
                deck_ids.push(
                    host.run(move || control.append(TrackSource::Config(Box::new(config))))
                        .await
                        .unwrap_or_else(|error| {
                            panic!("{}: append deck {index} track {track}: {error}", case.id)
                        }),
                );
            }
            decks.push(deck);
            ids.push(deck_ids);
        }
        let mut harness = Self {
            decks,
            player_controls,
            failures: Vec::new(),
            ids,
            block_frames,
            host,
            rendered_frames: 0,
            host_grid: None,
            sync_requested: false,
            sync_activation: None,
            tap: AudioArtifactTap::from_env(
                &format!("{}-{}", artifact_label(), case.id),
                case.sample_rate,
                CHANNELS,
            )
            .expect("listening tap"),
            server: None,
            paced,
        };
        harness.wait_loaded(case).await;
        for (index, deck) in harness.decks.iter().enumerate() {
            let id = harness.ids[index][0];
            let control = deck.control().clone();
            harness
                .host
                .run(move || {
                    let selected = control.select(id, Transition::None);
                    control.pause();
                    selected
                })
                .await
                .unwrap_or_else(|error| panic!("{}: select deck {index}: {error}", case.id));
        }
        if let Some(bpm) = provider.uniform_bpm() {
            let grid = uniform_grid(bpm);
            for deck in 0..harness.ids.len() {
                let items = harness.ids[deck].clone();
                for item in items {
                    let _ = harness
                        .publish_track_grid(deck, item, grid.clone(), BeatGridState::Complete)
                        .await
                        .unwrap_or_else(|error| {
                            panic!("{}: publish deck {deck} fixture grid: {error}", case.id)
                        });
                }
            }
        }
        harness.set_tempo(case, case.start_bpm(), true).await;
        let _ = harness.render(case, harness.block_frames).await;
        if !case.paused {
            harness.start_staggered(case).await;
        }
        harness
    }

    async fn wait_loaded(&mut self, case: SyncCase) {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        loop {
            self.tick_all(case).await;
            let mut loaded = true;
            for (index, deck) in self.decks.iter().enumerate() {
                for id in &self.ids[index] {
                    match deck.track(*id).map(|track| track.status) {
                        Some(TrackStatus::Loaded) => {}
                        Some(TrackStatus::Failed(error)) => {
                            panic!("{}: deck {index} failed to load: {error}", case.id)
                        }
                        Some(_) | None => loaded = false,
                    }
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

    /// Ticks every deck from the host owner thread, as the app update loop
    /// would.
    async fn tick_all(&self, case: SyncCase) {
        let controls: Vec<_> = self
            .decks
            .iter()
            .map(|deck| deck.control().clone())
            .collect();
        self.host
            .run(move || {
                for (index, control) in controls.iter().enumerate() {
                    control
                        .tick()
                        .unwrap_or_else(|error| panic!("{}: tick deck {index}: {error}", case.id));
                }
            })
            .await;
    }

    pub(super) async fn render(&mut self, case: SyncCase, frames: usize) -> Vec<f32> {
        let started = Instant::now();
        self.tick_all(case).await;
        let start = self.rendered_frames;
        let end = start
            .checked_add(u64::try_from(frames).expect("render frame count fits u64"))
            .expect("offline render timeline fits u64");
        let samples = self.host.render(frames).await;
        assert_eq!(self.host.position(), end);
        self.tick_all(case).await;
        self.rendered_frames = end;
        self.record_host_grid(start, end);
        if let Some(tap) = self.tap.as_mut() {
            tap.timeline().span(
                "host-output",
                start,
                end,
                "presented",
                "offline Host render",
                None,
            );
            tap.push(&samples);
        }
        let delay = if self.paced {
            Duration::from_secs_f64(frames as f64 / f64::from(case.sample_rate))
                .saturating_sub(started.elapsed())
        } else {
            Duration::from_millis(1)
        };
        time::sleep(delay).await;
        samples
    }

    fn record_host_grid(&mut self, start: u64, end: u64) {
        let Some(grid) = self.host_grid.as_ref() else {
            return;
        };
        let at = |frame| {
            grid.beat_at(MapPoint::new(
                grid.stamp(),
                MapPosition::Session(SessionFrame::new(frame)),
            ))
        };
        let (BeatGridQuery::Resolved(first), BeatGridQuery::Resolved(last)) = (
            at(i64::try_from(start).unwrap_or(i64::MAX)),
            at(i64::try_from(end).unwrap_or(i64::MAX)),
        ) else {
            return;
        };
        let Some(first) = f64::from(*first.value().value()).ceil().to_i64() else {
            return;
        };
        let Some(last) = f64::from(*last.value().value()).floor().to_i64() else {
            return;
        };
        for ordinal in first..=last {
            let Ok(beat) = Beat::try_from(kithara::warp::BeatOrdinal::new(ordinal)) else {
                continue;
            };
            let BeatGridQuery::Resolved(position) =
                grid.position_at(MapPoint::new(grid.stamp(), beat))
            else {
                continue;
            };
            let MapPosition::Session(frame) = *position.value().value() else {
                continue;
            };
            let Ok(frame) = u64::try_from(i64::from(frame)) else {
                continue;
            };
            let kind = match grid.meter_at(MapPoint::new(grid.stamp(), beat)) {
                BeatGridQuery::Resolved(meter)
                    if (ordinal - i64::from(meter.value().downbeat()))
                        .rem_euclid(i64::from(meter.value().beats_per_bar()))
                        == 0 =>
                {
                    "downbeat"
                }
                _ => "beat",
            };
            if let Some(tap) = self.tap.as_mut() {
                tap.timeline().point(
                    "host-grid",
                    frame,
                    "grid",
                    &format!("Host {kind} {ordinal}"),
                );
            }
        }
    }

    pub(super) async fn settle(&mut self, case: SyncCase, blocks: usize) {
        for _ in 0..blocks {
            let _ = self.render(case, self.block_frames).await;
        }
    }

    pub(super) async fn settle_sync_activation(&mut self, case: SyncCase) {
        let activation = self.sync_activation.unwrap_or_else(|| {
            panic!(
                "fixture SYNC must prepare an activation: {}",
                self.failures.join("; ")
            )
        });
        while self.rendered_frames <= activation {
            let _ = self.render(case, self.block_frames).await;
        }
    }

    async fn play_all(&self) {
        let controls: Vec<_> = self
            .decks
            .iter()
            .map(|deck| deck.control().clone())
            .collect();
        self.host
            .run(move || {
                for control in &controls {
                    control.play();
                }
            })
            .await;
    }

    async fn start_staggered(&mut self, case: SyncCase) {
        let stagger_frames =
            (f64::from(case.sample_rate) * 3.0 / 8.0 * 60.0 / case.start_bpm()).round() as usize;
        for index in 0..self.decks.len() {
            let control = self.decks[index].control().clone();
            self.host.run(move || control.play()).await;
            if index + 1 < self.decks.len() {
                let mut remaining = stagger_frames;
                while remaining > 0 {
                    let frames = remaining.min(self.block_frames);
                    let _ = self.render(case, frames).await;
                    remaining -= frames;
                }
            }
        }
        self.settle(case, 2).await;
    }

    pub(super) async fn seek_staggered(&mut self, case: SyncCase) {
        self.mark("seek");
        let stagger_seconds = 3.0 / 8.0 * 60.0 / case.start_bpm();
        for index in 0..self.decks.len() {
            let deck = &self.decks[index];
            let seconds = case.start_seconds.map_or_else(
                || 5.25 + index as f64 * stagger_seconds,
                |starts| {
                    *starts
                        .get(index)
                        .unwrap_or_else(|| panic!("{}: missing deck {index} start", case.id))
                },
            );
            if let Some(tap) = self.tap.as_mut() {
                let source = (seconds * f64::from(case.sample_rate)).round() as u64;
                tap.timeline().span(
                    &format!("deck-{index}"),
                    self.rendered_frames,
                    self.rendered_frames,
                    "command",
                    &format!("seek request {seconds:.6}s"),
                    Some((source, source)),
                );
            }
            if self.sync_requested {
                let target = deck.id();
                let source_frame = (seconds * f64::from(case.sample_rate)).round() as u64;
                let _ = deck;
                let transport = self.transport_revision(case).await;
                let admission = self
                    .host
                    .with(move |host| {
                        host.transact(SyncOperation::Transport {
                            target,
                            load: LoadGeneration::first(),
                            transport,
                            operation: kithara::warp::TransportOperation::Seek { source_frame },
                        })
                    })
                    .await
                    .unwrap_or_else(|rejected| {
                        panic!("{}: synchronized seek deck {index}: {rejected}", case.id)
                    });
                if let SyncAdmission::Prepared { activation, .. } = admission {
                    let activation = u64::try_from(i64::from(activation))
                        .expect("fixture activation must be non-negative");
                    self.sync_activation = Some(
                        self.sync_activation
                            .map_or(activation, |current| current.max(activation)),
                    );
                }
            } else {
                deck.seek(seconds)
                    .unwrap_or_else(|error| panic!("{}: seek deck {index}: {error}", case.id));
            }
        }
        if self.sync_requested {
            self.settle_sync_activation(case).await;
        }
        self.settle(case, 96).await;
    }

    pub(super) async fn set_tempo(&mut self, case: SyncCase, bpm: f64, required: bool) {
        self.mark(&format!("set_tempo-{bpm}"));
        let tempo = Tempo::new(bpm).expect("fixture tempo");
        match self.host.with(move |host| host.set_tempo(tempo)).await {
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

    async fn transport_revision(&mut self, case: SyncCase) -> kithara::warp::TransportRevision {
        let (revision, grid) = self
            .host
            .transport_revision_and_grid()
            .await
            .unwrap_or_else(|error| panic!("{}: query Host transport: {error}", case.id));
        self.host_grid = Some(grid);
        revision
    }

    pub(super) async fn request_sync(&mut self, case: SyncCase) {
        self.request_sync_intent(case, SyncIntent::Enable).await;
        self.sync_requested = true;
    }

    pub(super) async fn request_sync_intent(&mut self, case: SyncCase, intent: SyncIntent) {
        self.mark(&format!("request_sync-{intent:?}"));
        let transport = self.transport_revision(case).await;
        for index in 0..self.decks.len() {
            {
                let deck = &self.decks[index];
                let playback = deck.playback_view();
                let position = playback.position.unwrap_or(0.0);
                let presentation_source = (position * f64::from(case.sample_rate)).max(0.0) as u64;
                let preparation_source = self.player_controls[index]
                    .playback_snapshot()
                    .map(|snapshot| {
                        snapshot.preparation_source(
                            presentation_source,
                            NonZeroUsize::new(448).expect("fixture response budget is non-zero"),
                        )
                    })
                    .unwrap_or(presentation_source);
                let source = if playback.playing {
                    AlignmentSource::Audible {
                        presentation: PresentationFrontier::builder()
                            .source(presentation_source)
                            .output(SessionFrame::new(
                                i64::try_from(self.rendered_frames).unwrap_or(i64::MAX),
                            ))
                            .build(),
                        preparation_source,
                        playback_rate: kithara::warp::RateTarget::default().with_speed(
                            self.player_controls[index]
                                .playback_snapshot()
                                .map_or(1.0, |snapshot| snapshot.rate()),
                        ),
                    }
                } else {
                    AlignmentSource::Prepared(
                        PresentationFrontier::builder()
                            .source(presentation_source)
                            .output(SessionFrame::new(
                                i64::try_from(self.rendered_frames).unwrap_or(i64::MAX),
                            ))
                            .build(),
                    )
                };
                let target = deck.id();
                let activation =
                    SessionFrame::new(i64::try_from(self.rendered_frames).unwrap_or(i64::MAX));
                let admission = self
                    .host
                    .with(move |host| {
                        host.transact(SyncOperation::Sync {
                            target,
                            load: LoadGeneration::first(),
                            transport,
                            source,
                            activation,
                            intent,
                        })
                    })
                    .await
                    .unwrap_or_else(|rejected| {
                        panic!("{}: sync deck {index}: {rejected}", case.id)
                    });
                if let SyncAdmission::Unavailable { capability, .. } = admission {
                    self.failures.push(format!(
                        "sync deck {index} admitted unavailable capability {capability:?}"
                    ));
                }
                if let SyncAdmission::Prepared { activation, .. } = admission {
                    let activation = u64::try_from(i64::from(activation))
                        .expect("fixture activation must be non-negative");
                    if let Some(tap) = self.tap.as_mut() {
                        tap.timeline().point(
                            &format!("deck-{index}"),
                            activation,
                            "planned",
                            "sync activation",
                        );
                    }
                    self.sync_activation = Some(
                        self.sync_activation
                            .map_or(activation, |current| current.max(activation)),
                    );
                }
            }
            if matches!(case.order, OperationOrder::SequentialSync) {
                let _ = self.render(case, self.block_frames).await;
            }
        }
    }

    pub(super) async fn publish_track_grid(
        &mut self,
        deck: usize,
        item: TrackId,
        segments: SegmentSet,
        state: BeatGridState,
    ) -> Result<SyncAdmission, PlayError> {
        if let Some(tap) = self.tap.as_mut() {
            let diagnostic = BeatGridSnapshot::segments(
                BeatGridId::allocate().expect("diagnostic grid identity"),
                BeatGridRevision::first(),
                state,
                segments.clone(),
            )
            .expect("published track grid is valid for artifact diagnostics");
            tap.source_grid(item.as_u64(), diagnostic);
        }
        let target = self.decks[deck].id();
        self.host
            .with(move |host| host.publish_track_grid(target, item, segments, state))
            .await
    }

    pub(super) fn mark(&mut self, label: &str) {
        if let Some(tap) = self.tap.as_mut() {
            tap.mark(label);
        }
    }

    pub(super) async fn run_operations(&mut self, case: SyncCase) {
        for operation in case.order.operations() {
            match operation {
                Operation::Play => {
                    self.play_all().await;
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
                self.set_tempo(case, bpm, false).await;
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
        self.play_all().await;
        if self.sync_activation.is_some() {
            self.settle_sync_activation(case).await;
        }
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

pub(super) async fn sources(
    provider: Provider,
    decks: usize,
    server: &TestServerHelper,
) -> Vec<String> {
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
        Provider::Library(names) => names
            .iter()
            .cycle()
            .take(decks)
            .map(|name| {
                let asset = by_name(name).unwrap_or_else(|| {
                    panic!(
                        "BLOCKED_FIXTURE: library fixture `{name}` is not registered; build with KITHARA_REMOTE_FIXTURES=1"
                    )
                });
                asset
                    .try_bytes()
                    .unwrap_or_else(|error| panic!("BLOCKED_FIXTURE: {error}"));
                asset_path(asset)
            })
            .collect(),
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

fn asset_path(asset: Asset) -> String {
    asset
        .path()
        .expect("native product fixture is materialized on disk")
        .to_str()
        .expect("fixture path is UTF-8")
        .to_owned()
}

fn analysis_name(source: &str) -> Option<String> {
    source
        .strip_prefix("rhythm_wav_")
        .map(|case| format!("rhythm_expected_analysis_{case}"))
        .or_else(|| {
            source
                .strip_prefix("library_flac_")
                .map(|case| format!("library_analysis_{case}"))
        })
}

fn fixture_grid(source: &str) -> SegmentSet {
    const FINGERPRINT: &str = "rhythm-fixture:v1";

    let analysis_name = analysis_name(source)
        .unwrap_or_else(|| panic!("`{source}` has no analysis-sidecar naming contract"));
    let asset = by_name(&analysis_name)
        .unwrap_or_else(|| panic!("missing analysis fixture `{analysis_name}`"));
    let file = AnalysisFile::parse(
        asset.bytes(),
        &AnalysisFingerprint::new(Some(FINGERPRINT), None),
    )
    .unwrap_or_else(|error| panic!("decode `{analysis_name}`: {error}"));
    let analysis = file.latest().analysis();
    let beat = analysis
        .beat()
        .unwrap_or_else(|| panic!("`{analysis_name}` has no beat analysis"));
    let artifact: &BeatArtifact = beat.artifact();
    let extent = analysis
        .extent()
        .unwrap_or_else(|| panic!("`{analysis_name}` has no source extent"));
    let axis = AssetAxis::new(analysis.source_sample_rate(), extent);
    segment_set(artifact, axis)
        .unwrap_or_else(|error| panic!("`{analysis_name}` has no usable beat grid: {error}"))
}

pub(super) async fn prepare_fixture_grids(
    harness: &mut ProductHarness,
    case: SyncCase,
    provider: &PreparedSources,
) {
    let sources = match provider.0 {
        Provider::Rhythm(sources) | Provider::Library(sources) => sources,
        _ => return,
    };
    for deck in 0..harness.decks.len() {
        let source = sources[deck % sources.len()];
        let grid = fixture_grid(source);
        let _ = harness
            .publish_track_grid(deck, harness.ids[deck][0], grid, BeatGridState::Complete)
            .await
            .unwrap_or_else(|error| {
                panic!("{}: publish deck {deck} fixture grid: {error}", case.id())
            });
    }
    if matches!(provider.0, Provider::Library(_)) {
        harness.seek_staggered(case).await;
    }
}

#[derive(Clone, Copy)]
struct SingleDeckTempoControl {
    id: &'static str,
    provider: Provider,
    source: &'static str,
    source_bpm: u32,
    requested_source_seconds: f64,
    requested_source_frame: u64,
    selected_source_beat: i64,
    selected_source_frame: u64,
    next_source_frame: u64,
    rate: &'static str,
}

const ORIGIN_ZERO_HOUSE_124: &[&str] =
    &["rhythm_wav_scenario_1_origin_zero_long_house_124_left_only"];
const ORIGIN_ZERO_DOWNTEMPO_96: &[&str] =
    &["rhythm_wav_scenario_1_origin_zero_long_downtempo_96_left_only"];
const PICKUP_HOUSE_124: &[&str] =
    &["rhythm_wav_scenario_1_origin_zero_pickup_long_house_124_left_only"];
const PICKUP_DOWNTEMPO_96: &[&str] =
    &["rhythm_wav_scenario_1_origin_zero_pickup_long_downtempo_96_left_only"];
const LISTENING_ALIGNED_DOWNTEMPO_96: &[&str] =
    &["rhythm_wav_scenario_1_origin_zero_listening_long_downtempo_96_stereo_55s"];
const LISTENING_PICKUP_DOWNTEMPO_96: &[&str] =
    &["rhythm_wav_scenario_1_origin_zero_pickup_listening_downtempo_96_stereo_45s"];

#[ignore = "ignored-red: origin-zero single-deck prepared launch baseline, 2026-09-12"]
#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
#[case::equal_124(SingleDeckTempoControl {
    id: "origin-zero-equal-124",
    provider: Provider::Rhythm(ORIGIN_ZERO_HOUSE_124),
    source: "rhythm_wav_scenario_1_origin_zero_long_house_124_left_only",
    source_bpm: 124,
    requested_source_seconds: 0.0,
    requested_source_frame: 0,
    selected_source_beat: 0,
    selected_source_frame: 0,
    next_source_frame: 23_226,
    rate: "1",
})]
#[case::equal_124_start_10(SingleDeckTempoControl {
    id: "origin-zero-equal-124-start-10",
    provider: Provider::Rhythm(ORIGIN_ZERO_HOUSE_124),
    source: "rhythm_wav_scenario_1_origin_zero_long_house_124_left_only",
    source_bpm: 124,
    requested_source_seconds: 10.0,
    requested_source_frame: 480_000,
    selected_source_beat: 24,
    selected_source_frame: 557_424,
    next_source_frame: 580_650,
    rate: "1",
})]
#[case::equal_124_start_10_1(SingleDeckTempoControl {
    id: "origin-zero-equal-124-start-10-1",
    provider: Provider::Rhythm(ORIGIN_ZERO_HOUSE_124),
    source: "rhythm_wav_scenario_1_origin_zero_long_house_124_left_only",
    source_bpm: 124,
    requested_source_seconds: 10.1,
    requested_source_frame: 484_800,
    selected_source_beat: 24,
    selected_source_frame: 557_424,
    next_source_frame: 580_650,
    rate: "1",
})]
#[case::different_96(SingleDeckTempoControl {
    id: "origin-zero-different-96",
    provider: Provider::Rhythm(ORIGIN_ZERO_DOWNTEMPO_96),
    source: "rhythm_wav_scenario_1_origin_zero_long_downtempo_96_left_only",
    source_bpm: 96,
    requested_source_seconds: 0.0,
    requested_source_frame: 0,
    selected_source_beat: 0,
    selected_source_frame: 0,
    next_source_frame: 30_000,
    rate: "31/24",
})]
#[case::different_96_start_10(SingleDeckTempoControl {
    id: "origin-zero-different-96-start-10",
    provider: Provider::Rhythm(ORIGIN_ZERO_DOWNTEMPO_96),
    source: "rhythm_wav_scenario_1_origin_zero_long_downtempo_96_left_only",
    source_bpm: 96,
    requested_source_seconds: 10.0,
    requested_source_frame: 480_000,
    selected_source_beat: 16,
    selected_source_frame: 480_000,
    next_source_frame: 510_000,
    rate: "31/24",
})]
#[case::different_96_start_10_1(SingleDeckTempoControl {
    id: "origin-zero-different-96-start-10-1",
    provider: Provider::Rhythm(ORIGIN_ZERO_DOWNTEMPO_96),
    source: "rhythm_wav_scenario_1_origin_zero_long_downtempo_96_left_only",
    source_bpm: 96,
    requested_source_seconds: 10.1,
    requested_source_frame: 484_800,
    selected_source_beat: 20,
    selected_source_frame: 600_000,
    next_source_frame: 630_000,
    rate: "31/24",
})]
#[case::pickup_equal_124(SingleDeckTempoControl {
    id: "pickup-first-downbeat-124",
    provider: Provider::Rhythm(PICKUP_HOUSE_124),
    source: "rhythm_wav_scenario_1_origin_zero_pickup_long_house_124_left_only",
    source_bpm: 124,
    requested_source_seconds: 0.0,
    requested_source_frame: 0,
    selected_source_beat: 1,
    selected_source_frame: 23_226,
    next_source_frame: 46_452,
    rate: "1",
})]
#[case::pickup_equal_124_start_10(SingleDeckTempoControl {
    id: "pickup-first-downbeat-124-start-10",
    provider: Provider::Rhythm(PICKUP_HOUSE_124),
    source: "rhythm_wav_scenario_1_origin_zero_pickup_long_house_124_left_only",
    source_bpm: 124,
    requested_source_seconds: 10.0,
    requested_source_frame: 480_000,
    selected_source_beat: 21,
    selected_source_frame: 487_746,
    next_source_frame: 510_972,
    rate: "1",
})]
#[case::pickup_equal_124_start_10_1(SingleDeckTempoControl {
    id: "pickup-first-downbeat-124-start-10-1",
    provider: Provider::Rhythm(PICKUP_HOUSE_124),
    source: "rhythm_wav_scenario_1_origin_zero_pickup_long_house_124_left_only",
    source_bpm: 124,
    requested_source_seconds: 10.1,
    requested_source_frame: 484_800,
    selected_source_beat: 21,
    selected_source_frame: 487_746,
    next_source_frame: 510_972,
    rate: "1",
})]
#[case::pickup_different_96(SingleDeckTempoControl {
    id: "pickup-first-downbeat-96",
    provider: Provider::Rhythm(PICKUP_DOWNTEMPO_96),
    source: "rhythm_wav_scenario_1_origin_zero_pickup_long_downtempo_96_left_only",
    source_bpm: 96,
    requested_source_seconds: 0.0,
    requested_source_frame: 0,
    selected_source_beat: 1,
    selected_source_frame: 30_000,
    next_source_frame: 60_000,
    rate: "31/24",
})]
#[case::pickup_different_96_start_10(SingleDeckTempoControl {
    id: "pickup-first-downbeat-96-start-10",
    provider: Provider::Rhythm(PICKUP_DOWNTEMPO_96),
    source: "rhythm_wav_scenario_1_origin_zero_pickup_long_downtempo_96_left_only",
    source_bpm: 96,
    requested_source_seconds: 10.0,
    requested_source_frame: 480_000,
    selected_source_beat: 17,
    selected_source_frame: 510_000,
    next_source_frame: 540_000,
    rate: "31/24",
})]
#[case::pickup_different_96_start_10_1(SingleDeckTempoControl {
    id: "pickup-first-downbeat-96-start-10-1",
    provider: Provider::Rhythm(PICKUP_DOWNTEMPO_96),
    source: "rhythm_wav_scenario_1_origin_zero_pickup_long_downtempo_96_left_only",
    source_bpm: 96,
    requested_source_seconds: 10.1,
    requested_source_frame: 484_800,
    selected_source_beat: 17,
    selected_source_frame: 510_000,
    next_source_frame: 540_000,
    rate: "31/24",
})]
async fn single_deck_origin_zero_tempo_controls_reach_real_pcm(
    #[case] control: SingleDeckTempoControl,
) {
    const REQUEST_OUTPUT_FRONTIER: u64 = 1_152;
    const ASSIGNED_OUTPUT_FRAME: u64 = 92_903;
    const NEXT_OUTPUT_FRAME: u64 = 116_129;
    const PRELAUNCH_FRAMES: usize = BLOCK_FRAMES * 8;
    const EXPECTED_WARP_MAP_REVISION: u64 = 2;

    let code_identity =
        env::var("KITHARA_SCENARIO_CODE_IDENTITY").expect("scenario code identity is required");
    let dirty_scope =
        env::var("KITHARA_SCENARIO_DIRTY_SCOPE").expect("scenario dirty scope is required");
    let recorder = probe_capture::install();
    let case = SyncCase::running(control.id, 1, 48_000, OperationOrder::PlaySyncSeek)
        .paused()
        .hold(124.0);
    let sources = prepared_sources(control.provider).await;
    let mut harness = ProductHarness::new_for_block(case, &sources, 0, BLOCK_FRAMES).await;
    let source_grid = BeatGridSnapshot::segments(
        BeatGridId::allocate().expect("origin-zero source grid id is available"),
        BeatGridRevision::first(),
        BeatGridState::Complete,
        fixture_grid(control.source),
    )
    .expect("origin-zero source score truth creates a complete grid");
    let source_frame = |ordinal| match source_grid.position_at(MapPoint::new(
        source_grid.stamp(),
        Beat::try_from(kithara::warp::BeatOrdinal::new(ordinal))
            .expect("origin-zero source beat is valid"),
    )) {
        BeatGridQuery::Resolved(position) => match *position.value().value() {
            MapPosition::Asset(frame) => f64::from(frame)
                .round()
                .to_u64()
                .expect("origin-zero source frame fits u64"),
            position => panic!("origin-zero source grid returned non-asset position {position:?}"),
        },
        query => panic!("origin-zero source grid did not resolve source beat: {query:?}"),
    };
    let selected_source_frame = source_frame(control.selected_source_beat);
    let next_source_frame = source_frame(control.selected_source_beat + 1);
    assert_eq!(selected_source_frame, control.selected_source_frame);
    assert_eq!(next_source_frame, control.next_source_frame);

    prepare_fixture_grids(&mut harness, case, &sources).await;
    let prelaunch = harness.render(case, PRELAUNCH_FRAMES).await;
    assert!(prelaunch.iter().all(|sample| *sample == 0.0));
    assert_eq!(harness.rendered_frames, REQUEST_OUTPUT_FRONTIER);
    if control.requested_source_frame != 0 {
        harness.mark("origin-zero-single-deck-source-start");
        harness.decks[0]
            .seek(control.requested_source_seconds)
            .unwrap_or_else(|error| panic!("{}: source start: {error}", control.id));
    }
    harness.mark("origin-zero-single-deck-enable");
    harness.request_sync(case).await;
    harness.mark("origin-zero-single-deck-play");
    harness.play_all().await;
    let capture_start = harness.rendered_frames;
    let capture_frames = usize::try_from(
        NEXT_OUTPUT_FRAME
            .checked_add(u64::try_from(BLOCK_FRAMES).expect("block frames fit u64"))
            .and_then(|end| end.checked_sub(capture_start))
            .expect("capture covers the second selected beat"),
    )
    .expect("capture span fits usize");
    let capture = harness
        .capture_frames(case, capture_frames, harness.block_frames)
        .await;
    let left = lane_samples(&capture, 0);
    let activation_offset = usize::try_from(ASSIGNED_OUTPUT_FRAME - capture_start)
        .expect("assigned output falls in capture");
    let pre_activation_nonzero = left[..activation_offset]
        .iter()
        .filter(|sample| **sample != 0.0)
        .count();
    let post_activation_nonzero = left[activation_offset..]
        .iter()
        .filter(|sample| **sample != 0.0)
        .count();
    let first_nonzero = left
        .iter()
        .position(|sample| *sample != 0.0)
        .map(|frame| capture_start + u64::try_from(frame).expect("capture frame fits u64"));
    let (markers, _) = lane_score_markers(&capture, 0, capture_start, case.sample_rate);
    let underruns = harness.underrun_failures();
    let gate = kithara::play::DEFAULT_GATE_SMOOTHING;
    let gate_settled_frames = (f64::from(case.sample_rate)
        * f64::from(gate.smooth_seconds)
        * (1.0 / (2.0 * f64::from(gate.settle_epsilon))).ln())
    .ceil() as usize
        + harness.block_frames;
    let equal_rate_waveform = (control.source_bpm == 124).then(|| {
        let fixture = by_name(control.source).expect("origin-zero fixture is registered");
        let source = fixture
            .bytes()
            .get(44..)
            .expect("origin-zero WAV has its PCM payload");
        let expected = source
            .chunks_exact(4)
            .skip(control.selected_source_frame as usize + gate_settled_frames)
            .take(
                control.next_source_frame as usize
                    - control.selected_source_frame as usize
                    - gate_settled_frames,
            )
            .map(|frame| f32::from(i16::from_le_bytes([frame[0], frame[1]])) / 32_768.0_f32);
        let actual = left[activation_offset + gate_settled_frames
            ..activation_offset + control.next_source_frame as usize
                - control.selected_source_frame as usize]
            .iter()
            .copied();
        expected
            .zip(actual)
            .all(|(expected, actual)| expected.to_bits() == actual.to_bits())
    });
    let continuous_host_beat_frames = f64::from(case.sample_rate) * 60.0 / 124.0;
    let expected_target_rate_bits =
        ((next_source_frame - selected_source_frame) as f64 / continuous_host_beat_frames) as f32;
    let expected_target_rate_bits = expected_target_rate_bits.to_bits();
    let probe_events = recorder
        .snapshot()
        .into_iter()
        .filter(|event| {
            matches!(
                event.probe_name(),
                Some(
                    "warp_plan_published"
                        | "prepared_launch_command_admitted"
                        | "prepared_launch_seek_begun"
                        | "prepared_launch_readiness_checked"
                        | "prepared_render_revision_selected"
                        | "decoder_seek_epoch_observed"
                        | "producer_pcm_admitted"
                        | "scheduled_seek_activated"
                        | "pcm_consumed"
                        | "pcm_underrun"
                )
            )
        })
        .map(|event| {
            serde_json::json!({
                "probe": event.probe_name(),
                "seq": event.seq(),
                "thread_id": event.thread_id(),
                "fields": event.fields,
                "strings": event.string_fields,
            })
        })
        .collect::<Vec<_>>();
    if let Some(tap) = harness.tap.as_mut() {
        tap.evidence(
            "single_deck_origin_zero_control",
            serde_json::json!({
                "verdict": "capture-complete-before-assertions",
                "fixture_generation": "generated rhythm score with source origin 0",
                "source": control.source,
                "source_bpm": control.source_bpm,
                "requested_source": {
                    "seconds": control.requested_source_seconds,
                    "frame": control.requested_source_frame,
                },
                "selected_source": {
                    "beat": control.selected_source_beat,
                    "frame": control.selected_source_frame,
                    "next_beat_frame": control.next_source_frame,
                },
                "host": { "bpm": 124, "beat_4": ASSIGNED_OUTPUT_FRAME, "beat_5": NEXT_OUTPUT_FRAME },
                "rate": control.rate,
                "capture": {
                    "start": capture_start,
                    "frames": capture_frames,
                    "pre_activation_nonzero_samples": pre_activation_nonzero,
                    "post_activation_nonzero_samples": post_activation_nonzero,
                    "first_nonzero_host_frame": first_nonzero,
                    "score_marker_host_frames": markers,
                    "gate_settled_frames": gate_settled_frames,
                    "equal_rate_waveform_exact_after_gate": equal_rate_waveform,
                    "underruns": underruns,
                    "probes": probe_events,
                },
                "code_identity": code_identity,
                "dirty_scope": dirty_scope,
            }),
        );
    }
    drop(harness);
    let warp_plan = probe_events
        .iter()
        .find(|event| event["probe"] == "warp_plan_published")
        .expect("prepared launch publishes a warp plan");
    assert_eq!(
        warp_plan["fields"]["activation_source"].as_u64(),
        Some(control.selected_source_frame),
        "{}: warp activation source",
        control.id
    );
    assert_eq!(
        warp_plan["fields"]["presentation_source"].as_u64(),
        Some(control.requested_source_frame),
        "{}: warp presentation source",
        control.id
    );
    assert_eq!(
        warp_plan["fields"]["preparation_source"].as_u64(),
        Some(control.requested_source_frame),
        "{}: warp preparation source",
        control.id
    );
    assert_eq!(
        warp_plan["fields"]["activation_output"].as_u64(),
        Some(ASSIGNED_OUTPUT_FRAME),
        "{}: warp activation output",
        control.id
    );
    assert_eq!(
        warp_plan["fields"]["warp_map_revision"].as_u64(),
        Some(EXPECTED_WARP_MAP_REVISION),
        "{}: warp map revision",
        control.id
    );
    let admitted_launch = probe_events
        .iter()
        .find(|event| {
            event["probe"] == "prepared_launch_command_admitted"
                && event["fields"]["activation_output"].as_i64()
                    == Some(ASSIGNED_OUTPUT_FRAME as i64)
                && event["fields"]["warp_map_revision"].as_u64() == Some(EXPECTED_WARP_MAP_REVISION)
                && event["fields"]["armed"].as_u64() == Some(1)
        })
        .expect("prepared launch command is admitted for the selected map and host phase");
    let launch_epoch = admitted_launch["fields"]["seek_epoch"]
        .as_u64()
        .expect("admitted prepared launch carries its decoder epoch");
    assert!(
        probe_events.iter().any(|event| {
            event["probe"] == "decoder_seek_epoch_observed"
                && event["fields"]["current_epoch"].as_u64() == Some(launch_epoch)
                && event["fields"]["adopted"].as_u64() == Some(1)
        }),
        "{}: decoder must adopt the admitted prepared-launch epoch {launch_epoch}",
        control.id
    );
    let selected_render = probe_events
        .iter()
        .find(|event| {
            event["probe"] == "prepared_render_revision_selected"
                && event["fields"]["warp_map_revision"].as_u64() == Some(EXPECTED_WARP_MAP_REVISION)
                && event["fields"]["source_frame_offset"].as_u64()
                    == Some(control.selected_source_frame)
                && event["fields"]["target_rate_bits"].as_u64()
                    == Some(u64::from(expected_target_rate_bits))
        })
        .expect("renderer selects the admitted launch segment at its expected physical rate");
    let admitted_pcm = probe_events
        .iter()
        .find(|event| {
            event["probe"] == "producer_pcm_admitted"
                && event["fields"]["seek_epoch"].as_u64() == Some(launch_epoch)
                && event["fields"]["source_start"].as_u64() == Some(control.selected_source_frame)
                && event["fields"]["render_revision"].as_u64()
                    == selected_render["fields"]["render_revision"].as_u64()
        })
        .expect("the selected renderer segment is admitted for the launch decoder epoch");
    let first_pcm = probe_events
        .iter()
        .find(|event| {
            event["probe"] == "pcm_consumed"
                && event["fields"]["source_start"].as_u64() == Some(control.selected_source_frame)
                && event["fields"]["output_start"].as_u64() == Some(ASSIGNED_OUTPUT_FRAME)
        })
        .expect("prepared launch consumes PCM");
    assert_eq!(
        first_pcm["fields"]["source_start"].as_u64(),
        Some(control.selected_source_frame),
        "{}: first consumed PCM source",
        control.id
    );
    assert_eq!(
        first_pcm["fields"]["output_start"].as_u64(),
        Some(ASSIGNED_OUTPUT_FRAME),
        "{}: first consumed PCM output",
        control.id
    );
    assert_eq!(
        first_pcm["fields"]["render_revision"].as_u64(),
        admitted_pcm["fields"]["render_revision"].as_u64(),
        "{}: first consumed PCM uses the admitted renderer-selected launch revision",
        control.id
    );
    assert_eq!(
        pre_activation_nonzero, 0,
        "{}: PCM escaped before Host beat 4",
        control.id
    );
    assert!(
        post_activation_nonzero > 0,
        "{}: no PCM after Host beat 4",
        control.id
    );
    assert!(
        underruns.is_empty(),
        "{}: {}",
        control.id,
        underruns.join("; ")
    );
    assert_eq!(
        first_nonzero,
        Some(ASSIGNED_OUTPUT_FRAME),
        "{}: beat 0 PCM onset",
        control.id
    );
    assert!(
        markers.contains(&NEXT_OUTPUT_FRAME),
        "{}: source beat 1 must mark Host beat 5 at {NEXT_OUTPUT_FRAME}; markers={markers:?}",
        control.id
    );
    assert!(
        equal_rate_waveform.unwrap_or(true),
        "{}: rate-1 PCM differs from source after the configured gate settles",
        control.id
    );
}

#[derive(Clone, Copy)]
struct TrackStartPickup {
    id: &'static str,
    provider: Provider,
    source_downbeat_frame: u64,
    seek_seconds: Option<f64>,
    expected_source_frame: u64,
    expected_host_onset: u64,
    expected_next_host_marker: u64,
}

#[ignore = "ignored-red: TrackStart pickup launch requires real PCM proof, 2026-09-12"]
#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
#[case::house_124(TrackStartPickup { id: "track-start-pickup-124", provider: Provider::Rhythm(PICKUP_HOUSE_124), source_downbeat_frame: 23_226, seek_seconds: None, expected_source_frame: 0, expected_host_onset: 69_677, expected_next_host_marker: 92_903 })]
#[case::downtempo_96(TrackStartPickup { id: "track-start-pickup-96", provider: Provider::Rhythm(PICKUP_DOWNTEMPO_96), source_downbeat_frame: 30_000, seek_seconds: None, expected_source_frame: 0, expected_host_onset: 69_677, expected_next_host_marker: 92_903 })]
#[case::house_124_seek_zero(TrackStartPickup { id: "track-start-pickup-124-seek-zero", provider: Provider::Rhythm(PICKUP_HOUSE_124), source_downbeat_frame: 23_226, seek_seconds: Some(0.0), expected_source_frame: 23_226, expected_host_onset: 92_903, expected_next_host_marker: 116_129 })]
#[case::downtempo_96_seek_zero(TrackStartPickup { id: "track-start-pickup-96-seek-zero", provider: Provider::Rhythm(PICKUP_DOWNTEMPO_96), source_downbeat_frame: 30_000, seek_seconds: Some(0.0), expected_source_frame: 30_000, expected_host_onset: 92_903, expected_next_host_marker: 116_129 })]
#[case::house_124_seek_ten(TrackStartPickup { id: "track-start-pickup-124-seek-ten", provider: Provider::Rhythm(PICKUP_HOUSE_124), source_downbeat_frame: 23_226, seek_seconds: Some(10.0), expected_source_frame: 487_746, expected_host_onset: 92_903, expected_next_host_marker: 116_129 })]
#[case::downtempo_96_seek_ten(TrackStartPickup { id: "track-start-pickup-96-seek-ten", provider: Provider::Rhythm(PICKUP_DOWNTEMPO_96), source_downbeat_frame: 30_000, seek_seconds: Some(10.0), expected_source_frame: 510_000, expected_host_onset: 92_903, expected_next_host_marker: 116_129 })]
async fn track_start_pickup_reaches_real_pcm_at_its_host_phase(#[case] pickup: TrackStartPickup) {
    let recorder = probe_capture::install();
    let case = SyncCase::running(pickup.id, 1, 48_000, OperationOrder::PlaySyncSeek)
        .paused()
        .hold(124.0);
    let sources = prepared_sources(pickup.provider).await;
    let mut harness = ProductHarness::new_track_start(case, &sources, 0).await;
    prepare_fixture_grids(&mut harness, case, &sources).await;
    let _ = harness.render(case, BLOCK_FRAMES * 8).await;
    if let Some(seconds) = pickup.seek_seconds {
        harness.decks[0]
            .seek(seconds)
            .expect("public seek cancels the pending TrackStart cue");
    }
    harness.request_sync(case).await;
    harness.play_all().await;
    let capture_start = harness.rendered_frames;
    let capture = harness
        .capture_frames(case, 120_000, harness.block_frames)
        .await;
    let left = lane_samples(&capture, 0);
    let (markers, _) = lane_score_markers(&capture, 0, capture_start, case.sample_rate);
    let first = left
        .iter()
        .position(|sample| *sample != 0.0)
        .map(|frame| capture_start + u64::try_from(frame).expect("capture frame fits u64"));
    let equal_rate_waveform = (pickup.source_downbeat_frame == 23_226).then(|| {
        let gate = kithara::play::DEFAULT_GATE_SMOOTHING;
        let settled = (f64::from(case.sample_rate)
            * f64::from(gate.smooth_seconds)
            * (1.0 / (2.0 * f64::from(gate.settle_epsilon))).ln())
        .ceil() as usize
            + harness.block_frames;
        let source = by_name("rhythm_wav_scenario_1_origin_zero_pickup_long_house_124_left_only")
            .expect("pickup fixture is registered")
            .bytes()
            .get(44..)
            .expect("pickup WAV has PCM");
        let span = 23_226_usize.saturating_sub(settled);
        assert!(span > 0, "post-gate comparison must be nonempty");
        let output = usize::try_from(pickup.expected_host_onset - capture_start)
            .expect("onset is in capture");
        source
            .chunks_exact(4)
            .skip(pickup.expected_source_frame as usize + settled)
            .take(span)
            .map(|frame| f32::from(i16::from_le_bytes([frame[0], frame[1]])) / 32_768.0)
            .zip(
                left[output + settled..output + settled + span]
                    .iter()
                    .copied(),
            )
            .all(|(expected, actual)| expected.to_bits() == actual.to_bits())
    });
    let underruns = harness.underrun_failures();
    let events = recorder.snapshot();
    harness
        .tap
        .as_mut()
        .expect("TrackStart product proof requires KITHARA_AUDIO_ARTIFACT_DIR")
        .evidence(
            "track_start_pickup_124",
            serde_json::json!({
                "verdict": "capture-complete-before-assertions",
                "source": { "frame": pickup.expected_source_frame, "downbeat": 1 },
                "seek_seconds": pickup.seek_seconds,
                "source_downbeat_frame": pickup.source_downbeat_frame,
                "host": { "onset": first, "expected_onset": pickup.expected_host_onset, "next_marker": pickup.expected_next_host_marker },
                "capture_origin": capture_start,
                "markers": markers,
                "underruns": underruns,
            }),
        );
    drop(harness);
    assert_eq!(first, Some(pickup.expected_host_onset));
    assert!(markers.contains(&pickup.expected_next_host_marker));
    assert!(underruns.is_empty());
    assert!(equal_rate_waveform.unwrap_or(true));
    let plan = events
        .iter()
        .find(|event| event.probe_name() == Some("warp_plan_published"))
        .expect("prepared launch publishes a warp plan");
    assert_eq!(
        plan.fields["activation_source"],
        pickup.expected_source_frame
    );
    assert_eq!(plan.fields["activation_output"], pickup.expected_host_onset);
}

#[ignore = "ignored-red: late grid must arm an already-requested prepared launch, 2026-09-12"]
#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
#[case::grid_before_play(true)]
#[case::grid_after_play(false)]
async fn late_grid_preserves_requested_playback(#[case] grid_before_play: bool) {
    let recorder = probe_capture::install();
    let case = SyncCase::running(
        "late-grid-track-start-124",
        1,
        48_000,
        OperationOrder::PlaySyncSeek,
    )
    .paused()
    .hold(124.0);
    let sources = prepared_sources(Provider::Rhythm(PICKUP_HOUSE_124)).await;
    let mut harness = ProductHarness::new_track_start(case, &sources, 0).await;
    let _ = harness.render(case, BLOCK_FRAMES * 8).await;
    if grid_before_play {
        prepare_fixture_grids(&mut harness, case, &sources).await;
    }
    harness.request_sync(case).await;
    harness.play_all().await;
    if !grid_before_play {
        let grid = fixture_grid(PICKUP_HOUSE_124[0]);
        let admission = harness
            .publish_track_grid(0, harness.ids[0][0], grid, BeatGridState::Complete)
            .await
            .expect("late grid publication is admitted");
        assert!(
            matches!(admission, SyncAdmission::Prepared { .. }),
            "late grid must prepare the requested TrackStart launch, got {admission:?}"
        );
    }
    let capture_start = harness.rendered_frames;
    let capture = harness
        .capture_frames(case, 100_000, harness.block_frames)
        .await;
    let left = lane_samples(&capture, 0);
    let first = left
        .iter()
        .position(|sample| *sample != 0.0)
        .map(|frame| capture_start + u64::try_from(frame).expect("capture frame fits"));
    let probe_events = recorder
        .snapshot()
        .into_iter()
        .filter(|event| {
            matches!(
                event.probe_name(),
                Some(
                    "warp_plan_published"
                        | "prepared_launch_command_admitted"
                        | "prepared_launch_seek_begun"
                        | "prepared_launch_readiness_checked"
                        | "decoder_seek_epoch_observed"
                        | "producer_pcm_admitted"
                        | "scheduled_seek_activated"
                        | "pcm_consumed"
                        | "pcm_underrun"
                )
            )
        })
        .map(|event| {
            serde_json::json!({
                "probe": event.probe_name(),
                "seq": event.seq(),
                "fields": event.fields,
                "strings": event.string_fields,
            })
        })
        .collect::<Vec<_>>();
    if let Some(tap) = harness.tap.as_mut() {
        tap.evidence(
            "late_grid_lifecycle",
            serde_json::json!({
                "grid_before_play": grid_before_play,
                "first_pcm": first,
                "probe_events": probe_events,
            }),
        );
    }
    assert_eq!(first, Some(69_677));
}

#[ignore = "ignored-red: Disable must cancel an unarmed late-grid launch, 2026-09-13"]
#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
async fn disable_after_paused_late_grid_cannot_rearm_the_old_launch() {
    let recorder = probe_capture::install();
    let case = SyncCase::running(
        "disable-paused-late-grid",
        1,
        48_000,
        OperationOrder::PlaySyncSeek,
    )
    .paused()
    .hold(124.0);
    let sources = prepared_sources(Provider::Rhythm(PICKUP_HOUSE_124)).await;
    let mut harness = ProductHarness::new_track_start(case, &sources, 0).await;
    harness.request_sync(case).await;
    harness.play_all().await;
    let controls: Vec<_> = harness
        .decks
        .iter()
        .map(|deck| deck.control().clone())
        .collect();
    harness
        .host
        .run(move || controls.iter().for_each(|control| control.pause()))
        .await;
    let grid = fixture_grid(PICKUP_HOUSE_124[0]);
    assert!(matches!(
        harness
            .publish_track_grid(0, harness.ids[0][0], grid, BeatGridState::Complete)
            .await,
        Ok(SyncAdmission::Prepared { .. })
    ));
    harness.request_sync_intent(case, SyncIntent::Disable).await;
    harness.play_all().await;
    let capture = harness
        .capture_frames(case, 24_000, harness.block_frames)
        .await;
    assert!(
        lane_samples(&capture, 0)
            .iter()
            .any(|sample| *sample != 0.0)
    );
    assert!(
        !recorder
            .snapshot()
            .iter()
            .any(|event| event.probe_name() == Some("prepared_launch_seek_begun"))
    );
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
async fn disable_after_admitted_late_grid_cannot_start_the_old_launch() {
    let recorder = probe_capture::install();
    let case = SyncCase::running(
        "disable-admitted-late-grid",
        1,
        48_000,
        OperationOrder::PlaySyncSeek,
    )
    .paused()
    .hold(124.0);
    let sources = prepared_sources(Provider::Rhythm(PICKUP_HOUSE_124)).await;
    let mut harness = ProductHarness::new_track_start(case, &sources, 0).await;
    harness.request_sync(case).await;
    harness.play_all().await;
    let grid = fixture_grid(PICKUP_HOUSE_124[0]);
    assert!(matches!(
        harness
            .publish_track_grid(0, harness.ids[0][0], grid, BeatGridState::Complete)
            .await,
        Ok(SyncAdmission::Prepared { .. })
    ));
    let _ = harness.render(case, harness.block_frames).await;
    let admitted = recorder
        .snapshot()
        .into_iter()
        .find(|event| {
            event.probe_name() == Some("prepared_launch_command_admitted")
                && event.fields["armed"] == 1
        })
        .expect("prepared launch is admitted before Disable");
    let old_epoch = admitted.fields["seek_epoch"];
    let events_before_disable = recorder.snapshot().len();
    let served_target = harness.player_controls[0]
        .position_seconds()
        .expect("active track exposes its served-media position before Disable");
    let served_target_source = (served_target * f64::from(case.sample_rate)) as u64;

    harness.request_sync_intent(case, SyncIntent::Disable).await;
    let capture = harness
        .capture_frames(case, 100_000, harness.block_frames)
        .await;
    assert!(
        lane_samples(&capture, 0)
            .iter()
            .any(|sample| *sample != 0.0)
    );
    let after_disable = &recorder.snapshot()[events_before_disable..];
    assert!(after_disable.iter().any(|event| {
        event.probe_name() == Some("prepared_launch_cancelled")
            && event.fields["item_id"] == harness.ids[0][0].as_u64()
            && event.fields["prepared_seek_epoch"] == old_epoch
            && event.fields["replacement_seek_epoch"] != old_epoch
            && event.fields["presented"] == 1
    }));
    let replacement_epoch = after_disable
        .iter()
        .find(|event| {
            event.probe_name() == Some("prepared_launch_cancelled")
                && event.fields["prepared_seek_epoch"] == old_epoch
                && event.fields["presented"] == 1
        })
        .expect("Disable presents its replacement prepared seek")
        .fields["replacement_seek_epoch"];
    assert!(!after_disable.iter().any(|event| {
        event.probe_name() == Some("prepared_launch_readiness_checked")
            && event.fields["expected_activation"] == 69_677
    }));
    assert!(!after_disable.iter().any(|event| {
        event.probe_name() == Some("scheduled_seek_activated")
            && event.fields["seek_epoch"] == old_epoch
    }));
    let events = recorder.snapshot();
    assert!(events.iter().any(|event| {
        event.probe_name() == Some("producer_pcm_admitted")
            && event.fields["seek_epoch"] == old_epoch
    }));
    assert!(!events.iter().any(|event| {
        event.probe_name() == Some("pcm_reader_admitted") && event.fields["seek_epoch"] == old_epoch
    }));
    let replacement_pcm = events
        .iter()
        .find(|event| {
            event.probe_name() == Some("pcm_reader_admitted")
                && event.fields["seek_epoch"] == replacement_epoch
        })
        .expect("Disable admits replacement PCM");
    let replacement_source_start = replacement_pcm.fields["source_start"];
    let source_tolerance = u64::try_from(harness.block_frames)
        .expect("fixture block size fits source-frame tolerance");
    assert!(
        replacement_source_start.abs_diff(served_target_source) <= source_tolerance,
        "Disable replacement source start {replacement_source_start} must follow served target {served_target_source} within {source_tolerance} source frames"
    );
}

#[ignore = "ignored-red: Disable before a grid releases only requested playback, 2026-09-13"]
#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
#[case::playing(true)]
#[case::paused(false)]
async fn disable_before_grid_respects_requested_playback(#[case] play: bool) {
    let case = SyncCase::running(
        "disable-before-grid",
        1,
        48_000,
        OperationOrder::PlaySyncSeek,
    )
    .paused()
    .hold(124.0);
    let sources = prepared_sources(Provider::Rhythm(PICKUP_HOUSE_124)).await;
    let mut harness = ProductHarness::new_track_start(case, &sources, 0).await;
    harness.request_sync(case).await;
    if play {
        harness.play_all().await;
    }
    harness.request_sync_intent(case, SyncIntent::Disable).await;
    let capture = harness
        .capture_frames(case, 24_000, harness.block_frames)
        .await;
    let audible = lane_samples(&capture, 0)
        .iter()
        .any(|sample| *sample != 0.0);
    assert_eq!(audible, play);
}

#[ignore = "ignored-red: paused late grid resumes its prepared Host phase, 2026-09-13"]
#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
async fn paused_late_grid_resumes_at_the_prepared_host_phase() {
    let case = SyncCase::running(
        "paused-late-grid-resume",
        1,
        48_000,
        OperationOrder::PlaySyncSeek,
    )
    .paused()
    .hold(124.0);
    let sources = prepared_sources(Provider::Rhythm(PICKUP_HOUSE_124)).await;
    let mut harness = ProductHarness::new_track_start(case, &sources, 0).await;
    harness.request_sync(case).await;
    harness.play_all().await;
    let controls: Vec<_> = harness
        .decks
        .iter()
        .map(|deck| deck.control().clone())
        .collect();
    harness
        .host
        .run(move || controls.iter().for_each(|control| control.pause()))
        .await;
    let grid = fixture_grid(PICKUP_HOUSE_124[0]);
    assert!(matches!(
        harness
            .publish_track_grid(0, harness.ids[0][0], grid, BeatGridState::Complete)
            .await,
        Ok(SyncAdmission::Prepared { .. })
    ));
    let silent = harness
        .capture_frames(case, 8_000, harness.block_frames)
        .await;
    assert!(silent.iter().all(|sample| *sample == 0.0));
    harness.play_all().await;
    let start = harness.rendered_frames;
    let pcm = harness
        .capture_frames(case, 100_000, harness.block_frames)
        .await;
    let first = lane_samples(&pcm, 0)
        .iter()
        .position(|sample| *sample != 0.0)
        .map(|frame| start + frame as u64);
    assert_eq!(first, Some(69_677));
}

#[ignore = "ignored-red: normal HostSync pause resume keeps PCM continuity, 2026-09-13"]
#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
async fn normal_hostsync_pause_resume_keeps_pcm_continuity() {
    let recorder = probe_capture::install();
    let case = SyncCase::running(
        "normal-pause-resume",
        1,
        48_000,
        OperationOrder::PlaySyncSeek,
    )
    .paused()
    .hold(124.0);
    let sources = prepared_sources(Provider::Rhythm(PICKUP_HOUSE_124)).await;
    let mut harness = ProductHarness::new_track_start(case, &sources, 0).await;
    prepare_fixture_grids(&mut harness, case, &sources).await;
    harness.request_sync(case).await;
    harness.play_all().await;
    harness.settle_sync_activation(case).await;
    let before = harness
        .capture_frames(case, 8_000, harness.block_frames)
        .await;
    assert!(lane_samples(&before, 0).iter().any(|sample| *sample != 0.0));
    let pause_output = harness.rendered_frames;
    let controls: Vec<_> = harness
        .decks
        .iter()
        .map(|deck| deck.control().clone())
        .collect();
    harness
        .host
        .run(move || controls.iter().for_each(|control| control.pause()))
        .await;
    let paused = harness
        .capture_frames(case, 8_000, harness.block_frames)
        .await;
    let gate = kithara::play::DEFAULT_GATE_SMOOTHING;
    let settling_frames = (-(f64::from(case.sample_rate) * f64::from(gate.smooth_seconds))
        * f64::from(gate.settle_epsilon).ln())
    .ceil() as usize;
    let gate_tail_frames = settling_frames.div_ceil(harness.block_frames) * harness.block_frames;
    let paused = lane_samples(&paused, 0);
    let (_, silence) = paused.split_at(gate_tail_frames);
    assert!(
        silence.iter().all(|sample| *sample == 0.0),
        "paused PCM outlived the {gate_tail_frames}-frame gate tail"
    );
    let resume_output = harness.rendered_frames;
    harness.play_all().await;
    let after = harness
        .capture_frames(case, 8_000, harness.block_frames)
        .await;
    assert!(lane_samples(&after, 0).iter().any(|sample| *sample != 0.0));
    let consumed = recorder
        .snapshot()
        .into_iter()
        .filter(|event| event.probe_name() == Some("pcm_consumed"))
        .map(|event| {
            serde_json::json!({
                "output_start": event.fields["output_start"],
                "output_end": event.fields["output_end"],
                "source_start": event.fields["source_start"],
                "source_end": event.fields["source_end"],
            })
        })
        .collect::<Vec<_>>();
    let pause_tail_end = pause_output + gate_tail_frames as u64;
    let last_paused = consumed
        .iter()
        .filter(|event| {
            event["output_start"]
                .as_u64()
                .is_some_and(|start| start >= pause_output && start < resume_output)
        })
        .last()
        .expect("the pause ramp consumes the held source");
    assert!(
        last_paused["output_end"]
            .as_u64()
            .is_some_and(|end| end <= pause_tail_end),
        "pause consumed source after the derived gate tail: {last_paused}"
    );
    assert!(
        !consumed.iter().any(|event| {
            event["output_start"]
                .as_u64()
                .is_some_and(|start| start >= pause_tail_end && start < resume_output)
        }),
        "source advanced after the pause gate settled"
    );
    let first_resumed = consumed
        .iter()
        .find(|event| {
            event["output_start"]
                .as_u64()
                .is_some_and(|start| start >= resume_output)
        })
        .expect("resume consumes PCM after the held source");
    assert_eq!(
        first_resumed["source_start"], last_paused["source_end"],
        "resume must continue at the source frontier held after the pause ramp"
    );
}

#[derive(Clone, Copy)]
struct ListeningScenario {
    id: &'static str,
    source: &'static str,
    cue_in: CueIn,
    seek_seconds: Option<f64>,
    publish_grid_after_play: bool,
    expected_activation: u64,
}

#[ignore = "writes 30-second one-deck Host-metronome previews, 2026-09-12"]
#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
#[case::first_downbeat_96_start_10(ListeningScenario {
    id: "listening-first-downbeat-96-start-10",
    source: "rhythm_wav_scenario_1_origin_zero_listening_long_downtempo_96_stereo_55s",
    cue_in: CueIn::FirstDownbeat,
    seek_seconds: Some(10.0),
    publish_grid_after_play: false,
    expected_activation: 92_903,
})]
#[case::first_downbeat_pickup_96(ListeningScenario {
    id: "listening-first-downbeat-pickup-96",
    source: "rhythm_wav_scenario_1_origin_zero_pickup_listening_downtempo_96_stereo_45s",
    cue_in: CueIn::FirstDownbeat,
    seek_seconds: None,
    publish_grid_after_play: false,
    expected_activation: 92_903,
})]
#[case::track_start_pickup_96(ListeningScenario {
    id: "listening-track-start-pickup-96",
    source: "rhythm_wav_scenario_1_origin_zero_pickup_listening_downtempo_96_stereo_45s",
    cue_in: CueIn::TrackStart,
    seek_seconds: None,
    publish_grid_after_play: false,
    expected_activation: 69_677,
})]
#[case::late_grid_track_start_96(ListeningScenario {
    id: "listening-late-grid-track-start-96",
    source: "rhythm_wav_scenario_1_origin_zero_pickup_listening_downtempo_96_stereo_45s",
    cue_in: CueIn::TrackStart,
    seek_seconds: None,
    publish_grid_after_play: true,
    expected_activation: 69_677,
})]
async fn listening_single_deck_host_metronome_preview(#[case] scenario: ListeningScenario) {
    const PRELAUNCH_FRAMES: usize = BLOCK_FRAMES * 8;
    const AUDIBLE_CAPTURE_FRAMES: usize = 48_000 * 30;
    let case = SyncCase::running(scenario.id, 1, 48_000, OperationOrder::PlaySyncSeek)
        .paused()
        .hold(124.0);
    let provider = match scenario.source {
        "rhythm_wav_scenario_1_origin_zero_listening_long_downtempo_96_stereo_55s" => {
            Provider::Rhythm(LISTENING_ALIGNED_DOWNTEMPO_96)
        }
        "rhythm_wav_scenario_1_origin_zero_pickup_listening_downtempo_96_stereo_45s" => {
            Provider::Rhythm(LISTENING_PICKUP_DOWNTEMPO_96)
        }
        _ => panic!("{}: unregistered listening source", scenario.id),
    };
    let sources = prepared_sources(provider).await;
    let mut harness = match scenario.cue_in {
        CueIn::FirstDownbeat => {
            ProductHarness::new_for_block(case, &sources, 0, BLOCK_FRAMES).await
        }
        CueIn::TrackStart => ProductHarness::new_track_start(case, &sources, 0).await,
        _ => panic!("{}: unsupported CueIn preview", scenario.id),
    };
    if !scenario.publish_grid_after_play {
        prepare_fixture_grids(&mut harness, case, &sources).await;
    }
    let prelaunch = harness.render(case, PRELAUNCH_FRAMES).await;
    assert!(prelaunch.iter().all(|sample| *sample == 0.0));
    if let Some(seconds) = scenario.seek_seconds {
        harness.decks[0]
            .seek(seconds)
            .unwrap_or_else(|error| panic!("{}: source seek: {error}", scenario.id));
    }
    harness.mark("listening-enable");
    harness.request_sync(case).await;
    harness.mark("listening-play");
    harness.play_all().await;
    if scenario.publish_grid_after_play {
        let grid = fixture_grid(scenario.source);
        let admission = harness
            .publish_track_grid(0, harness.ids[0][0], grid, BeatGridState::Complete)
            .await
            .unwrap_or_else(|error| panic!("{}: late grid: {error}", scenario.id));
        assert!(matches!(admission, SyncAdmission::Prepared { .. }));
    }
    if !scenario.publish_grid_after_play {
        harness.settle_sync_activation(case).await;
        assert_eq!(
            harness.sync_activation,
            Some(scenario.expected_activation),
            "{}: prepared activation",
            scenario.id
        );
    }
    let audible_start = harness.rendered_frames;
    let raw = harness
        .capture_frames(case, AUDIBLE_CAPTURE_FRAMES, harness.block_frames)
        .await;
    let raw_before_reference = raw.clone();
    let host_grid = harness
        .host_grid
        .as_ref()
        .expect("sync request retains Host grid");
    let total_output_frames = harness.rendered_frames;
    let (reference, _host_markers) = host_grid_reference(host_grid, total_output_frames);
    let activation = harness.sync_activation;
    if !scenario.publish_grid_after_play {
        assert_eq!(activation, Some(scenario.expected_activation));
    }
    let reference_offset =
        usize::try_from(audible_start).expect("capture start fits usize") * usize::from(CHANNELS);
    let reference_capture = &reference[reference_offset..reference_offset + raw.len()];
    let (preview, preview_clips) = host_grid_preview(&raw, reference_capture);
    assert_eq!(
        preview_clips, 0,
        "derived listening preview must have headroom"
    );
    let underruns = harness.underrun_failures();
    if let Some(tap) = harness.tap.as_mut() {
        let reference_path = tap
            .write_reference("host-grid-reference", &reference)
            .expect("publish Host-grid metronome WAV");
        let preview_path = tap
            .write_reference("track-host-grid-preview", &preview)
            .expect("publish track and Host-grid preview WAV");
        tap.evidence(
            "single_deck_listening_preview",
            serde_json::json!({
                "verdict": "listening-preview-only",
                "scenario": scenario.id,
                "cue_in": format!("{:?}", scenario.cue_in),
                "source": scenario.source,
                "requested_source_seconds": scenario.seek_seconds,
                "grid_published_after_play": scenario.publish_grid_after_play,
                "expected_host_activation": scenario.expected_activation,
                "prepared_host_activation": activation,
                "audible_capture_start": audible_start,
                "audible_capture_frames": AUDIBLE_CAPTURE_FRAMES,
                "host_grid_reference": reference_path,
                "track_host_grid_preview": preview_path,
                "mix": {"track_gain": 0.8, "metronome_gain": 0.2, "preview_clipped_samples": preview_clips},
                "underruns": underruns,
                "raw_note": "output.wav is unmodified Host PCM; this artifact does not prove synchronization",
            }),
        );
    }
    assert_eq!(
        raw, raw_before_reference,
        "reference export must not alter raw Host PCM"
    );
    assert!(
        underruns.is_empty(),
        "listening capture underruns: {underruns:?}"
    );
}

fn host_grid_preview(raw: &[f32], reference: &[f32]) -> (Vec<f32>, usize) {
    let mut clipped = 0;
    let preview = raw
        .iter()
        .zip(reference)
        .map(|(track, click)| {
            let sample = *track * 0.8 + *click * 0.2;
            if sample.abs() > 1.0 {
                clipped += 1;
                sample.clamp(-1.0, 1.0)
            } else {
                sample
            }
        })
        .collect();
    (preview, clipped)
}

#[ignore = "ignored-red: real single-deck prepared launch currently produces no post-activation PCM, 2026-09-12"]
#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
async fn scenario_1_single_deck_prepared_launch_reaches_real_pcm(
    #[future(awt)] source_scenario_1_downtempo_house_provider: PreparedSources,
) {
    const REQUEST_OUTPUT_FRONTIER: u64 = 1_152;
    const REQUESTED_SOURCE_FRONTIER: u64 = 0;
    const SELECTED_SOURCE_FRAME: u64 = 30_000;
    const NEXT_SOURCE_BEAT_FRAME: u64 = 60_000;
    const SELECTED_HOST_BEAT: i64 = 4;
    const ASSIGNED_OUTPUT_FRAME: u64 = 92_903;
    const NEXT_HOST_BEAT: i64 = 5;
    const NEXT_OUTPUT_FRAME: u64 = 116_129;
    const PRELAUNCH_FRAMES: usize = BLOCK_FRAMES * 8;

    let recorder = probe_capture::install();
    let case = SyncCase::running(
        "scenario-1-single-deck-prepared-launch",
        1,
        48_000,
        OperationOrder::PlaySyncSeek,
    )
    .paused()
    .hold(124.0);
    let mut harness = ProductHarness::new_for_block(
        case,
        &source_scenario_1_downtempo_house_provider,
        0,
        BLOCK_FRAMES,
    )
    .await;
    let source_grid = BeatGridSnapshot::segments(
        BeatGridId::allocate().expect("Scenario 1 source grid id is available"),
        BeatGridRevision::first(),
        BeatGridState::Complete,
        fixture_grid("rhythm_wav_scenario_1_downtempo_96_left_only"),
    )
    .expect("Scenario 1 source score truth creates a complete grid");
    let source_start = MapPoint::new(
        source_grid.stamp(),
        MapPosition::Asset(
            AssetFrame::new(REQUESTED_SOURCE_FRONTIER as f64).expect("zero source frontier"),
        ),
    );
    let BeatGridQuery::Resolved(selected_beat) = source_grid.beat_at_or_next(source_start) else {
        panic!("Scenario 1 source grid resolves a selected beat");
    };
    assert_eq!(f64::from(*selected_beat.value().value()), 0.0);
    let BeatGridQuery::Resolved(source_meter) = source_grid.meter_at(*selected_beat.value()) else {
        panic!("Scenario 1 source grid resolves its configured meter");
    };
    assert_eq!(source_meter.value().beats_per_bar(), 4);
    assert_eq!(i64::from(source_meter.value().downbeat()), 0);
    let source_frame = |beat| match source_grid
        .position_at(MapPoint::new(source_grid.stamp(), beat))
    {
        BeatGridQuery::Resolved(position) => match *position.value().value() {
            MapPosition::Asset(frame) => f64::from(frame)
                .round()
                .to_u64()
                .expect("Scenario 1 source frame fits u64"),
            position => panic!("Scenario 1 source grid returned non-asset position {position:?}"),
        },
        query => panic!("Scenario 1 source grid did not resolve source beat: {query:?}"),
    };
    assert_eq!(
        source_frame(*selected_beat.value().value()),
        SELECTED_SOURCE_FRAME
    );
    assert_eq!(
        source_frame(
            Beat::try_from(kithara::warp::BeatOrdinal::new(1)).expect("exact source beat one")
        ),
        NEXT_SOURCE_BEAT_FRAME
    );

    prepare_fixture_grids(
        &mut harness,
        case,
        &source_scenario_1_downtempo_house_provider,
    )
    .await;
    let prelaunch = harness.render(case, PRELAUNCH_FRAMES).await;
    assert!(prelaunch.iter().all(|sample| *sample == 0.0));
    assert_eq!(harness.rendered_frames, REQUEST_OUTPUT_FRONTIER);
    harness.mark("scenario-1-single-deck-enable");
    harness.request_sync(case).await;
    let host_frame = |beat: i64| {
        (beat.to_f64().expect("Scenario 1 Host beat converts to f64")
            * f64::from(case.sample_rate)
            * 60.0
            / case.start_bpm())
        .round()
        .to_u64()
        .expect("Scenario 1 configured Host beat fits u64")
    };
    let assigned_output = host_frame(SELECTED_HOST_BEAT);
    let next_output = host_frame(NEXT_HOST_BEAT);
    assert_eq!(assigned_output, ASSIGNED_OUTPUT_FRAME);
    assert_eq!(next_output, NEXT_OUTPUT_FRAME);

    harness.mark("scenario-1-single-deck-play");
    harness.play_all().await;
    let capture_start = harness.rendered_frames;
    let capture_frames = usize::try_from(
        NEXT_OUTPUT_FRAME
            .checked_add(u64::try_from(BLOCK_FRAMES).expect("block frames fit u64"))
            .and_then(|end| end.checked_sub(capture_start))
            .expect("capture covers the second selected beat"),
    )
    .expect("capture span fits usize");
    let capture = harness
        .capture_frames(case, capture_frames, harness.block_frames)
        .await;
    let left = lane_samples(&capture, 0);
    let activation_offset =
        usize::try_from(assigned_output - capture_start).expect("assigned output falls in capture");
    let pre_activation_nonzero = left[..activation_offset]
        .iter()
        .filter(|sample| **sample != 0.0)
        .count();
    let post_activation_nonzero = left[activation_offset..]
        .iter()
        .filter(|sample| **sample != 0.0)
        .count();
    let (markers, _) = lane_score_markers(&capture, 0, capture_start, case.sample_rate);
    let underruns = harness.underrun_failures();
    let probe_events = recorder
        .snapshot()
        .into_iter()
        .filter(|event| {
            matches!(
                event.probe_name(),
                Some(
                    "warp_plan_published"
                        | "prepared_launch_command_admitted"
                        | "prepared_launch_seek_begun"
                        | "prepared_launch_readiness_checked"
                        | "decoder_seek_epoch_observed"
                        | "decoder_seek_epoch_backpressured"
                        | "producer_pcm_admitted"
                        | "scheduled_seek_activated"
                        | "pcm_consumed"
                        | "pcm_underrun"
                )
            )
        })
        .map(|event| {
            serde_json::json!({
                "probe": event.probe_name(),
                "seq": event.seq(),
                "thread_id": event.thread_id(),
                "fields": event.fields,
                "strings": event.string_fields,
            })
        })
        .collect::<Vec<_>>();
    if let Some(tap) = harness.tap.as_mut() {
        tap.evidence(
            "scenario_1_single_deck",
            serde_json::json!({
                "requested_output_frontier": REQUEST_OUTPUT_FRONTIER,
                "requested_source_frontier": REQUESTED_SOURCE_FRONTIER,
                "source_meter": { "beats_per_bar": 4, "downbeat": 0 },
                "host_meter": { "beats_per_bar": 4, "downbeat": 0 },
                "selected_source": { "beat": 0, "frame": SELECTED_SOURCE_FRAME },
                "assigned_host": { "beat": SELECTED_HOST_BEAT, "frame": assigned_output },
                "next_source": { "beat": 1, "frame": NEXT_SOURCE_BEAT_FRAME },
                "next_host": { "beat": NEXT_HOST_BEAT, "frame": next_output },
                "rate": { "host_over_source": "31/24", "source_bpm": 96, "host_bpm": 124 },
                "capture": {
                    "start": capture_start,
                    "frames": capture_frames,
                    "pre_activation_nonzero_samples": pre_activation_nonzero,
                    "post_activation_nonzero_samples": post_activation_nonzero,
                    "score_marker_host_frames": markers,
                    "underruns": underruns,
                    "probes": probe_events,
                },
                "provenance_limit": "A score marker is required for source/phase proof. Nonzero energy alone never passes this regression."
            }),
        );
    }
    drop(harness);
    assert!(
        post_activation_nonzero > 0,
        "Scenario 1 single deck: real decoder/Warp/ring/RT emitted no PCM after assigned Host frame {assigned_output}; selected source {SELECTED_SOURCE_FRAME}, rate 31/24"
    );
    assert_eq!(
        pre_activation_nonzero, 0,
        "Scenario 1 single deck: PCM escaped before assigned Host frame {assigned_output}"
    );
    assert!(
        underruns.is_empty(),
        "Scenario 1 single deck: {}",
        underruns.join("; ")
    );
    assert_eq!(
        markers.first().copied(),
        Some(assigned_output),
        "Scenario 1 single deck: selected source {SELECTED_SOURCE_FRAME} must first mark assigned Host beat {SELECTED_HOST_BEAT} at frame {assigned_output}"
    );
    assert_eq!(
        markers.get(1).copied(),
        Some(next_output),
        "Scenario 1 single deck: source beat 1 frame {NEXT_SOURCE_BEAT_FRAME} must mark Host beat {NEXT_HOST_BEAT} at frame {next_output}"
    );
}

#[ignore = "ignored-red: Scenario 1's same-session stereo-separated capture proves the left deck starts at its native 96 BPM instead of the shared 124 BPM, 2026-09-12"]
#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
async fn scenario_1_simultaneous_different_bpm_exact_grids(
    #[future(awt)] source_scenario_1_downtempo_house_provider: PreparedSources,
) {
    const PRELAUNCH_FRAMES: usize = BLOCK_FRAMES * 8;
    const CAPTURE_FRAMES: usize = 48_000 * 10;
    let case = DOWNTEMPO_HOUSE_SYNC.paused();
    let code_identity =
        env::var("KITHARA_SCENARIO_CODE_IDENTITY").expect("scenario code identity is required");
    let dirty_scope =
        env::var("KITHARA_SCENARIO_DIRTY_SCOPE").expect("scenario dirty scope is required");
    let mut harness = ProductHarness::new_for_block(
        case,
        &source_scenario_1_downtempo_house_provider,
        0,
        BLOCK_FRAMES,
    )
    .await;
    if let Some(tap) = harness.tap.as_mut() {
        tap.evidence(
            "scenario_1",
            serde_json::json!({
                "verdict": "capture-incomplete",
                "fixture_generation": {
                    "deck_0": "rhythm_wav_scenario_1_downtempo_96_left_only / rhythm_expected_analysis_scenario_1_downtempo_96_left_only",
                    "deck_1": "rhythm_wav_scenario_1_house_124_right_only / rhythm_expected_analysis_scenario_1_house_124_right_only",
                    "grid_source": "expected_analysis is serialized BeatArtifact::from(score::truth), not analyzer output"
                },
                "mix_layout": "same-session diagnostic mix: deck 0 left only, deck 1 right only; not a centered production listening mix",
                "fixture_lead_in": "both fixtures retain their one-beat digital-silence count-in; requested source 0 is not an immediate musical downbeat",
                "requested_source_start_frames": [0, 0],
                "prelaunch_frames": PRELAUNCH_FRAMES,
                "capture_frames_after_start": CAPTURE_FRAMES,
                "code_identity": code_identity.clone(),
                "dirty_scope": dirty_scope.clone(),
            }),
        );
    }
    for deck in &harness.decks {
        let control = deck.control().clone();
        harness.host.run(move || control.set_muted(false)).await;
    }
    prepare_fixture_grids(
        &mut harness,
        case,
        &source_scenario_1_downtempo_house_provider,
    )
    .await;

    let prelaunch = harness.render(case, PRELAUNCH_FRAMES).await;
    assert!(
        prelaunch.iter().all(|sample| *sample == 0.0),
        "scenario 1: prelaunch Host output must remain silent"
    );
    harness.mark("scenario-1-enable");
    harness.request_sync(case).await;
    harness.mark("scenario-1-simultaneous-host-start");
    harness.play_all().await;
    let capture_start = harness.rendered_frames;
    let capture = harness
        .capture_frames(case, CAPTURE_FRAMES, harness.block_frames)
        .await;
    let underruns = harness.underrun_failures();
    harness.failures.extend(underruns);

    let report = CochleaReport::measure(&capture, CHANNELS, case.sample_rate);
    let (left_markers, _) = lane_score_markers(&capture, 0, capture_start, case.sample_rate);
    let initial_launch_failures = scenario_1_scheduled_launch_failures(
        &capture,
        &left_markers,
        capture_start,
        harness
            .host_grid
            .as_ref()
            .expect("Scenario 1 sync request records the authoritative Host grid"),
        case.sample_rate,
        case.start_bpm(),
    );
    if let Some(tap) = harness.tap.as_mut() {
        tap.evidence(
            "scenario_1",
            serde_json::json!({
                "verdict": if initial_launch_failures.is_empty() { "pass" } else { "fail" },
                "reason": "same-session L/R fixture routing exposes each deck in the Host output; the left start intervals are checked against the requested shared Host tempo",
                "fixture_generation": {
                    "deck_0": "rhythm_wav_scenario_1_downtempo_96_left_only / rhythm_expected_analysis_scenario_1_downtempo_96_left_only",
                    "deck_1": "rhythm_wav_scenario_1_house_124_right_only / rhythm_expected_analysis_scenario_1_house_124_right_only",
                    "grid_source": "expected_analysis is serialized BeatArtifact::from(score::truth), not analyzer output"
                },
                "mix_layout": "same-session diagnostic mix: deck 0 left only, deck 1 right only; not a centered production listening mix",
                "fixture_lead_in": "both fixtures retain their one-beat digital-silence count-in; requested source 0 is not an immediate musical downbeat",
                "requested_source_start_frames": [0, 0],
                "prelaunch_frames": PRELAUNCH_FRAMES,
                "capture_frames_after_start": CAPTURE_FRAMES,
                "sync_activation_frame": harness.sync_activation,
                "per_lane_early_measurement": {
                    "capture_host_start_frame": capture_start,
                    "left": lane_early_measurement(&capture, 0, capture_start, case.sample_rate),
                    "right": lane_early_measurement(&capture, 1, capture_start, case.sample_rate),
                    "interpretation": "observations use the calibrated score-marker detector without shifts or onset tolerance; they report from each lane's first digital non-silence and detected beat marker through the full capture",
                    "post_activation_left_marker_limitation": "left post-activation PCM remains non-silent, but its peak is below the full-lane relative detector threshold; absent left markers after activation are not a missing-PCM or missing-cue verdict"
                },
                "initial_launch_failures": initial_launch_failures,
                "audible_measurement": report,
                "harness_failures": harness.failures,
                "code_identity": code_identity,
                "dirty_scope": dirty_scope,
            }),
        );
    }
    drop(harness);
    assert!(
        initial_launch_failures.is_empty(),
        "scenario 1 initial shared-tempo contract failed: {}",
        initial_launch_failures.join("; ")
    );
}

fn scenario_1_scheduled_launch_failures(
    capture: &[f32],
    markers: &[u64],
    request_frontier: u64,
    host_grid: &BeatGridSnapshot,
    sample_rate: u32,
    target_bpm: f64,
) -> Vec<String> {
    let selected_source = scenario_1_selected_source();
    let (assigned_output, _) = next_host_downbeat(
        host_grid,
        Meter::new(4).expect("Scenario 1 score fixtures declare 4/4"),
        request_frontier,
    )
    .expect("Scenario 1 Host grid supplies a reachable downbeat");
    let mut failures = Vec::new();
    let left = lane_samples(capture, 0);
    let boundary = usize::try_from(assigned_output.saturating_sub(request_frontier))
        .expect("assigned launch span fits capture indexing")
        .min(left.len());
    if left[..boundary].iter().any(|sample| *sample != 0.0) {
        failures.push(format!(
            "left emitted PCM before selected source {selected_source} may start at Host frame {assigned_output}"
        ));
    }
    match markers.first() {
        Some(actual) if *actual == assigned_output => {}
        Some(actual) => failures.push(format!(
            "left selected source cue {selected_source} appeared at Host frame {actual}, expected assigned Host downbeat {assigned_output}"
        )),
        None => failures.push(format!(
            "left has no detected selected source cue {selected_source} at assigned Host frame {assigned_output}"
        )),
    }
    let target_period = f64::from(sample_rate) * 60.0 / target_bpm;
    let expected_periods = [target_period.floor() as u64, target_period.ceil() as u64];
    for pair in markers.windows(2).take(2) {
        let actual = pair[1] - pair[0];
        if !expected_periods.contains(&actual) {
            failures.push(format!(
                "left score-cue interval {}..{} is {actual} frames, expected {}/{} frames for Host {target_bpm:.3} BPM",
                pair[0], pair[1], expected_periods[0], expected_periods[1]
            ));
        }
    }
    failures
}

fn scenario_1_selected_source() -> u64 {
    let grid = BeatGridSnapshot::segments(
        BeatGridId::allocate().expect("Scenario 1 fixture grid id is available"),
        BeatGridRevision::first(),
        BeatGridState::Complete,
        fixture_grid("rhythm_wav_scenario_1_downtempo_96_left_only"),
    )
    .expect("Scenario 1 fixture grid creates a complete snapshot");
    let start = MapPoint::new(
        grid.stamp(),
        MapPosition::Asset(AssetFrame::new(0.0).expect("zero is a valid asset frame")),
    );
    let BeatGridQuery::Resolved(beat) = grid.beat_at_or_next(start) else {
        panic!("Scenario 1 fixture grid resolves its first mapped beat");
    };
    let BeatGridQuery::Resolved(position) = grid.position_at(*beat.value()) else {
        panic!("Scenario 1 fixture grid resolves its first mapped beat to an asset frame");
    };
    let MapPosition::Asset(frame) = *position.value().value() else {
        panic!("Scenario 1 fixture grid remains asset-positioned");
    };
    f64::from(frame)
        .round()
        .to_u64()
        .expect("Scenario 1 selected source frame fits u64")
}

fn next_host_downbeat(grid: &BeatGridSnapshot, meter: Meter, frontier: u64) -> Option<(u64, i64)> {
    let downbeat = i64::from(meter.downbeat());
    let beats_per_bar = i64::from(meter.beats_per_bar());
    for bar_offset in -64_i64..=64 {
        let ordinal = downbeat.checked_add(bar_offset.checked_mul(beats_per_bar)?)?;
        let beat = Beat::try_from(kithara::warp::BeatOrdinal::new(ordinal)).ok()?;
        let position = match grid.position_at(MapPoint::new(grid.stamp(), beat)) {
            BeatGridQuery::Resolved(position) => position,
            _ => continue,
        };
        let MapPosition::Session(frame) = *position.value().value() else {
            continue;
        };
        let frame = u64::try_from(i64::from(frame)).ok()?;
        if frame >= frontier {
            return Some((frame, ordinal));
        }
    }
    None
}

fn lane_early_measurement(
    interleaved: &[f32],
    channel: usize,
    capture_start: u64,
    sample_rate: u32,
) -> serde_json::Value {
    let samples = lane_samples(interleaved, channel);
    let first_non_silent = samples
        .iter()
        .position(|sample| sample.abs() > f32::EPSILON)
        .map(|frame| capture_start + u64::try_from(frame).expect("capture frame fits u64"));
    let (beat_frames, downbeat_frames) =
        lane_score_markers(interleaved, channel, capture_start, sample_rate);
    let intervals = beat_frames
        .windows(2)
        .map(|pair| {
            let frames = pair[1] - pair[0];
            serde_json::json!({
                "from_host_frame": pair[0],
                "to_host_frame": pair[1],
                "frames": frames,
                "bpm": f64::from(sample_rate) * 60.0 / frames as f64,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "first_digital_non_silent_host_frame": first_non_silent,
        "beat_marker_host_frames": beat_frames,
        "downbeat_marker_host_frames": downbeat_frames,
        "beat_interval_series": intervals,
    })
}

fn lane_score_markers(
    interleaved: &[f32],
    channel: usize,
    capture_start: u64,
    sample_rate: u32,
) -> (Vec<u64>, Vec<u64>) {
    let samples = lane_samples(interleaved, channel);
    let (beats, downbeats) = marked_rhythm_markers(&samples, 1, sample_rate);
    let frames = |markers: Vec<usize>| {
        markers
            .into_iter()
            .map(|frame| capture_start + u64::try_from(frame).expect("capture frame fits u64"))
            .collect::<Vec<_>>()
    };
    (frames(beats), frames(downbeats))
}

fn lane_samples(interleaved: &[f32], channel: usize) -> Vec<f32> {
    interleaved
        .chunks_exact(usize::from(CHANNELS))
        .map(|frame| frame[channel])
        .collect()
}

fn host_grid_reference(grid: &BeatGridSnapshot, frames: u64) -> (Vec<f32>, Vec<(u64, bool)>) {
    let ordinal_at = |frame: u64| {
        let query = grid.beat_at(MapPoint::new(
            grid.stamp(),
            MapPosition::Session(SessionFrame::new(
                i64::try_from(frame).expect("reference frame fits i64"),
            )),
        ));
        let BeatGridQuery::Resolved(beat) = query else {
            panic!("Host grid must resolve the listening reference bounds");
        };
        f64::from(*beat.value().value())
            .round()
            .to_i64()
            .expect("Host beat ordinal fits i64")
    };
    let first = ordinal_at(0);
    let last = ordinal_at(frames.saturating_sub(1));
    let mut pcm = vec![0.0; usize::try_from(frames).expect("reference frames fit usize") * 2];
    let mut markers = Vec::new();
    for ordinal in first..=last {
        let beat = Beat::try_from(kithara::warp::BeatOrdinal::new(ordinal))
            .expect("Host beat ordinal is valid");
        let BeatGridQuery::Resolved(position) = grid.position_at(MapPoint::new(grid.stamp(), beat))
        else {
            continue;
        };
        let MapPosition::Session(position) = *position.value().value() else {
            continue;
        };
        let Ok(frame) = u64::try_from(i64::from(position)) else {
            continue;
        };
        if frame >= frames {
            continue;
        }
        let downbeat = matches!(
            grid.meter_at(MapPoint::new(grid.stamp(), beat)),
            BeatGridQuery::Resolved(meter)
                if (ordinal - i64::from(meter.value().downbeat()))
                    .rem_euclid(i64::from(meter.value().beats_per_bar())) == 0
        );
        const BURST_FRAMES: usize = 480;
        const BEAT_HZ: f32 = 1_760.0;
        const DOWNBEAT_HZ: f32 = 2_200.0;
        let (amplitude, frequency) = if downbeat {
            (0.72, DOWNBEAT_HZ)
        } else {
            (0.45, BEAT_HZ)
        };
        let start = usize::try_from(frame).expect("reference frame fits usize");
        for offset in 0..BURST_FRAMES {
            let Some(index) = start
                .checked_add(offset)
                .filter(|index| *index < frames as usize)
            else {
                break;
            };
            let phase = (offset as f32 * frequency / 48_000.0).fract();
            let envelope = 1.0 - offset as f32 / BURST_FRAMES as f32;
            let sample = amplitude * (phase.mul_add(2.0, -1.0)) * envelope;
            pcm[index * 2] = sample;
            pcm[index * 2 + 1] = sample;
        }
        markers.push((frame, downbeat));
    }
    (pcm, markers)
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

async fn run(case: SyncCase, prepared: PreparedSources) {
    let provider = prepared.0;
    let expected_samples = (f64::from(case.sample_rate) * 60.0 / case.ride.final_bpm() * 6.0)
        .round() as usize
        * usize::from(CHANNELS);
    let mut tracks = Vec::with_capacity(case.decks);
    let mut request_failures = Vec::new();
    for audible_deck in 0..case.decks {
        let mut harness =
            ProductHarness::new_for_block(case, &prepared, audible_deck, BLOCK_FRAMES).await;
        prepare_fixture_grids(&mut harness, case, &prepared).await;
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
        let underruns = harness.underrun_failures();
        harness.failures.extend(underruns);
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
#[case::mp3(source_mp3_same().await)]
#[case::drm(source_hls_same_drm().await)]
async fn encoded_rhythmic_controls_reach_the_pcm_oracle(#[case] prepared: PreparedSources) {
    let provider = prepared.0;
    let mut harness = ProductHarness::new(ONE_DECK, &prepared, 0).await;
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
#[case::play_sync_seek(PLAY_SYNC_SEEK, source_synthetic().await)]
#[case::play_seek_sync(PLAY_SEEK_SYNC, source_synthetic().await)]
#[case::seek_play_sync(SEEK_PLAY_SYNC, source_synthetic().await)]
#[case::seek_sync_play(SEEK_SYNC_PLAY, source_synthetic().await)]
#[case::sync_play_seek(SYNC_PLAY_SEEK, source_synthetic().await)]
#[case::sync_seek_play(SYNC_SEEK_PLAY, source_synthetic().await)]
#[case::sequential_sync(SEQUENTIAL_SYNC, source_synthetic().await)]
#[case::paused_sync_then_play(PAUSED_SYNC, source_synthetic().await)]
#[case::four_deck_sequential_sync(FOUR_DECK_SYNC, source_synthetic().await)]
#[case::tempo_up_120hz(TEMPO_UP_120, source_synthetic().await)]
#[case::tempo_down_30hz(TEMPO_DOWN_30, source_synthetic().await)]
#[case::ambient_trip_hop(AMBIENT_TRIP_HOP_SYNC, source_ambient_trip_hop_provider().await)]
#[case::downtempo_house(DOWNTEMPO_HOUSE_SYNC, source_downtempo_house_provider().await)]
#[case::techno_breakbeat(TECHNO_BREAKBEAT_SYNC, source_techno_breakbeat_provider().await)]
#[case::cross_style_four_deck(CROSS_STYLE_SYNC, source_cross_style_provider().await)]
async fn wav_product_rows_reach_the_pcm_oracle(
    #[case] case: SyncCase,
    #[case] provider: PreparedSources,
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
#[ignore = "ignored-red: product Warp alignment is not implemented"]
#[case::hls_same_play_sync_seek(source_hls_same_plain().await, PLAY_SYNC_SEEK)]
#[case::hls_same_play_seek_sync(source_hls_same_plain().await, PLAY_SEEK_SYNC)]
#[case::hls_same_seek_play_sync(source_hls_same_plain().await, SEEK_PLAY_SYNC)]
#[case::hls_same_seek_sync_play(source_hls_same_plain().await, SEEK_SYNC_PLAY)]
#[case::hls_same_sync_play_seek(source_hls_same_plain().await, SYNC_PLAY_SEEK)]
#[case::hls_same_sync_seek_play(source_hls_same_plain().await, SYNC_SEEK_PLAY)]
#[case::hls_same_sequential_sync(source_hls_same_plain().await, SEQUENTIAL_SYNC)]
#[case::hls_same_paused_sync_then_play(source_hls_same_plain().await, PAUSED_SYNC)]
#[case::hls_same_four_deck_sequential_sync(source_hls_same_plain().await, FOUR_DECK_SYNC)]
#[case::hls_same_tempo_up_120hz(source_hls_same_plain().await, TEMPO_UP_120)]
#[case::hls_same_tempo_down_30hz(source_hls_same_plain().await, TEMPO_DOWN_30)]
#[case::drm_same_play_sync_seek(source_hls_same_drm().await, PLAY_SYNC_SEEK)]
#[case::drm_same_play_seek_sync(source_hls_same_drm().await, PLAY_SEEK_SYNC)]
#[case::drm_same_seek_play_sync(source_hls_same_drm().await, SEEK_PLAY_SYNC)]
#[case::drm_same_seek_sync_play(source_hls_same_drm().await, SEEK_SYNC_PLAY)]
#[case::drm_same_sync_play_seek(source_hls_same_drm().await, SYNC_PLAY_SEEK)]
#[case::drm_same_sync_seek_play(source_hls_same_drm().await, SYNC_SEEK_PLAY)]
#[case::drm_same_sequential_sync(source_hls_same_drm().await, SEQUENTIAL_SYNC)]
#[case::drm_same_paused_sync_then_play(source_hls_same_drm().await, PAUSED_SYNC)]
#[case::drm_same_four_deck_sequential_sync(source_hls_same_drm().await, FOUR_DECK_SYNC)]
#[case::drm_same_tempo_up_120hz(source_hls_same_drm().await, TEMPO_UP_120)]
#[case::drm_same_tempo_down_30hz(source_hls_same_drm().await, TEMPO_DOWN_30)]
#[case::mp3_same_play_sync_seek(source_mp3_same().await, PLAY_SYNC_SEEK)]
#[case::mp3_same_play_seek_sync(source_mp3_same().await, PLAY_SEEK_SYNC)]
#[case::mp3_same_seek_play_sync(source_mp3_same().await, SEEK_PLAY_SYNC)]
#[case::mp3_same_seek_sync_play(source_mp3_same().await, SEEK_SYNC_PLAY)]
#[case::mp3_same_sync_play_seek(source_mp3_same().await, SYNC_PLAY_SEEK)]
#[case::mp3_same_sync_seek_play(source_mp3_same().await, SYNC_SEEK_PLAY)]
#[case::mp3_same_sequential_sync(source_mp3_same().await, SEQUENTIAL_SYNC)]
#[case::mp3_same_paused_sync_then_play(source_mp3_same().await, PAUSED_SYNC)]
#[case::mp3_same_four_deck_sequential_sync(source_mp3_same().await, FOUR_DECK_SYNC)]
#[case::mp3_same_tempo_up_120hz(source_mp3_same().await, TEMPO_UP_120)]
#[case::mp3_same_tempo_down_30hz(source_mp3_same().await, TEMPO_DOWN_30)]
#[case::mp3_distinct_play_sync_seek(source_mp3_distinct().await, PLAY_SYNC_SEEK)]
#[case::mp3_distinct_play_seek_sync(source_mp3_distinct().await, PLAY_SEEK_SYNC)]
#[case::mp3_distinct_seek_play_sync(source_mp3_distinct().await, SEEK_PLAY_SYNC)]
#[case::mp3_distinct_seek_sync_play(source_mp3_distinct().await, SEEK_SYNC_PLAY)]
#[case::mp3_distinct_sync_play_seek(source_mp3_distinct().await, SYNC_PLAY_SEEK)]
#[case::mp3_distinct_sync_seek_play(source_mp3_distinct().await, SYNC_SEEK_PLAY)]
#[case::mp3_distinct_sequential_sync(source_mp3_distinct().await, SEQUENTIAL_SYNC)]
#[case::mp3_distinct_paused_sync_then_play(source_mp3_distinct().await, PAUSED_SYNC)]
#[case::mp3_distinct_four_deck_sequential_sync(source_mp3_distinct().await, FOUR_DECK_SYNC)]
#[case::mp3_distinct_tempo_up_120hz(source_mp3_distinct().await, TEMPO_UP_120)]
#[case::mp3_distinct_tempo_down_30hz(source_mp3_distinct().await, TEMPO_DOWN_30)]
#[case::hls_mp3_play_sync_seek(source_hls_mp3_plain().await, PLAY_SYNC_SEEK)]
#[case::hls_mp3_play_seek_sync(source_hls_mp3_plain().await, PLAY_SEEK_SYNC)]
#[case::hls_mp3_seek_play_sync(source_hls_mp3_plain().await, SEEK_PLAY_SYNC)]
#[case::hls_mp3_seek_sync_play(source_hls_mp3_plain().await, SEEK_SYNC_PLAY)]
#[case::hls_mp3_sync_play_seek(source_hls_mp3_plain().await, SYNC_PLAY_SEEK)]
#[case::hls_mp3_sync_seek_play(source_hls_mp3_plain().await, SYNC_SEEK_PLAY)]
#[case::hls_mp3_sequential_sync(source_hls_mp3_plain().await, SEQUENTIAL_SYNC)]
#[case::hls_mp3_paused_sync_then_play(source_hls_mp3_plain().await, PAUSED_SYNC)]
#[case::hls_mp3_four_deck_sequential_sync(source_hls_mp3_plain().await, FOUR_DECK_SYNC)]
#[case::hls_mp3_tempo_up_120hz(source_hls_mp3_plain().await, TEMPO_UP_120)]
#[case::hls_mp3_tempo_down_30hz(source_hls_mp3_plain().await, TEMPO_DOWN_30)]
#[case::drm_mp3_play_sync_seek(source_hls_mp3_drm().await, PLAY_SYNC_SEEK)]
#[case::drm_mp3_play_seek_sync(source_hls_mp3_drm().await, PLAY_SEEK_SYNC)]
#[case::drm_mp3_seek_play_sync(source_hls_mp3_drm().await, SEEK_PLAY_SYNC)]
#[case::drm_mp3_seek_sync_play(source_hls_mp3_drm().await, SEEK_SYNC_PLAY)]
#[case::drm_mp3_sync_play_seek(source_hls_mp3_drm().await, SYNC_PLAY_SEEK)]
#[case::drm_mp3_sync_seek_play(source_hls_mp3_drm().await, SYNC_SEEK_PLAY)]
#[case::drm_mp3_sequential_sync(source_hls_mp3_drm().await, SEQUENTIAL_SYNC)]
#[case::drm_mp3_paused_sync_then_play(source_hls_mp3_drm().await, PAUSED_SYNC)]
#[case::drm_mp3_four_deck_sequential_sync(source_hls_mp3_drm().await, FOUR_DECK_SYNC)]
#[case::drm_mp3_tempo_up_120hz(source_hls_mp3_drm().await, TEMPO_UP_120)]
#[case::drm_mp3_tempo_down_30hz(source_hls_mp3_drm().await, TEMPO_DOWN_30)]
async fn real_media_product_rows_reach_the_pcm_oracle(
    #[case] provider: PreparedSources,
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
#[ignore = "ignored-red: requires KITHARA_REMOTE_FIXTURES at build time and product Warp alignment, 2026-09-07"]
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
async fn opt_in_library_product_rows_reach_the_pcm_oracle(
    #[case] case: SyncCase,
    #[future(awt)] library_sources: PreparedSources,
) {
    run(case, library_sources).await;
}

#[kithara::fixture]
pub(super) async fn synthetic_sources() -> PreparedSources {
    prepared_sources(Provider::Synthetic).await
}
#[kithara::fixture]
pub(super) async fn sweep_sources() -> PreparedSources {
    prepared_sources(Provider::Sweep).await
}
#[kithara::fixture]
pub(super) async fn mixed_sources() -> PreparedSources {
    prepared_sources(Provider::HlsMp3(HlsProtection::Plain)).await
}
#[kithara::fixture]
pub(super) async fn listening_sources() -> PreparedSources {
    prepared_sources(DOWNTEMPO_HOUSE_PROVIDER).await
}

#[kithara::fixture]
async fn source_mp3_same() -> PreparedSources {
    prepared_sources(Provider::Mp3Same).await
}

#[kithara::fixture]
async fn source_hls_same_drm() -> PreparedSources {
    prepared_sources(Provider::HlsSame(HlsProtection::Drm)).await
}

#[kithara::fixture]
async fn source_synthetic() -> PreparedSources {
    prepared_sources(Provider::Synthetic).await
}

#[kithara::fixture]
async fn source_ambient_trip_hop_provider() -> PreparedSources {
    prepared_sources(AMBIENT_TRIP_HOP_PROVIDER).await
}

#[kithara::fixture]
async fn source_downtempo_house_provider() -> PreparedSources {
    prepared_sources(DOWNTEMPO_HOUSE_PROVIDER).await
}

#[kithara::fixture]
async fn source_scenario_1_downtempo_house_provider() -> PreparedSources {
    prepared_sources(SCENARIO_1_DOWNTEMPO_HOUSE_PROVIDER).await
}

#[kithara::fixture]
async fn source_techno_breakbeat_provider() -> PreparedSources {
    prepared_sources(TECHNO_BREAKBEAT_PROVIDER).await
}

#[kithara::fixture]
async fn source_cross_style_provider() -> PreparedSources {
    prepared_sources(CROSS_STYLE_PROVIDER).await
}

#[kithara::fixture]
async fn source_hls_same_plain() -> PreparedSources {
    prepared_sources(Provider::HlsSame(HlsProtection::Plain)).await
}

#[kithara::fixture]
async fn source_mp3_distinct() -> PreparedSources {
    prepared_sources(Provider::Mp3Distinct).await
}

#[kithara::fixture]
async fn source_hls_mp3_plain() -> PreparedSources {
    prepared_sources(Provider::HlsMp3(HlsProtection::Plain)).await
}

#[kithara::fixture]
async fn source_hls_mp3_drm() -> PreparedSources {
    prepared_sources(Provider::HlsMp3(HlsProtection::Drm)).await
}

#[kithara::fixture]
async fn library_sources() -> PreparedSources {
    prepared_sources(Provider::Library(LIBRARY)).await
}
