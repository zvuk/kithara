#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};
#[cfg(not(target_os = "android"))]
use std::{env, io};

#[cfg(not(target_os = "android"))]
use kithara::{
    analysis::{
        AnalysisFile, AnalysisFingerprint, AnalysisToken, BeatArtifact, BeatSnapshot, BeatState,
        TrackAnalysis,
    },
    assets::{AssetResource, AssetResourceState, AssetSource, AssetStore, ReadSide, ResourceKey},
    encode::EncodeConfig,
    host::Host,
    output::{OfflineRenderRequest, OfflineRenderer},
    platform::CancelScope,
    record::{RecordingConfig, RecordingCore, RecordingSink},
    signal::AudioSpec,
};
use kithara::{
    events::TrackId,
    hls::AbrMode,
    host::{HostConfig, HostOwned},
    platform::{
        sync::Arc,
        time::{self, Duration, Instant},
    },
    play::{
        PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig,
        ResourceSrc, Tempo,
        player::{PlayerControl, PlayerControlSource},
    },
    queue::{CueIn, Queue, QueueConfig, TrackSource, TrackStatus, Transition},
    warp::{
        AlignmentSource, AssetAxis, AssetFrame, Beat, BeatGridId, BeatGridQuery, BeatGridRevision,
        BeatGridSnapshot, BeatGridState, LoadGeneration, MapPoint, MapPosition, Meter,
        PresentationFrontier, SegmentSet, SessionFrame, StretchControls, SyncAdmission, SyncGroup,
        SyncIntent, SyncMode, SyncOperation, SyncStatusSnapshot, TopologyStamp, TransportRevision,
        WarpConfig, WarpMapRevision,
    },
};
#[cfg(not(target_os = "android"))]
use kithara_app::recording::AssetPartSink;
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    audio_artifact::{AudioArtifactTap, artifact_label},
    bufpool_ext::{TestPools, pools},
    cochlea::{
        CochleaReport, host_beat_alignment_failures, marked_rhythm_markers,
        marked_synchronization_failures, synchronization_failures,
    },
    fixture_protocol::EncryptionRequest,
    hls_fixture::{aes128_iv, aes128_key_bytes},
    kithara, memory_asset_store,
    offline::OfflineHostHarness,
    underrun_ledger::UnderrunLedger,
    usdt_trace,
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
    Wobble,
}

impl TempoRide {
    const fn points(self) -> &'static [f64] {
        match self {
            Self::Down => &[116.0, 112.0, 108.0],
            Self::Hold(_) => &[],
            Self::Triangle => &[116.0, 112.0, 116.0, 120.0],
            Self::Up => &[122.0, 125.0, 127.0],
            Self::Wobble => &[124.0, 116.0, 124.0, 116.0, 124.0, 116.0, 120.0],
        }
    }

    const fn start_bpm(self) -> f64 {
        match self {
            Self::Hold(bpm) => bpm,
            Self::Down | Self::Triangle | Self::Up | Self::Wobble => START_BPM,
        }
    }

    const fn final_bpm(self) -> f64 {
        match self {
            Self::Down => 108.0,
            Self::Hold(bpm) => bpm,
            Self::Triangle | Self::Wobble => 120.0,
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
    /// The output rate the host runs at before it restarts mid-ride onto
    /// `sample_rate`, or `None` when the host never changes rate.
    start_sample_rate: Option<u32>,
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
            start_sample_rate: None,
        }
    }

    /// Starts the host at `start` and restarts it onto `sample_rate` midway
    /// through the tempo ride, the way a device route change moves the rate
    /// under a running session.
    const fn restarting_host(self, start: u32) -> Self {
        Self {
            start_sample_rate: Some(start),
            ..self
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

    pub(super) const fn paused(mut self) -> Self {
        self.paused = true;
        self
    }

    const fn ride(mut self, ride: TempoRide, updates_hz: u32) -> Self {
        self.ride = ride;
        self.updates_hz = updates_hz;
        self
    }

    pub(super) const fn hold(mut self, bpm: f64) -> Self {
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

    #[cfg(not(target_os = "android"))]
    pub(super) const fn decks(self) -> usize {
        self.decks
    }

    #[cfg(not(target_os = "android"))]
    pub(super) const fn id(self) -> &'static str {
        self.id
    }

    delegate::delegate! {
        to self.ride {
            const fn start_bpm(self) -> f64;
            pub(super) const fn final_bpm(self) -> f64;
        }
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
/// A tempo knob turned back and forth: the Host tempo moves every output block.
const TEMPO_WOBBLE_EVERY_BLOCK: SyncCase = SyncCase::running(
    "tempo-wobble-every-block",
    2,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.ride(TempoRide::Wobble, 375);
/// The host starts at 44.1k and restarts onto 48k mid-ride, so the region
/// plan and every frontier must follow the axis the decoder now emits.
const HOST_RATE_CHANGE: SyncCase =
    SyncCase::running("host-rate-change", 2, 48_000, OperationOrder::PlaySyncSeek)
        .ride(TempoRide::Up, 120)
        .restarting_host(44_100);
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
pub(super) const HOUSE_PAIR_SYNC: SyncCase = SyncCase::running(
    "house-124-staggered",
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
const SCENARIO_3_HOUSE_PAIR: &[&str] = &[
    "rhythm_wav_scenario_1_house_124_left_only",
    "rhythm_wav_scenario_1_house_124_right_only",
];
const SCENARIO_4_HOUSE_PAIR_PICKUP: &[&str] = &[
    "rhythm_wav_scenario_1_house_124_left_only",
    "rhythm_wav_scenario_2_house_124_right_only_pickup",
];
const SCENARIO_2_DOWNTEMPO_HOUSE_PICKUP: &[&str] = &[
    "rhythm_wav_scenario_1_downtempo_96_left_only",
    "rhythm_wav_scenario_2_house_124_right_only_pickup",
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
/// Richie Hawtin - The Tunnel, the straight-kick track the application is tried on.
pub(super) const PLAYLIST_TUNNEL: &[&str] = &["library_mp3_zvuk_27390231"];
pub(super) const PLAYLIST_151585912: &[&str] = &["library_mp3_zvuk_151585912"];
pub(super) const PLAYLIST_125475417: &[&str] = &["library_mp3_zvuk_125475417"];
pub(super) const PLAYLIST_138535169: &[&str] = &["library_mp3_zvuk_138535169"];
pub(super) const PLAYLIST_130432502: &[&str] = &["library_mp3_zvuk_130432502"];
pub(super) const PLAYLIST_132017169: &[&str] = &["library_mp3_zvuk_132017169"];
const PLAYLIST: &[&str] = &[
    "library_mp3_zvuk_27390231",
    "library_mp3_zvuk_151585912",
    "library_mp3_zvuk_125475417",
    "library_mp3_zvuk_138535169",
    "library_mp3_zvuk_130432502",
    "library_mp3_zvuk_132017169",
];

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
const SCENARIO_3_HOUSE_PAIR_PROVIDER: Provider = Provider::Rhythm(SCENARIO_3_HOUSE_PAIR);
const SCENARIO_4_HOUSE_PAIR_PICKUP_PROVIDER: Provider =
    Provider::Rhythm(SCENARIO_4_HOUSE_PAIR_PICKUP);
const SCENARIO_2_DOWNTEMPO_HOUSE_PICKUP_PROVIDER: Provider =
    Provider::Rhythm(SCENARIO_2_DOWNTEMPO_HOUSE_PICKUP);
pub(super) const TECHNO_BREAKBEAT_PROVIDER: Provider = Provider::Rhythm(TECHNO_BREAKBEAT);
pub(super) const CROSS_STYLE_PROVIDER: Provider = Provider::Rhythm(CROSS_STYLE);

impl Provider {
    pub(super) const ALL: &[Provider] = &[
        Self::Synthetic,
        Self::Rhythm(CROSS_STYLE),
        Self::HlsSame(HlsProtection::Plain),
        Self::HlsSame(HlsProtection::Drm),
        Self::Library(LIBRARY),
        Self::Library(PLAYLIST),
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
    TrackAnalysis::builder()
        .token(AnalysisToken::from("uniform-fixture"))
        .source_sample_rate(NonZeroU32::new(SAMPLE_RATE).expect("fixture sample rate"))
        .beat(BeatSnapshot::new(artifact, BeatState::Final, Vec::new()))
        .extent(SAMPLE_RATE as u64 * SECONDS)
        .revision(0)
        .build()
        .beat_grid()
        .expect("the fixture analysis carries a beat pass")
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
    /// Host session frame minus captured frame; moves when a stream restart
    /// rescales the Host clock to the new rate.
    session_offset: i64,
    host_grid: Option<BeatGridSnapshot>,
    sync_requested: bool,
    sync_activation: Option<u64>,
    tap: AudioArtifactTap,
    /// Holds the probe recording open for the whole case: the wire keeps one
    /// recording per process, and every reader here reads that one. Declared
    /// after every reader so its drop cannot clear the recording first.
    _trace: usdt_trace::Scope,
    server: Option<TestServerHelper>,
    paced: bool,
}

#[cfg(not(target_os = "android"))]
struct FailingPartSink(AssetPartSink<TestPools>);

#[cfg(not(target_os = "android"))]
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

#[cfg(not(target_os = "android"))]
struct OfflineRecordingArtifact;

#[cfg(not(target_os = "android"))]
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

#[cfg(not(target_os = "android"))]
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

#[cfg(not(target_os = "android"))]
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
        let ledger = self.tap.underrun_ledger();
        let mut failures: Vec<String> = self
            .player_controls
            .iter()
            .enumerate()
            .filter_map(|(deck, control)| {
                let count = control.rt_metrics()?.underruns();
                (count > 0).then(|| {
                    let site = self.underrun_site(&ledger, deck);
                    format!("deck {deck} rendered with {count} PCM underruns{site}")
                })
            })
            .collect();
        if ledger.unparsed > 0 {
            let unparsed = ledger.unparsed;
            failures.push(format!(
                "{unparsed} PCM underrun probes carried no output interval"
            ));
        }
        failures
    }

    /// Name the output frames each of this deck's tracks lost, from the capture's ledger.
    fn underrun_site(&self, ledger: &UnderrunLedger, deck: usize) -> String {
        let tracks = ledger.tracks();
        self.ids
            .get(deck)
            .into_iter()
            .flatten()
            .filter_map(|id| {
                let track = tracks
                    .iter()
                    .find(|track| track.track_id == Some(id.as_u64()))?;
                Some(format!(
                    "; track {}: {} frames silenced in output {}..{}",
                    id.as_u64(),
                    track.silenced_frames,
                    track.first_output_frame,
                    track.last_output_frame,
                ))
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
        let sample_rate = NonZeroU32::new(case.start_sample_rate.unwrap_or(case.sample_rate))
            .expect("fixture sample rate");
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
        let trace = usdt_trace::scope();
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
            session_offset: 0,
            host_grid: None,
            sync_requested: false,
            sync_activation: None,
            tap: AudioArtifactTap::from_env_or(
                std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
                &format!("{}-{}", artifact_label(), case.id),
                case.sample_rate,
                CHANNELS,
            )
            .expect("listening tap"),
            server: None,
            paced,
            _trace: trace,
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

    #[kithara::flash(true)]
    async fn wait_loaded(&mut self, case: SyncCase) {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        loop {
            self.tick_all(case).await;
            let mut loaded = true;
            for (index, deck) in self.decks.iter().enumerate() {
                for id in &self.ids[index] {
                    match deck.track(*id).map(|track| track.status) {
                        Some(TrackStatus::Loaded | TrackStatus::Consumed) => {}
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

    #[kithara::flash(true)]
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
        if self.host_grid.is_some() {
            self.refresh_host_grid().await;
        }
        self.record_host_grid(start, end);
        self.tap.timeline().span(
            "host-output",
            start,
            end,
            "presented",
            "offline Host render",
            None,
        );
        self.tap.push(&samples);
        let delay = if self.paced {
            Duration::from_secs_f64(frames as f64 / f64::from(case.sample_rate))
                .saturating_sub(started.elapsed())
        } else {
            Duration::from_millis(1)
        };
        time::sleep(delay).await;
        samples
    }

    /// Host session frame at a captured frame.
    pub(super) fn session_frame(&self, capture: u64) -> i64 {
        i64::try_from(capture)
            .unwrap_or(i64::MAX)
            .saturating_add(self.session_offset)
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
        let (BeatGridQuery::Resolved(first), BeatGridQuery::Resolved(last)) =
            (at(self.session_frame(start)), at(self.session_frame(end)))
        else {
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
            let Ok(frame) = u64::try_from(i64::from(frame) - self.session_offset) else {
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
            self.tap.host_beat(frame, kind == "downbeat");
            self.tap.timeline().point(
                "host-grid",
                frame,
                "grid",
                &format!("Host {kind} {ordinal}"),
            );
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
        self.render_through(case, activation).await;
    }

    /// Render callback blocks until the harness timeline passes `activation`.
    #[kithara_test_utils::kithara::hang_watchdog]
    pub(super) async fn render_through(&mut self, case: SyncCase, activation: u64) {
        while self.session_frame(self.rendered_frames)
            <= i64::try_from(activation).unwrap_or(i64::MAX)
        {
            let _ = self.render(case, self.block_frames).await;
            hang_reset!();
        }
    }

    async fn render_until_free_off(
        &mut self,
        case: SyncCase,
        operation: kithara::warp::SyncOperationId,
        topology: TopologyStamp,
        warp_map: WarpMapRevision,
    ) -> SyncStatusSnapshot {
        let mut activation = None;
        for _ in 0..16 {
            let deck = self.decks[0].id();
            let status = self
                .host
                .with(move |host| host.sync_status(deck))
                .await
                .expect("canonical deck synchronization status");
            match status {
                SyncStatusSnapshot::Preparing {
                    operation: current,
                    topology: current_topology,
                    warp_map: current_map,
                } => {
                    assert_eq!(
                        (current, current_topology, current_map),
                        (operation, topology, warp_map),
                        "Free may only retain its admitted Preparing identity"
                    );
                    let _ = self.render(case, self.block_frames).await;
                }
                SyncStatusSnapshot::Prepared {
                    operation: current,
                    topology: current_topology,
                    warp_map: current_map,
                    activation: prepared_activation,
                } => {
                    assert_eq!(
                        (current, current_topology, current_map),
                        (operation, topology, warp_map)
                    );
                    activation = Some(
                        u64::try_from(i64::from(prepared_activation))
                            .expect("non-negative activation"),
                    );
                    break;
                }
                other @ SyncStatusSnapshot::Off { .. } => {
                    let _ = self.render(case, self.block_frames).await;
                    return other;
                }
                other => panic!("unexpected Free status before worker preparation: {other:?}"),
            }
        }
        let activation =
            activation.expect("Free did not prepare within the bounded worker handoff window");
        self.sync_activation = Some(activation);
        self.settle_sync_activation(case).await;
        let _ = self.render(case, self.block_frames).await;

        let deck = self.decks[0].id();
        let status = self
            .host
            .with(move |host| host.sync_status(deck))
            .await
            .expect("canonical deck synchronization status after activation");
        if matches!(&status, SyncStatusSnapshot::Off { .. }) {
            // Grid publication acknowledges PCM after the callback has sampled
            // its render context. The next callback is the first one that can
            // observe the published Off context.
            let _ = self.render(case, self.block_frames).await;
        }
        status
    }

    pub(super) async fn play_all(&self) {
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

    #[kithara_test_utils::kithara::hang_watchdog]
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
                    hang_reset!();
                }
            }
        }
        self.settle(case, 2).await;
    }

    pub(super) async fn seek_staggered(&mut self, case: SyncCase) {
        self.mark("seek");
        let stagger_seconds = 3.0 / 8.0 * 60.0 / case.start_bpm();
        for index in 0..self.decks.len() {
            let deck = self.decks.remove(index);
            let seconds = case.start_seconds.map_or_else(
                || 5.25 + index as f64 * stagger_seconds,
                |starts| {
                    *starts
                        .get(index)
                        .unwrap_or_else(|| panic!("{}: missing deck {index} start", case.id))
                },
            );
            let source = (seconds * f64::from(case.sample_rate)).round() as u64;
            self.tap.timeline().span(
                &format!("deck-{index}"),
                self.rendered_frames,
                self.rendered_frames,
                "command",
                &format!("seek request {seconds:.6}s"),
                Some((source, source)),
            );
            let (deck, result) = self
                .host
                .with(move |host| {
                    let result = host.seek_deck(&deck, seconds);
                    (deck, result)
                })
                .await;
            self.decks.insert(index, deck);
            result.unwrap_or_else(|error| panic!("{}: seek deck {index}: {error}", case.id));
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

    async fn refresh_host_grid(&mut self) {
        self.host_grid = Some(self.host.session_grid().await);
    }

    pub(super) async fn request_sync(&mut self, case: SyncCase) {
        let _ = self.request_sync_intent(case, SyncIntent::Enable).await;
        self.sync_requested = true;
    }

    pub(super) async fn request_sync_intent(
        &mut self,
        case: SyncCase,
        intent: SyncIntent,
    ) -> Vec<SyncAdmission> {
        self.mark(&format!("request_sync-{intent:?}"));
        self.refresh_host_grid().await;
        let mut admissions = Vec::with_capacity(self.decks.len());
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
                            .output(SessionFrame::new(self.session_frame(self.rendered_frames)))
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
                            .output(SessionFrame::new(self.session_frame(self.rendered_frames)))
                            .build(),
                    )
                };
                let target = deck.id();
                let activation = SessionFrame::new(self.session_frame(self.rendered_frames));
                let admission = self
                    .host
                    .with(move |host| {
                        host.transact(SyncOperation::Sync {
                            target,
                            load: LoadGeneration::first(),
                            transport: TransportRevision::first(),
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
                    self.tap.timeline().point(
                        &format!("deck-{index}"),
                        activation,
                        "planned",
                        "sync activation",
                    );
                    self.sync_activation = Some(
                        self.sync_activation
                            .map_or(activation, |current| current.max(activation)),
                    );
                }
                admissions.push(admission);
            }
            if matches!(case.order, OperationOrder::SequentialSync) {
                let _ = self.render(case, self.block_frames).await;
            }
        }
        admissions
    }

    pub(super) async fn publish_track_grid(
        &mut self,
        deck: usize,
        item: TrackId,
        segments: SegmentSet,
        state: BeatGridState,
    ) -> Result<SyncAdmission, PlayError> {
        let diagnostic = BeatGridSnapshot::segments(
            BeatGridId::allocate().expect("diagnostic grid identity"),
            BeatGridRevision::first(),
            state,
            segments.clone(),
        )
        .expect("published track grid is valid for artifact diagnostics");
        self.tap.source_grid(item.as_u64(), diagnostic);
        let target = self.decks[deck].id();
        self.host
            .with(move |host| host.publish_track_grid(target, item, segments, state))
            .await
    }

    delegate::delegate! {
        to self.tap {
            /// Host beats recorded inside `frames` of the capture axis.
            pub(super) fn host_beats_in(&self, frames: std::ops::Range<u64>) -> Vec<u64>;
            pub(super) fn mark(&mut self, label: &str);
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
        let mut host_rate = case.start_sample_rate.unwrap_or(case.sample_rate);
        let restart_after = case.start_sample_rate.map(|_| case.ride.points().len() / 2);
        for (leg, &target) in case.ride.points().iter().enumerate() {
            for step in 1..=steps_per_leg {
                let fraction = f64::from(step) / f64::from(steps_per_leg);
                let bpm = start + (target - start) * fraction;
                self.set_tempo(case, bpm, false).await;
                update += 1;
                let deadline = update * u64::from(host_rate) / u64::from(case.updates_hz);
                let frames = deadline.saturating_sub(rendered);
                rendered = deadline;
                if frames > 0 {
                    let frames = usize::try_from(frames).expect("tempo interval fits usize");
                    let _ = self
                        .capture_blocks(case, frames, self.block_frames, self.paced)
                        .await;
                }
            }
            start = target;
            if restart_after == Some(leg) {
                self.restart_host_rate(case).await;
                host_rate = case.sample_rate;
                rendered = 0;
                update = 0;
            }
        }
        self.settle(case, 4).await;
    }

    /// Moves the live output rate the way a device route change does, so the
    /// deck must answer on the axis the decoded stream now carries.
    async fn restart_host_rate(&mut self, case: SyncCase) {
        let old_rate = f64::from(case.start_sample_rate.unwrap_or(case.sample_rate));
        let seconds = self.session_frame(self.rendered_frames) as f64 / old_rate;
        let new_rate = f64::from(case.sample_rate);
        let session = (seconds.floor() * new_rate + (seconds.fract() * new_rate).round()) as i64;
        self.session_offset = session - i64::try_from(self.rendered_frames).unwrap_or(i64::MAX);
        self.tap.timeline().point(
            "host",
            self.rendered_frames,
            "command",
            &format!(
                "restart output rate, session offset {}",
                self.session_offset
            ),
        );
        let rate = NonZeroU32::new(case.sample_rate).expect("case sample rate must be non-zero");
        if let Err(error) = self.host.set_sample_rate(rate).await {
            self.failures
                .push(format!("{}: restart host output rate: {error}", case.id));
        }
        self.settle(case, 4).await;
        self.refresh_host_grid().await;
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

    #[kithara::flash(true)]
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

#[cfg(not(target_os = "android"))]
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
                    panic!("BLOCKED_FIXTURE: library fixture `{name}` is not registered")
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
        .or_else(|| {
            source
                .strip_prefix("library_mp3_")
                .map(|case| format!("library_mp3_analysis_{case}"))
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
    assert!(
        analysis.extent().is_some(),
        "`{analysis_name}` states the source extent its grid is laid on"
    );
    analysis
        .beat_grid()
        .unwrap_or_else(|| panic!("`{analysis_name}` has no beat analysis"))
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

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
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
    let probe_events = usdt_trace::events()
        .into_iter()
        .filter(|event| {
            matches!(
                event.probe,
                "warp_plan_published"
                    | "prepared_launch_command_admitted"
                    | "prepared_launch_seek_begun"
                    | "prepared_launch_readiness_checked"
                    | "prepared_render_revision_selected"
                    | "decoder_seek_epoch_observed"
                    | "chunk_admitted"
                    | "scheduled_seek_activated"
                    | "pcm_consumed"
                    | "pcm_underrun"
            )
        })
        .map(|event| {
            serde_json::json!({
                "probe": event.probe,
                "thread": format!("{:?}", event.thread),
                "fields": event.fields().collect::<std::collections::BTreeMap<_, _>>(),
            })
        })
        .collect::<Vec<_>>();
    harness.tap.evidence(
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
        }),
    );
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
            event["probe"] == "chunk_admitted"
                && event["fields"]["epoch"].as_u64() == Some(launch_epoch)
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

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
#[case::house_124(TrackStartPickup { id: "track-start-pickup-124", provider: Provider::Rhythm(PICKUP_HOUSE_124), source_downbeat_frame: 23_226, seek_seconds: None, expected_source_frame: 0, expected_host_onset: 69_677, expected_next_host_marker: 92_903 })]
#[case::downtempo_96(TrackStartPickup { id: "track-start-pickup-96", provider: Provider::Rhythm(PICKUP_DOWNTEMPO_96), source_downbeat_frame: 30_000, seek_seconds: None, expected_source_frame: 0, expected_host_onset: 69_677, expected_next_host_marker: 92_903 })]
#[case::house_124_seek_zero(TrackStartPickup { id: "track-start-pickup-124-seek-zero", provider: Provider::Rhythm(PICKUP_HOUSE_124), source_downbeat_frame: 23_226, seek_seconds: Some(0.0), expected_source_frame: 23_226, expected_host_onset: 92_903, expected_next_host_marker: 116_129 })]
#[case::downtempo_96_seek_zero(TrackStartPickup { id: "track-start-pickup-96-seek-zero", provider: Provider::Rhythm(PICKUP_DOWNTEMPO_96), source_downbeat_frame: 30_000, seek_seconds: Some(0.0), expected_source_frame: 30_000, expected_host_onset: 92_903, expected_next_host_marker: 116_129 })]
#[case::house_124_seek_ten(TrackStartPickup { id: "track-start-pickup-124-seek-ten", provider: Provider::Rhythm(PICKUP_HOUSE_124), source_downbeat_frame: 23_226, seek_seconds: Some(10.0), expected_source_frame: 487_746, expected_host_onset: 92_903, expected_next_host_marker: 116_129 })]
#[case::downtempo_96_seek_ten(TrackStartPickup { id: "track-start-pickup-96-seek-ten", provider: Provider::Rhythm(PICKUP_DOWNTEMPO_96), source_downbeat_frame: 30_000, seek_seconds: Some(10.0), expected_source_frame: 510_000, expected_host_onset: 92_903, expected_next_host_marker: 116_129 })]
async fn track_start_pickup_reaches_real_pcm_at_its_host_phase(#[case] pickup: TrackStartPickup) {
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
    let events = usdt_trace::events();
    let pcm_flow: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event.probe,
                "chunk_admitted" | "pcm_reader_admitted" | "pcm_consumed" | "pcm_underrun"
            )
        })
        .map(|event| {
            serde_json::json!({
                "probe": event.probe,
                "thread": format!("{:?}", event.thread),
                "output_start": event.field("output_start"),
                "frames": event.field("frames"),
                "requested": event.field("requested_frames"),
                "available": event.field("available_frames"),
                "source_start": event.field("source_start"),
            })
        })
        .collect();
    harness
        .tap
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
                "pcm_flow": pcm_flow,
            }),
        );
    drop(harness);
    assert_eq!(first, Some(pickup.expected_host_onset));
    assert!(markers.contains(&pickup.expected_next_host_marker));
    assert!(underruns.is_empty());
    assert!(equal_rate_waveform.unwrap_or(true));
    let plan = events
        .iter()
        .find(|event| event.probe == "warp_plan_published")
        .expect("prepared launch publishes a warp plan");
    assert_eq!(
        plan.field("activation_source")
            .expect("the plan names its activation source"),
        pickup.expected_source_frame
    );
    assert_eq!(
        plan.field("activation_output"),
        Some(pickup.expected_host_onset)
    );
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn host_seek_publishes_one_post_command_plan_for_its_decoder_destination() {
    let case = SyncCase::running(
        "host-seek-plan-destination",
        1,
        48_000,
        OperationOrder::PlaySyncSeek,
    )
    .paused()
    .hold(124.0);
    let sources = prepared_sources(Provider::Synthetic).await;
    let mut harness = ProductHarness::new(case, &sources, 0).await;
    prepare_fixture_grids(&mut harness, case, &sources).await;
    harness.request_sync(case).await;
    let _ = harness.render(case, harness.block_frames * 2).await;
    let baseline = usdt_trace::events();
    let baseline_map = baseline
        .iter()
        .filter(|event| event.probe == "warp_plan_published")
        .filter_map(|event| event.field("warp_map_revision"))
        .max();
    let deck = harness.decks.remove(0);
    let (deck, outcome) = harness
        .host
        .with(move |host| {
            let outcome = host.seek_deck(&deck, 5.25);
            (deck, outcome)
        })
        .await;
    harness.decks.insert(0, deck);
    let outcome = outcome.expect("Host seek is admitted");
    let destination = match outcome {
        kithara::play::SeekOutcome::Landed { landed_at, .. } => landed_at,
        other => panic!("fixture seek must land: {other:?}"),
    };
    let destination_source =
        (destination.as_secs_f64() * f64::from(case.sample_rate)).round() as u64;
    let post = &usdt_trace::events()[baseline.len()..];
    let plan = post
        .iter()
        .find(|event| event.probe == "warp_plan_published")
        .expect("accepted Host seek publishes its plan before the next tick");
    assert_eq!(plan.field("activation_source"), Some(destination_source));
    assert!(
        baseline_map.is_none_or(|revision| plan
            .field("warp_map_revision")
            .is_some_and(|next| next > revision)),
        "Host seek must allocate a new map revision"
    );
    let activation = plan
        .field("activation_output")
        .expect("Host seek plan has an exact activation");
    harness.play_all().await;
    harness.render_through(case, activation).await;
    let events = usdt_trace::events();
    let adopted = events
        .iter()
        .skip(baseline.len())
        .find(|event| {
            event.probe == "decoder_seek_epoch_observed" && event.field("adopted") == Some(1)
        })
        .expect("Host seek decoder epoch is adopted");
    let epoch = adopted.field("current_epoch").expect("adopted epoch");
    assert!(events.iter().skip(baseline.len()).any(|event| {
        event.probe == "chunk_admitted"
            && event.field("epoch") == Some(epoch)
            && event.field("source_start") == Some(destination_source)
    }));
}

#[derive(Clone, Copy, Debug)]
enum ToggleStart {
    Off,
    Local,
    Host,
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
#[case::off_to_local(ToggleStart::Off, SyncIntent::Disable, SyncMode::LocalSync, 1.0)]
#[case::off_to_host(ToggleStart::Off, SyncIntent::Enable, SyncMode::HostSync, 1.0)]
#[case::local_to_off(ToggleStart::Local, SyncIntent::Free, SyncMode::Off, 0.75)]
#[case::local_to_host(ToggleStart::Local, SyncIntent::Enable, SyncMode::HostSync, 1.0)]
#[case::host_to_off(ToggleStart::Host, SyncIntent::Free, SyncMode::Off, 0.75)]
#[case::host_to_local(ToggleStart::Host, SyncIntent::Disable, SyncMode::LocalSync, 1.0)]
async fn directed_sync_toggle_matrix(
    #[case] start: ToggleStart,
    #[case] command: SyncIntent,
    #[case] expected_mode: SyncMode,
    #[case] expected_rate: f32,
) {
    let case = SyncCase::running(
        "directed-sync-toggle",
        1,
        48_000,
        OperationOrder::PlaySyncSeek,
    )
    .paused()
    .hold(124.0);
    let sources = prepared_sources(Provider::Rhythm(PICKUP_HOUSE_124)).await;
    let mut harness = ProductHarness::new(case, &sources, 0).await;
    prepare_fixture_grids(&mut harness, case, &sources).await;
    harness.decks[0].set_default_rate(0.75);
    harness.play_all().await;
    match start {
        ToggleStart::Off => {}
        ToggleStart::Local => {
            let initial = harness.request_sync_intent(case, SyncIntent::Disable).await;
            assert!(matches!(
                initial.as_slice(),
                [SyncAdmission::Prepared { .. }]
            ));
            harness.settle_sync_activation(case).await;
        }
        ToggleStart::Host => {
            harness.request_sync(case).await;
            harness.settle_sync_activation(case).await;
        }
    }
    let _ = harness
        .capture_frames(case, harness.block_frames * 4, harness.block_frames)
        .await;
    let baseline = usdt_trace::events();
    let baseline_map = baseline
        .iter()
        .filter(|event| event.probe == "warp_plan_published")
        .filter_map(|event| event.field("warp_map_revision"))
        .max();
    let baseline_underruns = harness.player_controls[0]
        .rt_metrics()
        .map_or(0, |metrics| metrics.underruns());

    let admission = harness
        .request_sync_intent(case, command)
        .await
        .pop()
        .expect("one-deck toggle returns one typed admission");
    let post = &usdt_trace::events()[baseline.len()..];
    match expected_mode {
        SyncMode::Off => {
            let SyncAdmission::Preparing {
                operation,
                topology,
                warp_map,
            } = admission
            else {
                panic!("{start:?} -> Off must reserve a worker-owned Free handoff: {admission:?}");
            };
            assert!(
                !post.iter().any(|event| matches!(
                    event.probe,
                    "prepared_sync_acknowledged" | "deck_grid_off_published"
                )),
                "Free cannot acknowledge or publish Off before worker adoption"
            );
            let status = harness
                .render_until_free_off(case, operation, topology, warp_map)
                .await;
            let events = usdt_trace::events();
            let post = &events[baseline.len()..];
            assert!(
                matches!(status, SyncStatusSnapshot::Off { .. }),
                "Free must publish Off after its prepared activation: {status:?}; retained events: {post:#?}"
            );
            let installed_index = post
                .iter()
                .position(|event| event.probe == "free_adoption_installed")
                .expect("worker commits the reserved Free request");
            let installed = &post[installed_index];
            assert_eq!(installed.field("operation"), Some(u64::from(operation)));
            assert_eq!(installed.field("warp_map"), Some(u64::from(warp_map)));
            let activation = post
                .iter()
                .find(|event| {
                    event.probe == "free_adoption_activation"
                        && event.field("operation") == Some(u64::from(operation))
                        && event.field("warp_map") == Some(u64::from(warp_map))
                })
                .expect("installed request publishes its exact worker geometry");
            assert!(activation.field("source").is_some());
            assert!(activation.field("output").is_some());
            let ack_index = post
                .iter()
                .position(|event| {
                    event.probe == "prepared_sync_acknowledged"
                        && event.field("free") == Some(1)
                        && event.field("warp_map_revision") == Some(u64::from(warp_map))
                })
                .expect("the adopted Free map is acknowledged");
            let off_index = post
                .iter()
                .position(|event| {
                    event.probe == "deck_grid_off_published"
                        && event.field("warp_map_revision") == Some(u64::from(warp_map))
                })
                .expect("the adopted Free map publishes Off");
            let target_pcm_index = post
                .iter()
                .enumerate()
                .skip(installed_index + 1)
                .find(|(_, event)| {
                    event.probe == "pcm_consumed"
                        && event
                            .field("render_revision")
                            .is_some_and(|revision| revision >> u32::BITS == u64::from(warp_map))
                })
                .map(|(index, _)| index)
                .expect("the installed Free map reaches presented PCM");
            assert!(
                installed_index < target_pcm_index
                    && target_pcm_index < ack_index
                    && ack_index < off_index,
                "Free event order must remain installed < target pcm_consumed < prepared_sync_acknowledged < deck_grid_off_published"
            );
            if matches!(start, ToggleStart::Host) {
                let pre_publication = post[..off_index]
                    .iter()
                    .rev()
                    .find(|event| event.probe == "deck_render_context")
                    .expect("the acknowledgement callback sampled its pre-publication context");
                assert_eq!(
                    pre_publication.field("mode"),
                    Some(2),
                    "the acknowledgement callback may still carry HostSync before Off publishes"
                );
            }
            assert!(baseline_map.is_none_or(|previous| u64::from(warp_map) > previous));
            assert!(
                u64::from(topology.revision()) > 0,
                "Preparing carries a live topology identity"
            );
            post.iter()
                .skip(off_index)
                .find(|event| {
                    event.probe == "deck_render_context"
                        && event.field("mode") == Some(0)
                        && event.field("rate_bits") == Some(u64::from(expected_rate.to_bits()))
                })
                .expect("Free publishes the exact manual Off render context");
            let first_rate_index = post
                .iter()
                .position(|event| {
                    event.probe == "rate_applied"
                        && event.field("applied_rate_bits")
                            == Some(u64::from(expected_rate.to_bits()))
                })
                .expect("the published Off rate reaches the renderer");
            let first_rate = &post[first_rate_index];
            let rate_revision = first_rate
                .field("request_revision")
                .expect("rate application carries its revision");
            let normal_rate = post
                .iter()
                .skip(first_rate_index + 1)
                .find(|event| {
                    event.probe == "rate_applied"
                        && event.field("applied_rate_bits")
                            == Some(u64::from(expected_rate.to_bits()))
                        && event.field("request_revision") == Some(rate_revision)
                })
                .expect("Free keeps the exact Off rate and revision after its carrier quantum");
            let normal_source = normal_rate
                .field("source_start")
                .expect("normal Off rate application carries its source boundary");
            assert!(
                !post
                    .iter()
                    .any(|event| event.probe == "decoder_seek_epoch_observed"),
                "Free retains the resident decoder without a seek"
            );
            let events = usdt_trace::events();
            let (reader_index, reader_source) = events
                .iter()
                .enumerate()
                .skip(baseline.len())
                .find_map(|(index, event)| {
                    if event.probe == "pcm_reader_admitted"
                        && event
                            .field("render_revision")
                            .is_some_and(|revision| revision >> u32::BITS == u64::from(warp_map))
                        && event
                            .field("source_start")
                            .is_some_and(|source| source >= normal_source)
                    {
                        event.field("source_start").map(|source| (index, source))
                    } else {
                        None
                    }
                })
                .expect("Free admits matching post-Off map PCM at the manual source boundary");
            assert_eq!(
                harness.block_frames, BLOCK_FRAMES,
                "the Free response contract is defined for 128-frame callbacks"
            );
            let target_frames = BLOCK_FRAMES;
            let callbacks = (target_frames - 1 + target_frames).div_ceil(target_frames);
            assert_eq!(callbacks, 2, "127 + 128 frames span exactly two callbacks");
            let mut event_cursor = reader_index.saturating_add(1);
            let mut run = None;
            let mut target_consumed_index = None;
            for _ in 0..callbacks {
                let current = usdt_trace::events();
                for (index, event) in current.iter().enumerate().skip(event_cursor) {
                    if event.probe != "pcm_consumed"
                        || event
                            .field("render_revision")
                            .is_none_or(|revision| revision >> u32::BITS != u64::from(warp_map))
                    {
                        continue;
                    }
                    let Some((source_start, source_end, output_start, output_end)) = event
                        .field("source_start")
                        .zip(event.field("source_end"))
                        .zip(event.field("output_start"))
                        .zip(event.field("output_end"))
                        .map(|(((source_start, source_end), output_start), output_end)| {
                            (source_start, source_end, output_start, output_end)
                        })
                    else {
                        continue;
                    };
                    if source_start < reader_source
                        || source_end <= source_start
                        || output_end <= output_start
                    {
                        continue;
                    }
                    match run {
                        Some((first_source, previous_source, first_output, previous_output))
                            if source_start == previous_source
                                && output_start == previous_output =>
                        {
                            run = Some((first_source, source_end, first_output, output_end));
                        }
                        _ => run = Some((source_start, source_end, output_start, output_end)),
                    }
                    let Some((_, _, first_output, last_output)) = run else {
                        continue;
                    };
                    let output_frames = last_output - first_output;
                    if output_frames < u64::try_from(target_frames).expect("block fits u64") {
                        continue;
                    }
                    target_consumed_index = Some(index);
                    break;
                }
                event_cursor = current.len();
                if target_consumed_index.is_some() {
                    break;
                }
                let _ = harness.render(case, harness.block_frames).await;
            }
            let _target_consumed_index = target_consumed_index.expect(
                "Free must consume 128 target map PCM frames within two callbacks including admission",
            );
            let commits = usdt_trace::events()
                .iter()
                .skip(reader_index)
                .filter(|event| event.probe == "render_committed")
                .filter_map(|event| {
                    Some((
                        event.field("output_start")?,
                        event.field("source_start")?,
                        event.field("source_end")?,
                    ))
                })
                .filter(|(_, source_start, _)| *source_start >= reader_source)
                .collect::<Vec<_>>();
            let target_output = u64::try_from(target_frames).expect("block fits u64");
            let (source_frames, output_frames) = commits
                .windows(2)
                .scan((0_u64, 0_u64), |(source, output), pair| {
                    let ((output_start, source_start, source_end), (next_output, next_source, _)) =
                        (pair[0], pair[1]);
                    (next_source == source_end).then(|| {
                        *source += source_end - source_start;
                        *output += next_output - output_start;
                        (*source, *output)
                    })
                })
                .find(|(_, output)| *output >= target_output)
                .expect("Free commits contiguous renders covering one renderer quantum");
            assert!(
                ((source_frames as f64) - f64::from(expected_rate) * output_frames as f64).abs()
                    < f64::from(expected_rate),
                "Free preserves the manual rate across whole committed renders: source={source_frames}, output={output_frames}, rate={expected_rate}"
            );
        }
        SyncMode::HostSync | SyncMode::LocalSync => {
            let SyncAdmission::Prepared { warp_map, .. } = admission else {
                panic!(
                    "{start:?} -> {expected_mode:?} must prepare a typed warp map: {admission:?}"
                );
            };
            let plans: Vec<_> = post
                .iter()
                .filter(|event| event.probe == "warp_plan_published")
                .collect();
            assert_eq!(plans.len(), 1, "one command publishes one correlated plan");
            let plan = plans[0];
            assert_eq!(plan.field("warp_map_revision"), Some(u64::from(warp_map)));
            assert!(baseline_map.is_none_or(|previous| {
                plan.field("warp_map_revision")
                    .is_some_and(|next| next > previous)
            }));
            let activation = plan
                .field("activation_output")
                .expect("planned map has an output activation");
            harness.render_through(case, activation).await;
            harness.settle(case, 4).await;
            let events = usdt_trace::events();
            let adopted = events
                .iter()
                .skip(baseline.len())
                .find(|event| {
                    event.probe == "decoder_seek_epoch_observed"
                        && event.field("adopted") == Some(1)
                })
                .expect("the command-correlated decoder epoch is adopted");
            let epoch = adopted.field("current_epoch").expect("adopted epoch");
            assert!(events.iter().skip(baseline.len()).any(|event| {
                event.probe == "pcm_reader_admitted" && event.field("seek_epoch") == Some(epoch)
            }));
        }
    }
    let capture = harness
        .capture_frames(case, harness.block_frames * 16, harness.block_frames)
        .await;
    assert!(
        lane_samples(&capture, 0)
            .iter()
            .any(|sample| *sample != 0.0)
    );
    assert_eq!(
        harness.player_controls[0]
            .rt_metrics()
            .map_or(0, |metrics| metrics.underruns()),
        baseline_underruns,
        "{start:?} -> {expected_mode:?} must not add a PCM underrun"
    );
    if expected_mode != SyncMode::Off {
        let events = usdt_trace::events();
        let consumed = events
            .iter()
            .skip(baseline.len())
            .rev()
            .find(|event| event.probe == "pcm_consumed")
            .expect("post-command output carries an attributed PCM span");
        let source_frames = consumed
            .field("source_end")
            .expect("PCM source end")
            .saturating_sub(consumed.field("source_start").expect("PCM source start"));
        let output_frames = consumed
            .field("output_end")
            .expect("PCM output end")
            .saturating_sub(consumed.field("output_start").expect("PCM output start"));
        let rate = source_frames as f32 / output_frames as f32;
        assert!(
            (rate - expected_rate).abs() < 0.02,
            "{start:?} -> {expected_mode:?} must expose rate {expected_rate}; actual={rate}, source_frames={source_frames}, output_frames={output_frames}"
        );
    }
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
#[case::grid_before_play(true)]
#[case::grid_after_play(false)]
async fn late_grid_preserves_requested_playback(#[case] grid_before_play: bool) {
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
    let probe_events = usdt_trace::events()
        .into_iter()
        .filter(|event| {
            matches!(
                event.probe,
                "warp_plan_published"
                    | "prepared_launch_command_admitted"
                    | "prepared_launch_seek_begun"
                    | "prepared_launch_readiness_checked"
                    | "decoder_seek_epoch_observed"
                    | "chunk_admitted"
                    | "scheduled_seek_activated"
                    | "pcm_consumed"
                    | "pcm_underrun"
            )
        })
        .map(|event| {
            serde_json::json!({
                "probe": event.probe,
                "fields": event.fields().collect::<std::collections::BTreeMap<_, _>>(),
            })
        })
        .collect::<Vec<_>>();
    harness.tap.evidence(
        "late_grid_lifecycle",
        serde_json::json!({
            "grid_before_play": grid_before_play,
            "first_pcm": first,
            "probe_events": probe_events,
        }),
    );
    assert_eq!(first, Some(69_677));
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn disable_after_paused_late_grid_cannot_rearm_the_old_launch() {
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
        !usdt_trace::events()
            .iter()
            .any(|event| event.probe == "prepared_launch_seek_begun")
    );
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn disable_after_admitted_late_grid_cannot_start_the_old_launch() {
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
    let admitted = usdt_trace::events()
        .into_iter()
        .find(|event| {
            event.probe == "prepared_launch_command_admitted" && event.field("armed") == Some(1)
        })
        .expect("prepared launch is admitted before Disable");
    let old_epoch = admitted
        .field("seek_epoch")
        .expect("the admitted launch names its seek epoch");
    let events_before_disable = usdt_trace::events().len();
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
    let after_disable = &usdt_trace::events()[events_before_disable..];
    assert!(after_disable.iter().any(|event| {
        event.probe == "prepared_launch_cancelled"
            && event.field("item_id") == Some(harness.ids[0][0].as_u64())
            && event.field("prepared_seek_epoch") == Some(old_epoch)
            && event.field("replacement_seek_epoch") != Some(old_epoch)
            && event.field("presented") == Some(1)
    }));
    let replacement_epoch = after_disable
        .iter()
        .find(|event| {
            event.probe == "prepared_launch_cancelled"
                && event.field("prepared_seek_epoch") == Some(old_epoch)
                && event.field("presented") == Some(1)
        })
        .expect("Disable presents its replacement prepared seek")
        .field("replacement_seek_epoch")
        .expect("the cancellation names its replacement epoch");
    assert!(!after_disable.iter().any(|event| {
        event.probe == "prepared_launch_readiness_checked"
            && event.field("expected_activation") == Some(69_677)
    }));
    assert!(!after_disable.iter().any(|event| {
        event.probe == "scheduled_seek_activated" && event.field("seek_epoch") == Some(old_epoch)
    }));
    let events = usdt_trace::events();
    assert!(events.iter().any(|event| {
        event.probe == "chunk_admitted" && event.field("epoch") == Some(old_epoch)
    }));
    assert!(!events.iter().any(|event| {
        event.probe == "pcm_reader_admitted" && event.field("seek_epoch") == Some(old_epoch)
    }));
    let replacement_pcm = events
        .iter()
        .find(|event| {
            event.probe == "pcm_reader_admitted"
                && event.field("seek_epoch") == Some(replacement_epoch)
        })
        .expect("Disable admits replacement PCM");
    let replacement_source_start = replacement_pcm
        .field("source_start")
        .expect("replacement PCM names its source start");
    let source_tolerance = u64::try_from(harness.block_frames)
        .expect("fixture block size fits source-frame tolerance");
    assert!(
        replacement_source_start.abs_diff(served_target_source) <= source_tolerance,
        "Disable replacement source start {replacement_source_start} must follow served target {served_target_source} within {source_tolerance} source frames"
    );
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
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

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
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

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn normal_hostsync_pause_resume_keeps_pcm_continuity() {
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
    let consumed = usdt_trace::events()
        .into_iter()
        .filter(|event| event.probe == "pcm_consumed")
        .map(|event| {
            serde_json::json!({
                "output_start": event.field("output_start"),
                "output_end": event.field("output_end"),
                "source_start": event.field("source_start"),
                "source_end": event.field("source_end"),
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
    provider: Provider,
    source: &'static str,
    cue_in: CueIn,
    seek_seconds: Option<f64>,
    publish_grid_after_play: bool,
    expected_activation: u64,
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
#[case::first_downbeat_96_start_10(ListeningScenario {
    id: "listening-first-downbeat-96-start-10",
    provider: Provider::Rhythm(LISTENING_ALIGNED_DOWNTEMPO_96),
    source: "rhythm_wav_scenario_1_origin_zero_listening_long_downtempo_96_stereo_55s",
    cue_in: CueIn::FirstDownbeat,
    seek_seconds: Some(10.0),
    publish_grid_after_play: false,
    expected_activation: 92_903,
})]
#[case::first_downbeat_pickup_96(ListeningScenario {
    id: "listening-first-downbeat-pickup-96",
    provider: Provider::Rhythm(LISTENING_PICKUP_DOWNTEMPO_96),
    source: "rhythm_wav_scenario_1_origin_zero_pickup_listening_downtempo_96_stereo_45s",
    cue_in: CueIn::FirstDownbeat,
    seek_seconds: None,
    publish_grid_after_play: false,
    expected_activation: 92_903,
})]
#[case::track_start_pickup_96(ListeningScenario {
    id: "listening-track-start-pickup-96",
    provider: Provider::Rhythm(LISTENING_PICKUP_DOWNTEMPO_96),
    source: "rhythm_wav_scenario_1_origin_zero_pickup_listening_downtempo_96_stereo_45s",
    cue_in: CueIn::TrackStart,
    seek_seconds: None,
    publish_grid_after_play: false,
    expected_activation: 69_677,
})]
#[case::late_grid_track_start_96(ListeningScenario {
    id: "listening-late-grid-track-start-96",
    provider: Provider::Rhythm(LISTENING_PICKUP_DOWNTEMPO_96),
    source: "rhythm_wav_scenario_1_origin_zero_pickup_listening_downtempo_96_stereo_45s",
    cue_in: CueIn::TrackStart,
    seek_seconds: None,
    publish_grid_after_play: true,
    expected_activation: 69_677,
})]
#[case::first_downbeat_tunnel(ListeningScenario {
    id: "listening-first-downbeat-tunnel",
    provider: Provider::Library(PLAYLIST_TUNNEL),
    source: "library_mp3_zvuk_27390231",
    cue_in: CueIn::FirstDownbeat,
    seek_seconds: None,
    publish_grid_after_play: false,
    expected_activation: 92_903,
})]
#[case::first_downbeat_playlist_151585912(ListeningScenario {
    id: "listening-first-downbeat-playlist-151585912",
    provider: Provider::Library(PLAYLIST_151585912),
    source: "library_mp3_zvuk_151585912",
    cue_in: CueIn::FirstDownbeat,
    seek_seconds: None,
    publish_grid_after_play: false,
    expected_activation: 92_903,
})]
#[case::first_downbeat_playlist_125475417(ListeningScenario {
    id: "listening-first-downbeat-playlist-125475417",
    provider: Provider::Library(PLAYLIST_125475417),
    source: "library_mp3_zvuk_125475417",
    cue_in: CueIn::FirstDownbeat,
    seek_seconds: None,
    publish_grid_after_play: false,
    expected_activation: 92_903,
})]
#[case::first_downbeat_playlist_138535169(ListeningScenario {
    id: "listening-first-downbeat-playlist-138535169",
    provider: Provider::Library(PLAYLIST_138535169),
    source: "library_mp3_zvuk_138535169",
    cue_in: CueIn::FirstDownbeat,
    seek_seconds: None,
    publish_grid_after_play: false,
    expected_activation: 92_903,
})]
#[case::first_downbeat_playlist_130432502(ListeningScenario {
    id: "listening-first-downbeat-playlist-130432502",
    provider: Provider::Library(PLAYLIST_130432502),
    source: "library_mp3_zvuk_130432502",
    cue_in: CueIn::FirstDownbeat,
    seek_seconds: None,
    publish_grid_after_play: false,
    expected_activation: 92_903,
})]
#[case::first_downbeat_playlist_132017169(ListeningScenario {
    id: "listening-first-downbeat-playlist-132017169",
    provider: Provider::Library(PLAYLIST_132017169),
    source: "library_mp3_zvuk_132017169",
    cue_in: CueIn::FirstDownbeat,
    seek_seconds: None,
    publish_grid_after_play: false,
    expected_activation: 92_903,
})]
async fn listening_single_deck_host_metronome_preview(#[case] scenario: ListeningScenario) {
    const PRELAUNCH_FRAMES: usize = BLOCK_FRAMES * 8;
    const AUDIBLE_CAPTURE_FRAMES: usize = 48_000 * 30;
    let case = SyncCase::running(scenario.id, 1, 48_000, OperationOrder::PlaySyncSeek)
        .paused()
        .hold(124.0);
    let sources = prepared_sources(scenario.provider).await;
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
    let raw = harness.capture_paced(case, AUDIBLE_CAPTURE_FRAMES).await;
    let activation = harness.sync_activation;
    if !scenario.publish_grid_after_play {
        assert_eq!(activation, Some(scenario.expected_activation));
    }
    assert!(
        harness.host_grid.is_some(),
        "{}: sync request retains Host grid",
        scenario.id
    );
    let audible_end = harness.rendered_frames;
    let audible_beats = harness
        .tap
        .timeline()
        .events()
        .iter()
        .filter(|event| {
            event.lane == "host-grid" && (audible_start..audible_end).contains(&event.output_start)
        })
        .count();
    assert!(
        audible_beats > 0,
        "{}: the metronome must click inside the audible capture",
        scenario.id
    );
    let (_, metronome_clips) = harness.tap.metronome_mix();
    assert_eq!(
        metronome_clips, 0,
        "Host metronome listening mix must have headroom"
    );
    let underruns = harness.underrun_failures();
    harness.tap.evidence(
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
            "audible_capture_pcm_frames": raw.len() / usize::from(CHANNELS),
            "underruns": underruns,
            "raw_note": "output.wav is unmodified Host PCM; this artifact does not prove synchronization",
        }),
    );
    assert!(
        underruns.is_empty(),
        "listening capture underruns: {underruns:?}"
    );
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
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
    let probe_events = usdt_trace::events()
        .into_iter()
        .filter(|event| {
            matches!(
                event.probe,
                "warp_plan_published"
                    | "prepared_launch_command_admitted"
                    | "prepared_launch_seek_begun"
                    | "prepared_launch_readiness_checked"
                    | "decoder_seek_epoch_observed"
                    | "decoder_seek_epoch_backpressured"
                    | "scheduled_seek_activation_ready"
                    | "scheduled_seek_activated"
                    | "region_plan_reader_refreshed"
                    | "prime_activation"
                    | "rate_applied"
                    | "prepared_render_revision_selected"
                    | "render_revision_floor"
                    | "chunk_admitted"
                    | "pcm_reader_admitted"
                    | "pcm_revision_discarded"
                    | "pcm_consumed"
                    | "pcm_underrun"
            )
        })
        .map(|event| {
            serde_json::json!({
                "probe": event.probe,
                "thread": format!("{:?}", event.thread),
                "fields": event.fields().collect::<std::collections::BTreeMap<_, _>>(),
            })
        })
        .collect::<Vec<_>>();
    harness.tap.evidence(
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

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn scenario_1_simultaneous_different_bpm_exact_grids(
    #[future(awt)] source_scenario_1_downtempo_house_provider: PreparedSources,
) {
    const PRELAUNCH_FRAMES: usize = BLOCK_FRAMES * 8;
    const CAPTURE_FRAMES: usize = 48_000 * 10;
    let case = DOWNTEMPO_HOUSE_SYNC.paused();
    let mut harness = ProductHarness::new_for_block(
        case,
        &source_scenario_1_downtempo_house_provider,
        0,
        BLOCK_FRAMES,
    )
    .await;
    harness.tap.evidence(
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
        }),
    );
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
    let (left_markers, left_downbeats) =
        lane_score_markers(&capture, 0, capture_start, case.sample_rate);
    let (right_markers, right_downbeats) =
        lane_score_markers(&capture, 1, capture_start, case.sample_rate);
    let mut initial_launch_failures = scenario_1_scheduled_launch_failures(
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
    initial_launch_failures.extend(shared_phase_failures(
        (&left_markers, &left_downbeats),
        (&right_markers, &right_downbeats),
        harness
            .sync_activation
            .expect("Scenario 1 sync request records its activation frame"),
        case.sample_rate,
        harness.block_frames,
    ));
    initial_launch_failures.extend(harness.failures.iter().cloned());
    harness.tap.evidence(
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
        }),
    );
    drop(harness);
    assert!(
        initial_launch_failures.is_empty(),
        "scenario 1 shared launch contract failed: {}",
        initial_launch_failures.join("; ")
    );
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn scenario_2_simultaneous_different_bpm_with_a_pickup(
    #[future(awt)] source_scenario_2_downtempo_house_pickup_provider: PreparedSources,
) {
    const PRELAUNCH_FRAMES: usize = BLOCK_FRAMES * 8;
    const CAPTURE_FRAMES: usize = 48_000 * 10;
    let sources = &source_scenario_2_downtempo_house_pickup_provider;
    let case = DOWNTEMPO_HOUSE_SYNC.paused();
    let mut harness = ProductHarness::new_track_start(case, sources, 0).await;
    for deck in &harness.decks {
        let control = deck.control().clone();
        harness.host.run(move || control.set_muted(false)).await;
    }
    prepare_fixture_grids(&mut harness, case, sources).await;

    let prelaunch = harness.render(case, PRELAUNCH_FRAMES).await;
    assert!(
        prelaunch.iter().all(|sample| *sample == 0.0),
        "scenario 2: prelaunch Host output must remain silent"
    );
    harness.mark("scenario-2-enable");
    harness.request_sync(case).await;
    harness.mark("scenario-2-simultaneous-host-start");
    harness.play_all().await;
    let capture_start = harness.rendered_frames;
    let capture = harness
        .capture_frames(case, CAPTURE_FRAMES, harness.block_frames)
        .await;
    harness.failures.extend(harness.underrun_failures());

    let (left_markers, left_downbeats) =
        lane_score_markers(&capture, 0, capture_start, case.sample_rate);
    let (right_markers, right_downbeats) =
        lane_score_markers(&capture, 1, capture_start, case.sample_rate);
    let activation = harness
        .sync_activation
        .expect("Scenario 2 sync request records its activation frame");
    let mut failures = scenario_1_scheduled_launch_failures(
        &capture,
        &left_markers,
        capture_start,
        harness
            .host_grid
            .as_ref()
            .expect("Scenario 2 sync request records the authoritative Host grid"),
        case.sample_rate,
        case.start_bpm(),
    );
    failures.extend(pickup_phase_failures(
        (&left_markers, &left_downbeats),
        (&right_markers, &right_downbeats),
        activation,
        case.sample_rate,
        (case.start_bpm(), harness.block_frames),
    ));
    failures.extend(harness.failures.iter().cloned());
    harness.tap.evidence(
        "scenario_2",
        serde_json::json!({
            "verdict": if failures.is_empty() { "pass" } else { "fail" },
            "fixture_generation": {
                "deck_0": "rhythm_wav_scenario_1_downtempo_96_left_only / rhythm_expected_analysis_scenario_1_downtempo_96_left_only",
                "deck_1": "rhythm_wav_scenario_2_house_124_right_only_pickup / rhythm_expected_analysis_scenario_2_house_124_right_only_pickup",
                "grid_source": "expected_analysis is serialized BeatArtifact::from(score::truth), not analyzer output",
                "pickup": "deck 1 opens on the last beat of a bar; its first downbeat is its second beat"
            },
            "mix_layout": "same-session diagnostic mix: deck 0 left only, deck 1 right only",
            "sync_activation_frame": activation,
            "capture_host_start_frame": capture_start,
            "left": lane_early_measurement(&capture, 0, capture_start, case.sample_rate),
            "right": lane_early_measurement(&capture, 1, capture_start, case.sample_rate),
            "failures": failures,
        }),
    );
    drop(harness);
    assert!(
        failures.is_empty(),
        "scenario 2 shared launch contract with a pickup failed: {}",
        failures.join("; ")
    );
}

/// Beats a deck joining later must place on the grid the playing deck set.
///
/// A staggered launch gives the joining deck fewer beats, so its markers are
/// required to fall on the playing deck's Host beats rather than to match them
/// one for one.
fn joined_lane_failures(playing: &[u64], joining: &[u64]) -> Vec<String> {
    let mut failures = Vec::new();
    if joining.is_empty() {
        failures.push("the joining deck produced no beat marker".to_owned());
    }
    let stray = joining
        .iter()
        .copied()
        .filter(|frame| !playing.contains(frame))
        .collect::<Vec<_>>();
    if !stray.is_empty() {
        failures.push(format!(
            "joining deck beats {stray:?} are off the playing deck's Host beats {playing:?}"
        ));
    }
    failures
}

/// A pickup sounds exactly one beat before the downbeat it leads into.
fn pickup_lead_failures(beats: &[u64], downbeats: &[u64], period: u64) -> Vec<String> {
    match (beats.first(), downbeats.first()) {
        (Some(first_beat), Some(first_downbeat))
            if *first_downbeat == first_beat.saturating_add(period) =>
        {
            Vec::new()
        }
        (Some(first_beat), Some(first_downbeat)) => vec![format!(
            "pickup lane opens at Host frame {first_beat} and reaches its downbeat at {first_downbeat}, expected one {period}-frame beat apart"
        )],
        (beat, downbeat) => vec![format!(
            "pickup lane is missing a marker: first beat {beat:?}, first downbeat {downbeat:?}"
        )],
    }
}

async fn staggered_launch_failures(
    harness: &mut ProductHarness,
    case: SyncCase,
    pickup: bool,
) -> Vec<String> {
    const PRELAUNCH_FRAMES: usize = BLOCK_FRAMES * 8;
    const CAPTURE_FRAMES: usize = 48_000 * 10;

    let prelaunch = harness.render(case, PRELAUNCH_FRAMES).await;
    assert!(
        prelaunch.iter().all(|sample| *sample == 0.0),
        "a staggered launch must stay silent before its first deck plays"
    );
    harness.mark("staggered-enable");
    harness.request_sync(case).await;
    let period = (f64::from(case.sample_rate) * 60.0 / case.start_bpm()).round() as u64;
    let stagger = usize::try_from(period * 3 / 8).expect("stagger fits usize");
    let capture_start = harness.rendered_frames;
    let playing = harness.decks[0].control().clone();
    harness.host.run(move || playing.play()).await;
    harness.mark("staggered-playing-deck");
    let mut capture = harness
        .capture_frames(case, stagger, harness.block_frames)
        .await;
    let joining = harness.decks[1].control().clone();
    harness.host.run(move || joining.play()).await;
    harness.mark("staggered-joining-deck");
    capture.extend(
        harness
            .capture_frames(case, CAPTURE_FRAMES, harness.block_frames)
            .await,
    );

    let (left_beats, left_downbeats) =
        lane_score_markers(&capture, 0, capture_start, case.sample_rate);
    let (right_beats, right_downbeats) =
        lane_score_markers(&capture, 1, capture_start, case.sample_rate);
    let activation = harness
        .sync_activation
        .expect("a staggered sync request records its activation frame");
    // A pickup sounds before the bar it leads into, so it cannot lie on the
    // playing deck's beats; `pickup_lead_failures` pins that one beat exactly.
    let graded = if pickup {
        right_beats.get(1..).unwrap_or_default()
    } else {
        &right_beats
    };
    let mut failures = joined_lane_failures(&left_beats, graded);
    failures.extend(shared_bar_phase_failures(
        &left_downbeats,
        &right_downbeats,
        activation,
        case.sample_rate,
        harness.block_frames,
    ));
    if pickup {
        failures.extend(pickup_lead_failures(&right_beats, &right_downbeats, period));
    }
    failures.extend(harness.underrun_failures());
    harness.tap.evidence(
        if pickup { "scenario_4" } else { "scenario_3" },
        serde_json::json!({
            "verdict": if failures.is_empty() { "pass" } else { "fail" },
            "mix_layout": "same-session diagnostic mix: playing deck left only, joining deck right only",
            "stagger_frames": stagger,
            "sync_activation_frame": activation,
            "capture_host_start_frame": capture_start,
            "left": lane_early_measurement(&capture, 0, capture_start, case.sample_rate),
            "right": lane_early_measurement(&capture, 1, capture_start, case.sample_rate),
            "failures": failures,
        }),
    );
    failures
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn scenario_3_staggered_equal_tempo_exact_grids(
    #[future(awt)] source_scenario_3_house_pair_provider: PreparedSources,
) {
    let sources = &source_scenario_3_house_pair_provider;
    let case = HOUSE_PAIR_SYNC.paused();
    let mut harness = ProductHarness::new_for_block(case, sources, 0, BLOCK_FRAMES).await;
    for deck in &harness.decks {
        let control = deck.control().clone();
        harness.host.run(move || control.set_muted(false)).await;
    }
    prepare_fixture_grids(&mut harness, case, sources).await;
    let failures = staggered_launch_failures(&mut harness, case, false).await;
    drop(harness);
    assert!(
        failures.is_empty(),
        "scenario 3 staggered equal-tempo contract failed: {}",
        failures.join("; ")
    );
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn scenario_4_staggered_equal_tempo_with_a_pickup(
    #[future(awt)] source_scenario_4_house_pair_pickup_provider: PreparedSources,
) {
    let sources = &source_scenario_4_house_pair_pickup_provider;
    let case = HOUSE_PAIR_SYNC.paused();
    let mut harness = ProductHarness::new_track_start(case, sources, 0).await;
    for deck in &harness.decks {
        let control = deck.control().clone();
        harness.host.run(move || control.set_muted(false)).await;
    }
    prepare_fixture_grids(&mut harness, case, sources).await;
    let failures = staggered_launch_failures(&mut harness, case, true).await;
    drop(harness);
    assert!(
        failures.is_empty(),
        "scenario 4 staggered pickup contract failed: {}",
        failures.join("; ")
    );
}

/// A deck whose track opens on a pickup leads into the shared downbeat.
///
/// The pickup is musical geometry: its beat sounds one beat before the bar it
/// leads into, so the lane starts earlier than the aligned lane while both
/// lanes still place their downbeats on the same Host frames.
fn pickup_phase_failures(
    aligned: (&[u64], &[u64]),
    pickup: (&[u64], &[u64]),
    activation: u64,
    sample_rate: u32,
    (target_bpm, block_frames): (f64, usize),
) -> Vec<String> {
    let mut failures = Vec::new();
    let period = (f64::from(sample_rate) * 60.0 / target_bpm).round() as u64;
    match (aligned.0.first(), pickup.0.first()) {
        (Some(first_aligned), Some(first_pickup))
            if *first_aligned == first_pickup.saturating_add(period) => {}
        (Some(first_aligned), Some(first_pickup)) => failures.push(format!(
            "pickup lane opens at Host frame {first_pickup}, expected one {period}-frame beat before the aligned lane's {first_aligned}"
        )),
        (aligned_first, pickup_first) => failures.push(format!(
            "a lane has no beat marker: aligned {aligned_first:?}, pickup {pickup_first:?}"
        )),
    }
    failures.extend(shared_bar_phase_failures(
        aligned.1,
        pickup.1,
        activation,
        sample_rate,
        block_frames,
    ));
    failures
}

/// Beat and bar phase two same-session lanes must share once both decks sound.
///
/// Beats compare from the launch onward. Downbeats compare only after the
/// launch gate has settled: the gate opens smoothly, so the first accent of
/// every deck is attenuated by the same amount and a relative-threshold
/// detector may classify it differently on lanes of different loudness.
fn shared_phase_failures(
    left: (&[u64], &[u64]),
    right: (&[u64], &[u64]),
    activation: u64,
    sample_rate: u32,
    block_frames: usize,
) -> Vec<String> {
    let mut failures = Vec::new();
    if right.0 != left.0 {
        failures.push(format!(
            "right deck beats {:?} do not land on the left deck's Host beats {:?}",
            right.0, left.0
        ));
    }
    failures.extend(shared_bar_phase_failures(
        left.1,
        right.1,
        activation,
        sample_rate,
        block_frames,
    ));
    failures
}

/// Bar phase two lanes must share once the launch gate has settled.
///
/// The gate opens smoothly, so the first accent of every deck is attenuated by
/// the same amount and a relative-threshold detector may classify it
/// differently on lanes of different loudness; comparison starts past it.
fn shared_bar_phase_failures(
    left: &[u64],
    right: &[u64],
    activation: u64,
    sample_rate: u32,
    block_frames: usize,
) -> Vec<String> {
    let mut failures = Vec::new();
    let gate = kithara::play::DEFAULT_GATE_SMOOTHING;
    let settle = (f64::from(sample_rate)
        * f64::from(gate.smooth_seconds)
        * (1.0 / (2.0 * f64::from(gate.settle_epsilon))).ln())
    .ceil() as u64
        + u64::try_from(block_frames).expect("block frames fit u64");
    let settled_from = activation + settle;
    let settled = |downbeats: &[u64]| {
        downbeats
            .iter()
            .copied()
            .filter(|frame| *frame >= settled_from)
            .collect::<Vec<_>>()
    };
    let (left_bars, right_bars) = (settled(left), settled(right));
    if left_bars.is_empty() {
        failures.push(format!(
            "the aligned lane has no downbeat after the launch gate settles at Host frame {settled_from}"
        ));
    }
    if right_bars != left_bars {
        failures.push(format!(
            "one lane's downbeats {right_bars:?} do not share the other lane's bar phase {left_bars:?}"
        ));
    }
    failures
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
        let Ok(beat) = Beat::try_from(kithara::warp::BeatOrdinal::new(ordinal)) else {
            continue;
        };
        let position = match grid.position_at(MapPoint::new(grid.stamp(), beat)) {
            BeatGridQuery::Resolved(position) => position,
            _ => continue,
        };
        let MapPosition::Session(frame) = *position.value().value() else {
            continue;
        };
        let Ok(frame) = u64::try_from(i64::from(frame)) else {
            continue;
        };
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
    let label = format!("{} {provider:?}", case.id);
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
        if provider.has_score_markers() {
            let capture_end = harness.rendered_frames;
            let capture_start = capture_end - (pcm.len() / usize::from(CHANNELS)) as u64;
            let host_beats = harness
                .tap
                .host_beats_in(capture_start..capture_end)
                .into_iter()
                .map(|frame| {
                    usize::try_from(frame - capture_start).expect("capture frame fits usize")
                })
                .collect::<Vec<_>>();
            request_failures.extend(host_beat_alignment_failures(
                &format!("{label} deck {audible_deck}"),
                &pcm,
                CHANNELS,
                case.sample_rate,
                &host_beats,
            ));
        }
        tracks.push(pcm);
        let underruns = harness.underrun_failures();
        harness.failures.extend(underruns);
        request_failures.extend(harness.failures);
    }
    let track_slices = tracks.iter().map(Vec::as_slice).collect::<Vec<_>>();
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

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
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

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(600)))]
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
#[case::host_rate_change(HOST_RATE_CHANGE, source_synthetic().await)]
#[case::tempo_wobble_every_block(TEMPO_WOBBLE_EVERY_BLOCK, source_synthetic().await)]
async fn wav_product_rows_reach_the_pcm_oracle(
    #[case] case: SyncCase,
    #[case] provider: PreparedSources,
) {
    run(case, provider).await;
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(600)))]
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

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(600)))]
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
#[cfg(not(target_os = "android"))]
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
async fn source_scenario_2_downtempo_house_pickup_provider() -> PreparedSources {
    prepared_sources(SCENARIO_2_DOWNTEMPO_HOUSE_PICKUP_PROVIDER).await
}

#[kithara::fixture]
async fn source_scenario_3_house_pair_provider() -> PreparedSources {
    prepared_sources(SCENARIO_3_HOUSE_PAIR_PROVIDER).await
}

#[kithara::fixture]
async fn source_scenario_4_house_pair_pickup_provider() -> PreparedSources {
    prepared_sources(SCENARIO_4_HOUSE_PAIR_PICKUP_PROVIDER).await
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
