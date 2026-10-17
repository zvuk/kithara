use super::*;
pub(super) use crate::mock::{
    Command, TestState, ask, constant_fixture, deck_snapshot, fixture_command, fixture_pass,
    mix_registration, occupied_slots, play_fixture, probe_owner, registration, render_block,
    test_state,
};

#[derive(Default)]
pub(super) struct RouteLossProbe {
    pub(super) fail_next_start: AtomicBool,
    pub(super) start_count: AtomicUsize,
}

impl RouteLossProbe {
    pub(super) fn reset(&self) {
        self.start_count.store(0, Ordering::SeqCst);
        self.fail_next_start.store(false, Ordering::SeqCst);
    }
}

thread_local! {
    static ROUTE_LOSS: RouteLossProbe = RouteLossProbe::default();
}

pub(super) fn route_loss<R>(f: impl FnOnce(&RouteLossProbe) -> R) -> R {
    ROUTE_LOSS.with(f)
}

pub(super) fn render_left(state: &mut TestState, clock: &mut u64, blocks: usize) -> Vec<f32> {
    let mut left = Vec::new();
    for _ in 0..blocks {
        left.extend(render_block(state, *clock).into_iter().step_by(2));
        *clock += 512;
    }
    left
}

/// A stereo source holding every sample at its value.
#[derive(Clone, Copy)]
pub(super) struct DcNode(pub(super) f32);

impl AudioNode for DcNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        _cx: ConstructProcessorContext,
    ) -> Result<impl AudioNodeProcessor, NodeError> {
        Ok(*self)
    }

    fn info(&self, _config: &Self::Configuration) -> Result<AudioNodeInfo, NodeError> {
        Ok(AudioNodeInfo::new()
            .debug_name("dc")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::ZERO,
                num_outputs: ChannelCount::STEREO,
            }))
    }
}

impl AudioNodeProcessor for DcNode {
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        for output in &mut *buffers.outputs {
            output[..info.frames].fill(self.0);
        }
        ProcessStatus::OutputsModified
    }
}

#[derive(Debug, thiserror::Error)]
#[error("route lost")]
pub(super) struct RouteLossError;

pub(super) fn start_route_loss_stream(
    ctx: &mut FirewheelContext,
    sample_rate: u32,
) -> Result<SessionStream, String> {
    route_loss(|probe| probe.start_count.fetch_add(1, Ordering::SeqCst));
    if route_loss(|probe| probe.fail_next_start.swap(false, Ordering::SeqCst)) {
        return Err(RouteLossError.to_string());
    }
    OfflineStream::start(
        ctx,
        BackendConfig::builder()
            .sample_rate(NonZeroU32::new(sample_rate).expect("rate"))
            .block_frames(NonZeroU32::new(512).expect("block"))
            .declared_latency(Duration::ZERO)
            .build(),
    )
    .map(|stream| SessionStream::Offline(Box::new(stream)))
    .map_err(|error| error.to_string())
}

pub(super) fn configured_registration(
    grid_id: BeatGridId,
    quantum: Option<NonZeroUsize>,
    budget: Option<NonZeroUsize>,
) -> Command {
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .warp(
            kithara_warp::WarpConfig::builder()
                .maybe_render_quantum_frames(quantum)
                .build(),
        )
        .maybe_response_budget_frames(budget)
        .build();
    HostCommand::Register {
        id: grid_id,
        deck: Box::new(Queue::new(QueueConfig::builder().prep(prep).build())),
    }
}

/// Attaches a deck, which the session registers and starts.
pub(super) fn insert(state: &mut TestState) -> BeatGridId {
    let grid_id = BeatGridId::allocate().expect("fixture player grid id");
    match ask(state, registration(grid_id)) {
        Ok(_) => grid_id,
        Err(err) => panic!("the deck failed to start: {err}"),
    }
}

/// Stops the deck `grid_id` and removes it from the session.
pub(super) fn remove(state: &mut TestState, grid_id: BeatGridId) {
    assert!(matches!(ask(state, HostCommand::Close(grid_id)), Ok(())));
}

/// Asks the session for `rate` from the next block on.
pub(super) fn configure_sample_rate(state: &mut TestState, rate: u32) {
    let rate = NonZeroU32::new(rate).expect("a fixture rate is not zero");
    assert!(matches!(
        ask(
            state,
            HostCommand::Configure(HostSettingsChange::SampleRate(rate), When::Next)
        ),
        Ok(())
    ));
}

/// Moves the session's output to a new platform route.
pub(super) fn change_route_to(state: &mut TestState, _reason: &str) {
    assert!(matches!(ask(state, HostCommand::Restart), Ok(())));
}

pub(super) fn deck(state: &TestState, index: usize) -> &DeckNode {
    &state.deck_nodes[index]
}

pub(super) fn deck_count(state: &TestState) -> usize {
    state.deck_nodes.len()
}

pub(super) fn host_grid(state: &TestState) -> BeatGridSnapshot {
    state.root.grid().clone()
}

pub(super) fn assert_route_boundary(before: &BeatGridSnapshot, boundary: &BeatGridSnapshot) {
    assert_eq!(
        boundary.state(),
        BeatGridState::Unavailable(BeatGridUnavailable::NoGeometry)
    );
    assert!(boundary.revision() > before.revision());
    let MapAxis::Session(before_axis) = before.axis() else {
        panic!("the previous host grid uses the session axis")
    };
    let MapAxis::Session(boundary_axis) = boundary.axis() else {
        panic!("the route boundary uses the session axis")
    };
    assert!(boundary_axis.epoch() > before_axis.epoch());
}

pub(super) fn deck_by_grid(state: &TestState, grid_id: BeatGridId) -> &DeckNode {
    state
        .deck_nodes
        .iter()
        .find(|node| node.id == grid_id)
        .expect("registered deck")
}

pub(super) fn mix_tap_writer(drops: &Arc<AtomicU64>) -> MixTapWriter {
    const TAP_CAPACITY: usize = 1_024;

    let (pcm, _cons) = HeapRb::<f32>::new(TAP_CAPACITY).split();
    MixTapWriter::new(pcm, Arc::clone(drops))
}

/// Sends `change` for the next block.
pub(super) fn configure_next(
    state: &mut TestState,
    change: HostSettingsChange,
) -> Result<(), PlayError> {
    ask(state, HostCommand::Configure(change, When::Next))
}

/// The Host queue's capacity: the batches in flight before a block
/// answers them.
pub(super) fn host_queue_capacity() -> u16 {
    let capacity = kithara_command::ChannelConfig::builder()
        .build()
        .values()
        .capacity
        .get();
    u16::try_from(capacity).expect("the queue capacity fits a step count")
}

/// A metronome level of its own for every step of a run.
pub(super) fn level_change(step: u16) -> (f32, HostSettingsChange) {
    let level = 0.25 + f32::from(step) / 1024.0;
    (
        level,
        HostSettingsChange::Metronome(MetronomeConfigChange::Level(level)),
    )
}
