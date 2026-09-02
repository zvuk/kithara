#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

use firewheel::{
    channel_config::{ChannelConfig, ChannelCount},
    event::ProcEvents,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        ProcBuffers, ProcExtra, ProcInfo, ProcessStatus,
    },
};
use kithara::{
    self,
    events::EventBus,
    platform::sync::Arc,
    play::{
        Cmd, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerId, PlayerImpl, Reply,
        SessionDispatcher,
    },
};
use kithara_integration_tests::ring::{
    CountingNode, CountingProbe, DeterministicToneNode, ManualRingConfig, ManualRingSession,
    RingRenderError, RingSessionError, fixtures::install_stereo_source,
};

use crate::bufpool_ext::{TestPools, pools};

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: u32 = 512;

fn session_rate() -> NonZeroU32 {
    NonZeroU32::new(SAMPLE_RATE).expect("test sample rate is non-zero")
}

fn config(capacity_blocks: usize) -> ManualRingConfig {
    ManualRingConfig::new(session_rate(), BLOCK_FRAMES, capacity_blocks)
}

fn tone_session(capacity_blocks: usize) -> ManualRingSession {
    ManualRingSession::start_with(config(capacity_blocks), |ctx| {
        install_stereo_source(ctx, DeterministicToneNode)
            .map(|_| ())
            .map_err(RingSessionError::Setup)
    })
    .expect("start manual tone session")
}

fn expect_ok(reply: Reply) {
    match reply {
        Reply::Ok => {}
        Reply::Err(error) => panic!("session command failed: {error}"),
        _ => panic!("unexpected session command reply"),
    }
}

fn register_started_player(session: &ManualRingSession) -> PlayerId {
    let player_id = match session
        .exec(Cmd::RegisterPlayer {
            grid_id: kithara::warp::BeatGridId::allocate().expect("fixture grid id"),
            bus: EventBus::default(),
            eq_layout: Vec::new(),
            pools: pools(),
            sample_rate: SAMPLE_RATE,
        })
        .expect("register player command")
    {
        Reply::PlayerRegistered(player_id) => player_id,
        Reply::Err(error) => panic!("register player failed: {error}"),
        _ => panic!("unexpected register player reply"),
    };
    expect_ok(
        session
            .exec(Cmd::StartPlayer {
                master_volume: 1.0,
                player_id,
                render_quantum_frames: NonZeroUsize::new(64)
                    .expect("fixture render quantum is non-zero"),
                response_budget_frames: NonZeroUsize::new(639)
                    .expect("fixture response budget is non-zero"),
                sample_rate: SAMPLE_RATE,
            })
            .expect("start player command"),
    );
    player_id
}

fn remove_player(session: &ManualRingSession, player_id: PlayerId) {
    expect_ok(
        session
            .exec(Cmd::StopPlayer { player_id })
            .expect("stop unrelated player command"),
    );
    expect_ok(
        session
            .exec(Cmd::UnregisterPlayer { player_id })
            .expect("unregister unrelated player command"),
    );
}

fn empty_player(session: &Arc<ManualRingSession>) -> PlayerImpl<TestPools> {
    let dispatcher: Arc<dyn SessionDispatcher<TestPools>> = session.clone();
    PlayerImpl::new(
        PlayerConfig::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .sample_rate(session_rate())
            .crossfade_duration(0.0)
            .session(dispatcher)
            .build(),
    )
}

#[kithara::test]
fn credit_partition_is_equivalent() {
    let split = tone_session(2);
    let combined = tone_session(2);

    split.credit(1).expect("first split credit");
    split.credit(1).expect("second split credit");
    combined.credit(2).expect("combined credits");

    let total_frames = (BLOCK_FRAMES as usize) * 2;
    assert_eq!(
        split.drain(total_frames).expect("drain split session"),
        combined
            .drain(total_frames)
            .expect("drain combined session")
    );
    assert_eq!(
        split.clock_samples().expect("split clock"),
        combined.clock_samples().expect("combined clock")
    );
}

#[kithara::test]
fn commit_ledger_matches_firewheel_clock() {
    const CREDITS: usize = 5;
    let session = tone_session(CREDITS);
    let initial_clock = session.clock_samples().expect("initial clock");

    session.credit(CREDITS).expect("render credited blocks");

    let expected = (CREDITS as u64) * u64::from(BLOCK_FRAMES);
    assert_eq!(
        session.committed_frames().expect("committed ledger"),
        expected
    );
    assert_eq!(
        session.clock_samples().expect("firewheel clock") - initial_clock,
        expected
    );
}

#[kithara::test]
fn constructor_sees_session_rate_and_new_stream_never_fires() {
    let probe = CountingProbe::default();
    let node_probe = probe.clone();
    let session = ManualRingSession::start_with(config(4), move |ctx| {
        install_stereo_source(ctx, CountingNode::new(node_probe))
            .map(|_| ())
            .map_err(RingSessionError::Setup)
    })
    .expect("start counting session");

    session.credit(1).expect("construct counting node");
    let unrelated = register_started_player(&session);
    session.credit(1).expect("render after graph addition");
    remove_player(&session, unrelated);
    session.credit(1).expect("render after graph removal");

    assert_eq!(probe.construction_count(), 1);
    assert_eq!(probe.construction_sample_rate(), Some(session_rate()));
    assert_eq!(probe.new_stream_count(), 0);
}

#[kithara::test]
fn backend_starts_exactly_once() {
    let session = ManualRingSession::start(config(3)).expect("start manual ring session");
    let unrelated = register_started_player(&session);
    session.credit(2).expect("render across graph edit");
    remove_player(&session, unrelated);
    session.credit(1).expect("render after graph removal");

    assert_eq!(session.start_count().expect("backend start ledger"), 1);
}

#[kithara::test]
fn clock_is_monotone_across_pause_and_graph_edits() {
    let session = Arc::new(
        ManualRingSession::start(config(6)).expect("start manual ring session for player"),
    );
    let player = empty_player(&session);
    player
        .ensure_engine_started()
        .expect("start deterministic player engine");
    player
        .ensure_slot()
        .expect("allocate deterministic player slot");
    player.play();

    let initial = session.clock_samples().expect("initial clock");
    session.credit(1).expect("credit playing block");
    assert_eq!(
        session.clock_samples().expect("playing clock"),
        initial + u64::from(BLOCK_FRAMES)
    );

    player.pause();
    session.credit(1).expect("credit paused block");
    assert_eq!(
        session.clock_samples().expect("paused clock"),
        initial + 2 * u64::from(BLOCK_FRAMES)
    );

    player.play();
    session.credit(1).expect("credit resumed block");
    let before_edit = session.clock_samples().expect("resumed clock");
    assert_eq!(before_edit, initial + 3 * u64::from(BLOCK_FRAMES));

    let unrelated = register_started_player(&session);
    assert_eq!(
        session.clock_samples().expect("clock after add"),
        before_edit
    );
    session.credit(1).expect("credit after graph addition");
    let after_add = session
        .clock_samples()
        .expect("clock after added-node credit");
    assert_eq!(after_add, before_edit + u64::from(BLOCK_FRAMES));
    remove_player(&session, unrelated);
    assert_eq!(
        session.clock_samples().expect("clock after remove"),
        after_add
    );
    session.credit(1).expect("credit after graph removal");
    assert_eq!(
        session
            .clock_samples()
            .expect("clock after removed-node credit"),
        after_add + u64::from(BLOCK_FRAMES)
    );
}

#[kithara::test]
fn graph_edits_compile_without_tick() {
    let session = tone_session(1);
    assert!(
        session
            .drain(BLOCK_FRAMES as usize)
            .expect("drain before first credit")
            .is_empty()
    );

    session
        .credit(1)
        .expect("credit compiles pending tone graph");

    let rendered = session
        .drain(BLOCK_FRAMES as usize)
        .expect("drain compiled tone graph");
    assert!(rendered.iter().any(|sample| *sample != 0.0));
}

#[kithara::test]
fn render_before_arming_is_refused_typed() {
    let session = ManualRingSession::start(config(1)).expect("start probed ring session");

    assert_eq!(
        session.pre_arm_error().expect("pre-arm probe result"),
        Some(RingRenderError::NotArmed)
    );
    assert_eq!(session.committed_frames().expect("pre-arm ledger"), 0);
    assert_eq!(session.clock_samples().expect("pre-arm clock"), 0);
    assert!(
        session
            .drain(BLOCK_FRAMES as usize)
            .expect("pre-arm ring")
            .is_empty()
    );
}

#[kithara::test]
fn full_ring_backpressures_typed() {
    let session = tone_session(1);
    session.credit(1).expect("fill one-block ring");
    let clock_at_full = session.clock_samples().expect("clock at full ring");
    let committed_at_full = session.committed_frames().expect("ledger at full ring");

    let error = session.credit(1).expect_err("full ring must refuse credit");
    assert!(matches!(
        error,
        RingSessionError::Render(RingRenderError::Full)
    ));
    assert_eq!(
        session.clock_samples().expect("clock after refusal"),
        clock_at_full
    );
    assert_eq!(
        session.committed_frames().expect("ledger after refusal"),
        committed_at_full
    );

    let drained = session
        .drain(BLOCK_FRAMES as usize)
        .expect("drain full ring");
    assert_eq!(drained.len(), (BLOCK_FRAMES as usize) * 2);
    session.credit(1).expect("credit after draining ring");
    assert_eq!(
        session.clock_samples().expect("clock after retry"),
        clock_at_full + u64::from(BLOCK_FRAMES)
    );
}

#[kithara::test]
fn shutdown_joins_and_panics_propagate() {
    let normal = ManualRingSession::start(config(1)).expect("start normal session");
    normal.shutdown().expect("normal shutdown joins");

    let panicking = ManualRingSession::start_with(config(1), |ctx| {
        install_stereo_source(ctx, PanickingNode)
            .map(|_| ())
            .map_err(RingSessionError::Setup)
    })
    .expect("start panicking session");
    let error = panicking
        .credit(1)
        .expect_err("worker panic must surface from credit");
    assert!(matches!(
        &error,
        RingSessionError::WorkerPanicked { message } if message == "ring fixture panic"
    ));
    let shutdown_error = panicking
        .shutdown()
        .expect_err("shutdown retains worker panic");
    assert!(matches!(
        shutdown_error,
        RingSessionError::WorkerPanicked { message } if message == "ring fixture panic"
    ));
}

#[derive(Clone, Copy)]
struct PanickingNode;

impl AudioNode for PanickingNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _configuration: &Self::Configuration,
        _cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        PanickingProcessor
    }

    fn info(&self, _configuration: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name("ring_panicking")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::ZERO,
                num_outputs: ChannelCount::STEREO,
            })
    }
}

struct PanickingProcessor;

impl AudioNodeProcessor for PanickingProcessor {
    fn process(
        &mut self,
        _info: &ProcInfo,
        _buffers: ProcBuffers,
        _events: &mut ProcEvents,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        panic!("ring fixture panic")
    }
}
