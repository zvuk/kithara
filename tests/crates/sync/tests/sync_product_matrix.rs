#![cfg(not(target_arch = "wasm32"))]

#[cfg(not(target_os = "android"))]
use std::{env, io};
use std::{
    num::{NonZeroU32, NonZeroUsize},
    ops::Range,
};

#[cfg(not(target_os = "android"))]
use kithara::{
    assets::{AssetResource, AssetResourceState, AssetSource, AssetStore, ReadSide, ResourceKey},
    encode::EncodeConfig,
    host::Host,
    output::{OfflineRenderRequest, OfflineRenderer},
    platform::CancelScope,
    record::{RecordingConfig, RecordingCore, RecordingSink},
    signal::AudioSpec,
};
use kithara::{
    beat::{BeatGridModel, BeatGridState, GridBeat, RawBeatGrid, SCHEMA_VERSION},
    hls::AbrMode,
    host::{HostConfig, HostOwned},
    platform::{
        sync::Arc,
        time::{self, Duration, Instant},
    },
    play::{
        ArtifactSource, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig,
        ResourceSrc, Tempo,
    },
    queue::{Queue, QueueConfig, TrackSource, TrackStatus, Transition},
    signal::SessionFrame,
    sync::{AlignmentSource, LoadGeneration, SyncGroup, SyncIntent, SyncOperation},
    warp::{
        AssetFrame, Beat, BeatGridQuery, BeatGridSnapshot, BeatOrdinal, MapPoint, MapPosition,
        PresentationFrontier,
    },
};
#[cfg(not(target_os = "android"))]
use kithara_app::recording::AssetPartSink;
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    audio_artifact::{AudioArtifactTap, artifact_label},
    bufpool_ext::{TestPools, pools},
    cochlea::{marked_synchronization_failures, synchronization_failures},
    fixture_protocol::EncryptionRequest,
    grid::{Start, analysed_grid},
    hls_fixture::{aes128_iv, aes128_key_bytes},
    kithara, memory_asset_store,
    offline::OfflineHostHarness,
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

pub(super) const BLOCK_FRAMES: usize = 512;
pub(super) const CHANNELS: u16 = 2;
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const START_BPM: f64 = 120.0;
/// A deadline of ~85 ms at 48 kHz: many times what one player's ring holds
/// at the bounded render quantum.
const LOOSE_RESPONSE_BUDGET: usize = 4_096;
/// The synthetic rhythm fixtures: 12 seconds at `START_BPM`, first beat on
/// frame 0.
const SYNTHETIC_BEATS: u32 = 24;
const SECONDS_PER_MINUTE: f64 = 60.0;
/// A start well inside every track, off its downbeat, for a case that needs
/// no musical entry.
pub(super) const CUE: Start = Start::Seconds(5.25);

/// The beat grid every synthetic rhythm fixture was rendered on.
fn synthetic_grid() -> ArtifactSource<BeatGridModel> {
    let spacing = SECONDS_PER_MINUTE / START_BPM;
    let beats = (0..SYNTHETIC_BEATS)
        .map(|ordinal| GridBeat {
            at: f64::from(ordinal) * spacing,
            ordinal: i64::from(ordinal),
            confidence: Some(1.0),
        })
        .collect();
    let model = BeatGridModel::try_from(RawBeatGrid {
        schema_version: SCHEMA_VERSION,
        model_id: "synthetic-rhythm".to_owned(),
        revision: 1,
        state: BeatGridState::Final,
        duration: None,
        bpm: START_BPM,
        beats,
        downbeats: Vec::new(),
        meter: None,
    })
    .expect("synthetic rhythm beats form a valid grid");
    ArtifactSource::Value(Arc::new(model))
}

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
    /// Lanes the shared playback worker admits; `None` keeps its default.
    capacity: Option<NonZeroUsize>,
    /// Each deck's track carries its beat grid, so a group can prepare it.
    gridded: bool,
    /// Each player's control-to-audio deadline; `None` keeps it unbounded.
    response_budget: Option<NonZeroUsize>,
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
            capacity: None,
            gridded: false,
            response_budget: None,
        }
    }

    const fn response_budget(mut self, frames: NonZeroUsize) -> Self {
        self.response_budget = Some(frames);
        self
    }

    const fn gridded(mut self) -> Self {
        self.gridded = true;
        self
    }

    const fn capacity(mut self, lanes: NonZeroUsize) -> Self {
        self.capacity = Some(lanes);
        self
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
/// A paused deck on a host whose rate differs from the fixtures' 48 kHz.
pub(super) const STAGED_CUE: SyncCase =
    SyncCase::running("staged-cue", 1, 44_100, OperationOrder::SyncPlaySeek)
        .paused()
        .hold(120.0)
        .gridded();
/// [`STAGED_CUE`] beside a second paused deck that keeps the session
/// running: unloading the first deck then leaves the session's grid alone,
/// where a session left with no started deck shuts down and withdraws the
/// preparation itself, racing the executor's own cancellation.
pub(super) const STAGED_CUE_BESIDE_A_DECK: SyncCase = SyncCase::running(
    "staged-cue-beside-a-deck",
    2,
    44_100,
    OperationOrder::SyncPlaySeek,
)
.paused()
.hold(120.0)
.gridded();
/// A staged cue under a response deadline far looser than the lane's ring.
pub(super) const STAGED_UNDER_LOOSE_DEADLINE: SyncCase = SyncCase::running(
    "staged-under-loose-deadline",
    1,
    44_100,
    OperationOrder::SyncPlaySeek,
)
.paused()
.hold(120.0)
.gridded()
.response_budget(NonZeroUsize::new(LOOSE_RESPONSE_BUDGET).expect("loose budget is not zero"));
/// A deck that keeps sounding while a lane is staged beside it.
pub(super) const STAGED_BESIDE_PLAYBACK: SyncCase = SyncCase::running(
    "staged-beside-playback",
    1,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.hold(120.0)
.gridded();
/// [`STAGED_BESIDE_PLAYBACK`] with nothing staged: the PCM the sounding deck
/// must keep.
pub(super) const STAGED_BESIDE_PLAYBACK_CONTROL: SyncCase = SyncCase::running(
    "staged-beside-playback-control",
    1,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.hold(120.0)
.gridded();
/// A sounding deck whose worker has no slot left for a staged lane.
pub(super) const STAGED_WITHOUT_CAPACITY: SyncCase = SyncCase::running(
    "staged-without-capacity",
    1,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.hold(120.0)
.capacity(NonZeroUsize::MIN)
.gridded();
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

/// Two decks of one real track synced one after the other onto the Host grid.
pub(super) const REAL_TRACK_SYNC: SyncCase = SyncCase::running(
    "real-track-sequential-sync",
    2,
    48_000,
    OperationOrder::SequentialSync,
)
.gridded();
/// Four decks of one real track, staggered, synced one after the other.
pub(super) const REAL_TRACK_FOUR_DECK_SYNC: SyncCase = SyncCase::running(
    "real-track-four-deck-sequential-sync",
    4,
    48_000,
    OperationOrder::SequentialSync,
)
.gridded();

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
pub(super) const LIBRARY: &[&str] = &["library_flac_song2", "library_flac_slowtechno"];
/// Richie Hawtin - The Tunnel, a straight-kick track with a steady grid.
pub(super) const TUNNEL: &[&str] = &["library_mp3_zvuk_27390231"];
/// A straight 48 kHz techno track, the Tunnel's counterpart at the session rate.
pub(super) const NEWTECHNO: &[&str] = &["library_flac_newtechno"];
/// Newtechno's grid states no bars: its detected downbeats disagree on the
/// bar phase. Its second phrase, where the full groove enters, opens on
/// analysed beat 64.
pub(super) const NEWTECHNO_PHRASE: Start = Start::Beat(64);
/// The Tunnel's fifth bar: a cue well inside the track, on its kick.
pub(super) const TUNNEL_CUE: Start = Start::bar(4);

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
pub(super) const TECHNO_BREAKBEAT_PROVIDER: Provider = Provider::Rhythm(TECHNO_BREAKBEAT);
pub(super) const CROSS_STYLE_PROVIDER: Provider = Provider::Rhythm(CROSS_STYLE);

impl Provider {
    pub(super) const ALL: &[Self] = &[
        Self::Synthetic,
        Self::Rhythm(CROSS_STYLE),
        Self::HlsSame(HlsProtection::Plain),
        Self::HlsSame(HlsProtection::Drm),
        Self::Library(LIBRARY),
        Self::Library(TUNNEL),
        Self::Library(NEWTECHNO),
        Self::Mp3Same,
        Self::Mp3Distinct,
        Self::HlsMp3(HlsProtection::Plain),
        Self::HlsMp3(HlsProtection::Drm),
        Self::Sweep,
    ];

    const fn has_score_markers(self) -> bool {
        matches!(self, Self::Rhythm(_))
    }

    /// The beat grid deck `deck`'s track was rendered or analysed on.
    fn beat_grid(self, deck: usize) -> ArtifactSource<BeatGridModel> {
        match self {
            Self::Synthetic => synthetic_grid(),
            Self::Library(names) => {
                ArtifactSource::Value(Arc::new(analysed_grid(names[deck % names.len()])))
            }
            other => panic!("{other:?} declares no beat grid for a gridded case"),
        }
    }

    /// The second deck `deck`'s track opens at for `start`.
    fn start_seconds(self, deck: usize, start: Start) -> f64 {
        let grid = match self {
            Self::Library(names) => Some(analysed_grid(names[deck % names.len()])),
            _ => None,
        };
        start.seconds(grid.as_ref())
    }
}

/// Marks every beat the Host session grid places inside `frames` on the
/// artifact's metronome. Output frames are session frames: the harness
/// renders its session from frame 0.
fn mark_host_beats(tap: &mut AudioArtifactTap, grid: &BeatGridSnapshot, frames: Range<u64>) {
    let at = |frame: u64| {
        grid.beat_at(MapPoint::new(
            grid.stamp(),
            MapPosition::Session(SessionFrame::new(i64::try_from(frame).unwrap_or(i64::MAX))),
        ))
    };
    let (BeatGridQuery::Resolved(first), BeatGridQuery::Resolved(last)) =
        (at(frames.start), at(frames.end))
    else {
        return;
    };
    let first = f64::from(*first.value().value()).ceil() as i64;
    let last = f64::from(*last.value().value()).floor() as i64;
    for ordinal in first..=last {
        let Ok(beat) = Beat::try_from(BeatOrdinal::new(ordinal)) else {
            continue;
        };
        let BeatGridQuery::Resolved(position) = grid.position_at(MapPoint::new(grid.stamp(), beat))
        else {
            continue;
        };
        let MapPosition::Session(frame) = *position.value().value() else {
            continue;
        };
        let Ok(frame) = u64::try_from(i64::from(frame)) else {
            continue;
        };
        if !frames.contains(&frame) {
            continue;
        }
        let downbeat = matches!(
            grid.meter_at(MapPoint::new(grid.stamp(), beat)),
            BeatGridQuery::Resolved(meter)
                if (ordinal - i64::from(meter.value().downbeat()))
                    .rem_euclid(i64::from(meter.value().beats_per_bar()))
                    == 0
        );
        tap.host_beat(frame, downbeat);
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
    pub(super) host: OfflineHostHarness<TestPools>,
    /// Records the render commits `transport_revision` reads for this harness.
    _trace: usdt_trace::Scope,
    output_frames: u64,
    paced: bool,
    provider: Provider,
    /// The second each deck's start opens it at, before its stagger.
    cues: Vec<f64>,
    /// Every rendered block with the Host metronome, when artifacts are on.
    tap: Option<AudioArtifactTap>,
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

/// Which decks a harness run hears.
#[derive(Clone, Copy, Debug)]
pub(super) enum Audible {
    /// One deck solo; every other deck is muted.
    Deck(usize),
    /// Every deck.
    Mix,
}

impl Audible {
    const fn hears(self, deck: usize) -> bool {
        match self {
            Self::Deck(solo) => solo == deck,
            Self::Mix => true,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Deck(deck) => format!("deck-{deck}"),
            Self::Mix => "mix".to_owned(),
        }
    }
}

/// The artifact of one harness run, named by the running test, the case and
/// the decks it hears.
fn open_tap(case: SyncCase, provider: Provider, audible: Audible) -> Option<AudioArtifactTap> {
    let mut tap = AudioArtifactTap::from_env(
        &format!("{}-{}-{}", artifact_label(), case.id, audible.label()),
        case.sample_rate,
        CHANNELS,
    )
    .unwrap_or_else(|error| panic!("{}: open the listening artifact: {error}", case.id))?;
    tap.evidence(
        "provider",
        serde_json::Value::String(format!("{provider:?}")),
    );
    Some(tap)
}

impl ProductHarness {
    /// A harness whose decks seek to `start` when the case seeks.
    pub(super) async fn new(
        case: SyncCase,
        prepared: &PreparedSources,
        start: Start,
        audible: Audible,
    ) -> Self {
        Self::build(case, prepared, start, audible, BLOCK_FRAMES, false).await
    }

    pub(super) async fn new_for_block(
        case: SyncCase,
        prepared: &PreparedSources,
        start: Start,
        audible: Audible,
        block_frames: usize,
    ) -> Self {
        Self::build(case, prepared, start, audible, block_frames, true).await
    }

    async fn build(
        case: SyncCase,
        prepared: &PreparedSources,
        start: Start,
        audible: Audible,
        block_frames: usize,
        paced: bool,
    ) -> Self {
        let provider = prepared.0;
        let sources: Vec<_> = prepared
            .2
            .iter()
            .cycle()
            .take(case.decks)
            .cloned()
            .collect();
        let pools = pools();
        let worker = PlayWorker::new(
            PlayWorkerConfig::builder(pools.clone())
                .maybe_capacity(case.capacity)
                .build(),
        );
        let sample_rate = NonZeroU32::new(case.sample_rate).expect("fixture sample rate");
        let render_block_frames = NonZeroU32::new(
            u32::try_from(block_frames).expect("offline render block count fits u32"),
        )
        .expect("offline render block count is non-zero");
        let session = HostConfig::offline(pools)
            .sample_rate(sample_rate)
            .max_block_frames(render_block_frames)
            .build();
        let trace = usdt_trace::scope();
        let host = OfflineHostHarness::new(session)
            .await
            .unwrap_or_else(|error| panic!("{}: create offline Host: {error}", case.id));
        let mut decks = Vec::with_capacity(sources.len());
        let mut ids = Vec::with_capacity(sources.len());
        for (index, source) in sources.into_iter().enumerate() {
            let player = PlayerImpl::new(
                PlayerConfig::builder()
                    .worker(worker.clone())
                    .sample_rate(sample_rate)
                    .crossfade_duration(0.0)
                    .maybe_response_budget_frames(case.response_budget)
                    .build(),
            );
            let queue = Queue::new(QueueConfig::builder().player(player).build());
            queue.set_muted(!audible.hears(index));
            let deck = host
                .insert(queue)
                .await
                .unwrap_or_else(|error| panic!("{}: insert deck {index}: {error}", case.id));
            let config = ResourceConfig::for_src(
                ResourceSrc::parse(&source)
                    .unwrap_or_else(|error| panic!("{}: parse source {source}: {error}", case.id)),
            )
            .store(memory_asset_store())
            .initial_abr_mode(AbrMode::manual(0))
            .discriminator(format!("{}-{provider:?}-{index}", case.id))
            .maybe_beat_grid(case.gridded.then(|| provider.beat_grid(index)))
            .build();
            let control = deck.control().clone();
            let id = host
                .run(move || control.append(TrackSource::Config(Box::new(config))))
                .await
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
            provider,
            cues: (0..case.decks)
                .map(|deck| provider.start_seconds(deck, start))
                .collect(),
            tap: open_tap(case, provider, audible),
            _trace: trace,
        };
        harness.wait_loaded(case, &ids).await;
        for (index, (deck, id)) in harness.decks.iter().zip(ids).enumerate() {
            let control = deck.control().clone();
            harness
                .host
                .run(move || control.select(id, Transition::None))
                .await
                .unwrap_or_else(|error| panic!("{}: select deck {index}: {error}", case.id));
        }
        harness.set_tempo(case, case.start_bpm(), true).await;
        harness.warm_up_transport(case).await;
        if !case.paused {
            harness.start_staggered(case).await;
        }
        harness
    }

    /// The Host reports its transport from the last render it committed, so a
    /// harness that hands a revision to `request_sync` must have committed one
    /// first. The warm-up render usually is that commit; on a loaded host it
    /// can return before the renderer publishes, and one more render is what
    /// the wait costs.
    async fn warm_up_transport(&mut self, case: SyncCase) {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        loop {
            let _ = self.render(case, self.block_frames).await;
            if self.host.transport_revision().await.is_ok() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{}: no render committed a session transport",
                case.id
            );
        }
    }

    async fn wait_loaded(&mut self, case: SyncCase, ids: &[kithara::events::TrackId]) {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        loop {
            self.tick_all(case).await;
            let mut loaded = true;
            for (index, (deck, id)) in self.decks.iter().zip(ids).enumerate() {
                match deck.track(*id).map(|track| track.status) {
                    Some(TrackStatus::Loaded | TrackStatus::Consumed) => {}
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
        let start = self.output_frames;
        let end = start
            .checked_add(u64::try_from(frames).expect("render frame count fits u64"))
            .expect("offline render timeline fits u64");
        let samples = self.host.render(frames).await;
        assert_eq!(self.host.position(), end);
        self.tick_all(case).await;
        self.output_frames = end;
        if let Some(tap) = self.tap.as_mut() {
            let grid = self.host.session_grid().await;
            mark_host_beats(tap, &grid, start..end);
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

    /// Stamps a control marker on the artifact, when artifacts are on.
    pub(super) fn mark(&mut self, label: &str) {
        if let Some(tap) = self.tap.as_mut() {
            tap.mark(label);
        }
    }

    pub(super) async fn settle(&mut self, case: SyncCase, blocks: usize) {
        for _ in 0..blocks {
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
                let _ = self.render(case, stagger_frames).await;
            }
        }
        self.settle(case, 2).await;
    }

    /// The second deck `deck`'s track opens at for `start`.
    pub(super) fn start_seconds(&self, deck: usize, start: Start) -> f64 {
        self.provider.start_seconds(deck, start)
    }

    pub(super) async fn seek_staggered(&mut self, case: SyncCase) {
        let stagger_seconds = 3.0 / 8.0 * 60.0 / case.start_bpm();
        for (index, deck) in self.decks.iter().enumerate() {
            deck.seek(self.cues[index] + index as f64 * stagger_seconds)
                .unwrap_or_else(|error| panic!("{}: seek deck {index}: {error}", case.id));
        }
        self.settle(case, 96).await;
    }

    pub(super) async fn set_tempo(&mut self, case: SyncCase, bpm: f64, required: bool) {
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

    async fn transport_revision(&self, case: SyncCase) -> kithara::signal::TransportRevision {
        self.host
            .transport_revision()
            .await
            .unwrap_or_else(|error| panic!("{}: query Host transport: {error}", case.id))
    }

    pub(super) async fn request_sync(&mut self, case: SyncCase) {
        self.request_sync_intent(case, SyncIntent::Enable).await;
    }

    pub(super) async fn request_sync_intent(&mut self, case: SyncCase, intent: SyncIntent) {
        self.mark(&format!("sync {intent:?}"));
        let transport = self.transport_revision(case).await;
        for index in 0..self.decks.len() {
            {
                let deck = &self.decks[index];
                let playback = deck.playback_view();
                let position = playback.position.unwrap_or(0.0);
                let source = if playback.playing {
                    AlignmentSource::Audible {
                        frontier: PresentationFrontier::builder()
                            .source((position * f64::from(case.sample_rate)).max(0.0) as u64)
                            .output(SessionFrame::new(
                                i64::try_from(self.output_frames).unwrap_or(i64::MAX),
                            ))
                            .build(),
                        speed: f64::from(deck.rate()),
                    }
                } else {
                    AlignmentSource::Prepared(
                        AssetFrame::new((position * f64::from(case.sample_rate)).max(0.0))
                            .unwrap_or_else(|error| {
                                panic!("{}: cue deck {index}: {error:?}", case.id)
                            }),
                    )
                };
                let target = deck.id();
                let activation =
                    SessionFrame::new(i64::try_from(self.output_frames).unwrap_or(i64::MAX));
                let _ = self
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
    let failed_sink = AssetPartSink::acquire(&store, &failed_key).map_or_else(
        |error| panic!("acquire failing sink: {error}"),
        FailingPartSink,
    );
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
                        "BLOCKED_FIXTURE: library fixture `{name}` is not registered; build without KITHARA_DISABLE_REMOTE_FIXTURES"
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

async fn run(case: SyncCase, prepared: PreparedSources, start: Start) {
    let provider = prepared.0;
    let expected_samples = (f64::from(case.sample_rate) * 60.0 / case.ride.final_bpm() * 6.0)
        .round() as usize
        * usize::from(CHANNELS);
    let mut tracks = Vec::with_capacity(case.decks);
    let mut request_failures = Vec::new();
    for audible_deck in 0..case.decks {
        let mut harness =
            ProductHarness::new(case, &prepared, start, Audible::Deck(audible_deck)).await;
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
#[case::mp3(source_mp3_same().await)]
#[case::drm(source_hls_same_drm().await)]
async fn encoded_rhythmic_controls_reach_the_pcm_oracle(#[case] prepared: PreparedSources) {
    let provider = prepared.0;
    let mut harness = ProductHarness::new(ONE_DECK, &prepared, CUE, Audible::Deck(0)).await;
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
    run(case, provider, CUE).await;
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
    run(case, provider, CUE).await;
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
#[case::tunnel_play_sync_seek(tunnel_sources().await, TUNNEL_CUE, PLAY_SYNC_SEEK.gridded())]
#[case::tunnel_play_seek_sync(tunnel_sources().await, TUNNEL_CUE, PLAY_SEEK_SYNC.gridded())]
#[case::tunnel_seek_play_sync(tunnel_sources().await, TUNNEL_CUE, SEEK_PLAY_SYNC.gridded())]
#[case::tunnel_seek_sync_play(tunnel_sources().await, TUNNEL_CUE, SEEK_SYNC_PLAY.gridded())]
#[case::tunnel_sync_play_seek(tunnel_sources().await, TUNNEL_CUE, SYNC_PLAY_SEEK.gridded())]
#[case::tunnel_sync_seek_play(tunnel_sources().await, TUNNEL_CUE, SYNC_SEEK_PLAY.gridded())]
#[case::tunnel_sequential_sync(tunnel_sources().await, TUNNEL_CUE, SEQUENTIAL_SYNC.gridded())]
#[case::tunnel_paused_sync_then_play(tunnel_sources().await, TUNNEL_CUE, PAUSED_SYNC.gridded())]
#[case::tunnel_four_deck_sequential_sync(tunnel_sources().await, TUNNEL_CUE, FOUR_DECK_SYNC.gridded())]
#[case::tunnel_tempo_up_120hz(tunnel_sources().await, TUNNEL_CUE, TEMPO_UP_120.gridded())]
#[case::tunnel_tempo_down_30hz(tunnel_sources().await, TUNNEL_CUE, TEMPO_DOWN_30.gridded())]
#[case::newtechno_play_sync_seek(newtechno_sources().await, NEWTECHNO_PHRASE, PLAY_SYNC_SEEK.gridded())]
#[case::newtechno_play_seek_sync(newtechno_sources().await, NEWTECHNO_PHRASE, PLAY_SEEK_SYNC.gridded())]
#[case::newtechno_seek_play_sync(newtechno_sources().await, NEWTECHNO_PHRASE, SEEK_PLAY_SYNC.gridded())]
#[case::newtechno_seek_sync_play(newtechno_sources().await, NEWTECHNO_PHRASE, SEEK_SYNC_PLAY.gridded())]
#[case::newtechno_sync_play_seek(newtechno_sources().await, NEWTECHNO_PHRASE, SYNC_PLAY_SEEK.gridded())]
#[case::newtechno_sync_seek_play(newtechno_sources().await, NEWTECHNO_PHRASE, SYNC_SEEK_PLAY.gridded())]
#[case::newtechno_sequential_sync(newtechno_sources().await, NEWTECHNO_PHRASE, SEQUENTIAL_SYNC.gridded())]
#[case::newtechno_paused_sync_then_play(newtechno_sources().await, NEWTECHNO_PHRASE, PAUSED_SYNC.gridded())]
#[case::newtechno_four_deck_sequential_sync(newtechno_sources().await, NEWTECHNO_PHRASE, FOUR_DECK_SYNC.gridded())]
#[case::newtechno_tempo_up_120hz(newtechno_sources().await, NEWTECHNO_PHRASE, TEMPO_UP_120.gridded())]
#[case::newtechno_tempo_down_30hz(newtechno_sources().await, NEWTECHNO_PHRASE, TEMPO_DOWN_30.gridded())]
async fn real_track_product_rows_reach_the_pcm_oracle(
    #[case] provider: PreparedSources,
    #[case] start: Start,
    #[case] case: SyncCase,
) {
    run(case, provider, start).await;
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
pub(super) async fn tunnel_sources() -> PreparedSources {
    prepared_sources(Provider::Library(TUNNEL)).await
}

#[kithara::fixture]
pub(super) async fn newtechno_sources() -> PreparedSources {
    prepared_sources(Provider::Library(NEWTECHNO)).await
}
