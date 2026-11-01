use super::imports::*;

pub(crate) const BLOCK_FRAMES: usize = 512;
/// How many blocks a build renders before it checks that a transport was
/// committed. Builds of one case are compared frame by frame, so the span has
/// to be the same every time rather than however long one race took to
/// resolve; it is wide enough that the commit lands inside it on a loaded
/// machine, and a build that still has no transport by the end says so.
pub(crate) const WARM_UP_BLOCKS: usize = 24;
pub(crate) const CHANNELS: u16 = 2;
pub(crate) const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Room in the master tap for one `render`, which renders at most a second.
pub(crate) const MASTER_TAP_SECONDS: usize = 2;
pub(crate) const START_BPM: f64 = 120.0;
/// The metronome over the scenario music: the duck takes a fifth of the mix
/// under a click, so the beat the click sits on keeps its attack, and the
/// click peaks at that fifth of the ceiling, the most the duck leaves room for.
pub(crate) const METRONOME_DUCK: f32 = 0.2;
/// A deadline of ~85 ms at 48 kHz: many times what one player's ring holds
/// at the bounded render quantum.
pub(crate) const LOOSE_RESPONSE_BUDGET: usize = 4_096;
/// The synthetic rhythm fixtures: 12 seconds at `START_BPM`, with the first
/// audible downbeat one beat after zero.
pub(crate) const SYNTHETIC_BEATS: u32 = 24;
pub(crate) const SECONDS_PER_MINUTE: f64 = 60.0;
/// A start well inside every track, off its downbeat, for a case that needs
/// no musical entry.
pub(crate) const CUE: Start = Start::Seconds(5.25);

/// The beat grid every synthetic rhythm fixture was rendered on.
pub(crate) fn synthetic_grid() -> ArtifactSource<BeatGridModel> {
    let model =
        serde_json::from_slice(kithara_test_fixtures::assets::synthetic_grid_default().bytes())
            .expect("prepared synthetic rhythm grid");
    ArtifactSource::Value(Arc::new(model))
}

#[kithara::test(native)]
fn prepared_synthetic_grid_preserves_the_authored_score() {
    let ArtifactSource::Value(model) = synthetic_grid() else {
        panic!("synthetic grid is a prepared value");
    };
    let raw = model.as_raw();
    assert_eq!(raw.schema_version, SCHEMA_VERSION);
    assert_eq!(raw.model_id, "synthetic-rhythm");
    assert_eq!(raw.revision, 1);
    assert_eq!(raw.state, BeatGridState::Final);
    assert_eq!(raw.duration, Some(12.0));
    assert_eq!(raw.bpm, START_BPM);
    assert_eq!(raw.beats.len(), SYNTHETIC_BEATS as usize);
    for (beat, ordinal) in raw.beats.iter().zip(0..SYNTHETIC_BEATS) {
        assert_eq!(
            beat.at.to_bits(),
            (f64::from(ordinal) * SECONDS_PER_MINUTE / START_BPM).to_bits()
        );
        assert_eq!(beat.ordinal, i64::from(ordinal));
        assert_eq!(beat.confidence, Some(1.0));
    }
    let meter = raw.meter.expect("synthetic meter");
    assert_eq!(meter.beats_per_bar.get(), 4);
    assert_eq!(meter.origin_beat_ordinal, 1);
    assert_eq!(raw.downbeats.len(), 6);
    for (beat, ordinal) in raw.downbeats.iter().zip((1..SYNTHETIC_BEATS).step_by(4)) {
        assert_eq!(
            beat.at.to_bits(),
            (f64::from(ordinal) * SECONDS_PER_MINUTE / START_BPM).to_bits()
        );
        assert_eq!(beat.beat_ordinal, i64::from(ordinal));
        assert_eq!(beat.confidence, Some(1.0));
    }
}

#[kithara::test(native)]
fn prepared_synthetic_grid_covers_the_complete_authored_pcm() {
    use kithara::warp::{
        AssetAxis, AssetExtent, AssetFrame, BeatEvidence, BeatGridId, BeatGridQuery,
        BeatGridRevision, BeatGridSnapshot, MapPoint, MapPosition,
    };

    let ArtifactSource::Value(model) = synthetic_grid() else {
        panic!("synthetic grid is a prepared value");
    };
    let rate = NonZeroU32::new(48_000).expect("authored sample rate");
    let media = Duration::from_nanos(11_930_854_167) - Duration::from_nanos(23_020_833);
    assert_eq!(media, Duration::from_nanos(11_907_833_334));
    let observed = media.as_secs_f64() * f64::from(rate.get());
    assert_eq!(observed, 571_576.000_032);
    let after_end =
        (Duration::from_secs(12) + Duration::from_nanos(1)).as_secs_f64() * f64::from(rate.get());
    for extent in [AssetExtent::Unknown, AssetExtent::Bounded(576_000)] {
        let grid = BeatGridSnapshot::model(
            BeatGridId::allocate().expect("fixture grid identity"),
            BeatGridRevision::first(),
            &model,
            AssetAxis::new(rate, extent),
        )
        .expect("authored finite source grid");
        let query = |frame| {
            grid.beat_at(MapPoint::new(
                grid.stamp(),
                MapPosition::Asset(AssetFrame::new(frame).expect("finite source frame")),
            ))
        };
        for frame in [552_000.0, observed, 576_000.0] {
            let BeatGridQuery::Resolved(beat) = query(frame) else {
                panic!("authored source frame {frame:?} must have a grid beat");
            };
            let expected = if frame == 552_000.0 {
                BeatEvidence::Observed
            } else {
                BeatEvidence::Extrapolated
            };
            assert_eq!(beat.evidence(), expected);
        }
        if matches!(extent, AssetExtent::Unknown) {
            assert!(matches!(query(after_end), BeatGridQuery::Uncovered { .. }));
        } else {
            assert!(matches!(query(after_end), BeatGridQuery::OutsideDomain));
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Operation {
    Play,
    Seek,
    Sync,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum OperationOrder {
    PlaySyncSeek,
    PlaySeekSync,
    SeekPlaySync,
    SeekSyncPlay,
    SyncPlaySeek,
    SyncSeekPlay,
    SequentialSync,
}

impl OperationOrder {
    pub(super) const fn operations(self) -> &'static [Operation] {
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
pub(super) enum TempoRide {
    Down,
    Hold(f64),
    Triangle,
    Up,
}

impl TempoRide {
    pub(super) const fn points(self) -> &'static [f64] {
        match self {
            Self::Down => &[116.0, 112.0, 108.0],
            Self::Hold(_) => &[],
            Self::Triangle => &[116.0, 112.0, 116.0, 120.0],
            Self::Up => &[122.0, 125.0, 127.0],
        }
    }

    pub(super) const fn start_bpm(self) -> f64 {
        match self {
            Self::Hold(bpm) => bpm,
            Self::Down | Self::Triangle | Self::Up => START_BPM,
        }
    }

    pub(super) const fn final_bpm(self) -> f64 {
        match self {
            Self::Down => 108.0,
            Self::Hold(bpm) => bpm,
            Self::Triangle => 120.0,
            Self::Up => 127.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SyncCase {
    pub(super) id: &'static str,
    pub(super) decks: usize,
    pub(crate) sample_rate: u32,
    pub(super) order: OperationOrder,
    pub(super) paused: bool,
    pub(super) ride: TempoRide,
    pub(super) updates_hz: u32,
    /// Lanes the shared playback worker admits; `None` keeps its default.
    pub(super) capacity: Option<NonZeroUsize>,
    /// Each deck's track carries its beat grid, so a group can prepare it.
    pub(super) gridded: bool,
    /// Each player's control-to-audio deadline; `None` keeps it unbounded.
    pub(super) response_budget: Option<NonZeroUsize>,
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

    pub(crate) const fn response_budget(mut self, frames: NonZeroUsize) -> Self {
        self.response_budget = Some(frames);
        self
    }

    pub(super) const fn gridded(mut self) -> Self {
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
    pub(crate) const fn decks(self) -> usize {
        self.decks
    }

    pub(crate) const fn id(self) -> &'static str {
        self.id
    }

    delegate::delegate! {
        to self.ride {
            pub(super) const fn start_bpm(self) -> f64;
            pub(crate) const fn final_bpm(self) -> f64;
        }
    }
}

pub(crate) const PLAY_SYNC_SEEK: SyncCase =
    SyncCase::running("play-sync-seek", 2, 48_000, OperationOrder::PlaySyncSeek);
pub(crate) const PLAY_SEEK_SYNC: SyncCase =
    SyncCase::running("play-seek-sync", 2, 44_100, OperationOrder::PlaySeekSync)
        .ride(TempoRide::Up, 30);
pub(crate) const SEEK_PLAY_SYNC: SyncCase =
    SyncCase::running("seek-play-sync", 2, 48_000, OperationOrder::SeekPlaySync)
        .ride(TempoRide::Down, 60);
pub(crate) const SEEK_SYNC_PLAY: SyncCase =
    SyncCase::running("seek-sync-play", 2, 44_100, OperationOrder::SeekSyncPlay)
        .ride(TempoRide::Triangle, 30);
pub(crate) const SYNC_PLAY_SEEK: SyncCase =
    SyncCase::running("sync-play-seek", 2, 48_000, OperationOrder::SyncPlaySeek)
        .ride(TempoRide::Up, 60);
pub(crate) const SYNC_SEEK_PLAY: SyncCase =
    SyncCase::running("sync-seek-play", 2, 44_100, OperationOrder::SyncSeekPlay)
        .ride(TempoRide::Down, 120);
pub(crate) const SEQUENTIAL_SYNC: SyncCase =
    SyncCase::running("sequential-sync", 2, 48_000, OperationOrder::SequentialSync);
pub(crate) const PAUSED_SYNC: SyncCase = SyncCase::running(
    "paused-sync-then-play",
    2,
    48_000,
    OperationOrder::SyncPlaySeek,
)
.paused();
pub(crate) const FOUR_DECK_SYNC: SyncCase = SyncCase::running(
    "four-deck-sequential-sync",
    4,
    48_000,
    OperationOrder::SequentialSync,
);
pub(crate) const TEMPO_UP_120: SyncCase =
    SyncCase::running("tempo-up-120hz", 2, 48_000, OperationOrder::PlaySyncSeek)
        .ride(TempoRide::Up, 120);
pub(crate) const TEMPO_DOWN_30: SyncCase =
    SyncCase::running("tempo-down-30hz", 2, 44_100, OperationOrder::PlaySyncSeek)
        .ride(TempoRide::Down, 30);
pub(crate) const ONE_DECK: SyncCase =
    SyncCase::running("one-deck-runtime", 1, 48_000, OperationOrder::PlaySyncSeek);
/// A paused deck on a host whose rate differs from the fixtures' 48 kHz.
pub(crate) const STAGED_CUE: SyncCase =
    SyncCase::running("staged-cue", 1, 44_100, OperationOrder::SyncPlaySeek)
        .paused()
        .hold(120.0)
        .gridded();
/// [`STAGED_CUE`] beside a second paused deck that keeps the session
/// running: unloading the first deck then leaves the session's grid alone,
/// where a session left with no started deck shuts down and withdraws the
/// preparation itself, racing the executor's own cancellation.
pub(crate) const STAGED_CUE_BESIDE_A_DECK: SyncCase = SyncCase::running(
    "staged-cue-beside-a-deck",
    2,
    44_100,
    OperationOrder::SyncPlaySeek,
)
.paused()
.hold(120.0)
.gridded();
/// A staged cue under a response deadline far looser than the lane's ring.
pub(crate) const STAGED_UNDER_LOOSE_DEADLINE: SyncCase = SyncCase::running(
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
pub(crate) const STAGED_BESIDE_PLAYBACK: SyncCase = SyncCase::running(
    "staged-beside-playback",
    1,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.hold(120.0)
.gridded();
/// [`STAGED_BESIDE_PLAYBACK`] with nothing staged: the PCM the sounding deck
/// must keep.
pub(crate) const STAGED_BESIDE_PLAYBACK_CONTROL: SyncCase = SyncCase::running(
    "staged-beside-playback-control",
    1,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.hold(120.0)
.gridded();
/// A sounding deck whose worker has no slot left for a staged lane.
pub(crate) const STAGED_WITHOUT_CAPACITY: SyncCase = SyncCase::running(
    "staged-without-capacity",
    1,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.hold(120.0)
.capacity(NonZeroUsize::MIN)
.gridded();
pub(crate) const SHARED_DEADLINE: SyncCase = SyncCase::running(
    "shared-worker-deadline",
    4,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.ride(TempoRide::Up, 120);
pub(crate) const SHARED_DEADLINE_CONTROL: SyncCase = SyncCase::running(
    "shared-worker-control",
    1,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.ride(TempoRide::Up, 120);

/// Two decks of one real track synced one after the other onto the Host grid.
pub(crate) const REAL_TRACK_SYNC: SyncCase = SyncCase::running(
    "real-track-sequential-sync",
    2,
    48_000,
    OperationOrder::SequentialSync,
)
.gridded();
/// Four decks of one real track, staggered, synced one after the other.
pub(crate) const REAL_TRACK_FOUR_DECK_SYNC: SyncCase = SyncCase::running(
    "real-track-four-deck-sequential-sync",
    4,
    48_000,
    OperationOrder::SequentialSync,
)
.gridded();

pub(crate) const AMBIENT_TRIP_HOP_SYNC: SyncCase = SyncCase::running(
    "ambient-dub-62-to-trip-hop-74",
    2,
    48_000,
    OperationOrder::SequentialSync,
)
.hold(74.0);
pub(crate) const DOWNTEMPO_HOUSE_SYNC: SyncCase = SyncCase::running(
    "downtempo-96-to-house-124",
    2,
    48_000,
    OperationOrder::PlaySyncSeek,
)
.hold(124.0);
pub(crate) const TECHNO_BREAKBEAT_SYNC: SyncCase = SyncCase::running(
    "techno-132-to-breakbeat-140",
    2,
    48_000,
    OperationOrder::SeekPlaySync,
)
.hold(132.0);
pub(crate) const CROSS_STYLE_SYNC: SyncCase = SyncCase::running(
    "cross-style-four-deck-124",
    4,
    48_000,
    OperationOrder::SequentialSync,
)
.hold(124.0);

pub(crate) const AMBIENT_TRIP_HOP: &[&str] = &[
    "rhythm_wav_16s_ambient_dub_62_aligned",
    "rhythm_wav_16s_trip_hop_74_aligned",
];
pub(crate) const DOWNTEMPO_HOUSE: &[&str] = &[
    "rhythm_wav_downtempo_96_aligned",
    "rhythm_wav_house_124_aligned",
];
pub(crate) const TECHNO_BREAKBEAT: &[&str] = &[
    "rhythm_wav_techno_132_aligned",
    "rhythm_wav_breakbeat_140_aligned",
];
pub(crate) const CROSS_STYLE: &[&str] = &[
    "rhythm_wav_16s_ambient_dub_62_aligned",
    "rhythm_wav_downtempo_96_aligned",
    "rhythm_wav_house_124_aligned",
    "rhythm_wav_breakbeat_140_aligned",
];
/// Richie Hawtin - The Tunnel, a straight-kick track with a steady grid.
pub(crate) const TUNNEL: &[&str] = &["library_mp3_zvuk_27390231"];
/// A straight 48 kHz techno track, the Tunnel's counterpart at the session rate.
pub(crate) const NEWTECHNO: &[&str] = &["library_flac_newtechno"];
/// Newtechno's grid states no bars: its detected downbeats disagree on the
/// bar phase. Its second phrase, where the full groove enters, opens on
/// analysed beat 64.
pub(crate) const NEWTECHNO_PHRASE: Start = Start::Beat(64);
/// The Tunnel's fifth bar: a cue well inside the track, on its kick.
pub(crate) const TUNNEL_CUE: Start = Start::bar(4);
