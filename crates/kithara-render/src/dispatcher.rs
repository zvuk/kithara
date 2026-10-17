use std::{
    convert::Infallible,
    fmt::{self, Debug},
    future::{Future, poll_fn},
    marker::PhantomData,
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
    task::{Context, Poll, Waker},
};

use futures::{FutureExt, StreamExt, future::LocalBoxFuture, stream::FuturesUnordered};
use kithara_command::{Inbox, Protocol, Seq};
use kithara_platform::{time::Duration, tokio::runtime::Handle};
use kithara_signal::FrameCount;
use kithara_sync::SpeedCurve;
use kithara_warp::StretchKind;
use kithara_worker::{Priority, Task, TickResult};
use tracing::Instrument as _;

use crate::{LaneProtocol, LoadRefusal, ServiceClass, worker::scheduler::StreamWake};

/// Dispatcher-issued identity in load admission order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LaneId(u64);

#[derive(Debug)]
pub enum DispatcherCommand<I> {
    Load(Box<LoadRequest<I>>),
    Release(LaneId),
    SetPriority(LaneId, ServiceClass),
}

pub struct LoadRequest<I> {
    pub item: I,
    /// The lane starts Idle for a background load, or Warm for a seated track.
    pub class: ServiceClass,
    pub position: Duration,
    pub start: LaneStart,
    pub inbox: Inbox<LaneProtocol>,
}

impl<I: Debug> Debug for LoadRequest<I> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadRequest")
            .field("item", &self.item)
            .field("class", &self.class)
            .field("position", &self.position)
            .field("start", &self.start)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneStart {
    pub speed: SpeedCurve,
    pub keylock: bool,
    pub backend: StretchKind,
}

#[derive(Debug)]
pub struct Loaded<O> {
    pub lane: LaneId,
    pub opened: O,
    pub engine_latency: FrameCount,
}

#[derive(Debug)]
pub enum Dispatched<O> {
    Loaded(Loaded<O>),
    Released,
    Prioritized,
}

/// Opened receiver, worker-owned lane, and prepared engine latency, or a load refusal.
pub type OpenResult<O, L> = Result<(O, L, FrameCount), LoadRefusal>;

/// One source open yielding its receiver and its actual worker-owned lane.
pub trait Open: Debug {
    type Opened: Debug;
    type Lane;

    fn open(
        self,
        position: Duration,
        start: LaneStart,
        inbox: Inbox<LaneProtocol>,
    ) -> impl Future<Output = OpenResult<Self::Opened, Self::Lane>>;
}

/// Mutable lane state accessed only by the dispatcher that owns the task.
pub trait LaneTask: Task {
    fn set_priority(&mut self, class: ServiceClass);
    fn poll_commands(&mut self, cx: &mut Context<'_>) -> Poll<()>;
    /// Whether the lane holds its preload.
    ///
    /// # Errors
    ///
    /// Returns the refusal that ended the load: its cancellation or the
    /// source error it hit while preloading.
    fn preload_status(&mut self) -> Result<bool, LoadRefusal>;
}

#[derive(Debug)]
pub struct DispatcherProtocol<I>(PhantomData<fn() -> I>);

impl<I: Open> Protocol for DispatcherProtocol<I> {
    type Applied = Dispatched<I::Opened>;
    type Clock = ();
    type Command = DispatcherCommand<I>;
    type Refusal = LoadRefusal;
    type Target = Infallible;

    fn frames_since((): (), (): ()) -> Option<u64> {
        Some(0)
    }
}

type Opening<O, L> = LocalBoxFuture<'static, (Seq, LaneId, ServiceClass, OpenResult<O, L>)>;

struct Resident<O, L> {
    id: LaneId,
    class: ServiceClass,
    task: L,
    admission: Option<(Seq, O, FrameCount)>,
}

struct DispatchState<I: Open>
where
    I::Lane: LaneTask,
{
    inbox: Inbox<DispatcherProtocol<I>>,
    opening: FuturesUnordered<Opening<I::Opened, I::Lane>>,
    lanes: Vec<Resident<I::Opened, I::Lane>>,
    /// Bounds seated-serving lanes; the queue owns the bound on Idle lanes.
    capacity: NonZeroUsize,
    playing_opens: usize,
    outcome: TickResult,
}

impl<I: Open> DispatchState<I>
where
    I::Lane: LaneTask,
{
    fn remove_lane(&mut self, index: usize, refusal: LoadRefusal) {
        let mut resident = self.lanes.remove(index);
        let lane = resident.id;
        if let Some((seq, _, _)) = resident.admission.as_ref()
            && let Some(due) = self.inbox.resume(*seq, (), ())
        {
            due.refuse(refusal);
        }
        if catch_unwind(AssertUnwindSafe(|| resident.task.on_cancel())).is_err() {
            tracing::error!(?lane, "render lane panicked during cancellation");
        }
        if catch_unwind(AssertUnwindSafe(|| drop(resident))).is_err() {
            tracing::error!(?lane, "render lane panicked during cleanup");
        }
    }

    fn cancel_lanes(&mut self) {
        while !self.lanes.is_empty() {
            self.remove_lane(self.lanes.len() - 1, LoadRefusal::Cancelled);
        }
    }
}

impl<I: Open> Drop for DispatchState<I>
where
    I::Lane: LaneTask,
{
    fn drop(&mut self) {
        self.cancel_lanes();
    }
}

impl<I> DispatchState<I>
where
    I: Open + 'static,
    I::Opened: 'static,
    I::Lane: LaneTask,
{
    fn new(inbox: Inbox<DispatcherProtocol<I>>, capacity: NonZeroUsize) -> Self {
        Self {
            inbox,
            opening: FuturesUnordered::new(),
            lanes: Vec::with_capacity(capacity.get()),
            capacity,
            playing_opens: 0,
            outcome: TickResult::Waiting,
        }
    }

    fn poll_openings(&mut self, cx: &mut Context<'_>) -> bool {
        let mut progress = false;
        while let Poll::Ready(Some((seq, lane, class, result))) = self.opening.poll_next_unpin(cx) {
            progress = true;
            if class != ServiceClass::Idle {
                self.playing_opens = self.playing_opens.saturating_sub(1);
            }
            match result {
                Ok((opened, task, engine_latency)) => {
                    let index = self.lanes.len();
                    self.lanes.push(Resident {
                        id: lane,
                        class,
                        task,
                        admission: Some((seq, opened, engine_latency)),
                    });
                    let prepared = catch_unwind(AssertUnwindSafe(|| {
                        let task = &mut self.lanes[index].task;
                        task.warm_up();
                        task.set_priority(class);
                    }));
                    if prepared.is_err() {
                        tracing::error!(?lane, "render lane panicked during preparation");
                        self.remove_lane(
                            index,
                            LoadRefusal::Source(kithara_audio::TrackFailureKind::Render),
                        );
                    }
                }
                Err(refusal) => {
                    if let Some(due) = self.inbox.resume(seq, (), ()) {
                        due.refuse(refusal);
                    }
                }
            }
        }
        progress
    }

    fn poll(&mut self, cx: &mut Context<'_>, runtime_ready: bool) -> Poll<()> {
        let mut progress = self.poll_openings(cx);
        let _ = self.inbox.poll_drain(cx);
        if self.inbox.is_closed() {
            return Poll::Ready(());
        }
        loop {
            let Some(mut due) = self.inbox.next_due((), 1) else {
                break;
            };
            progress = true;
            if due.commands().len() != 1 {
                continue;
            }
            let Some(command) = due.commands_mut().pop() else {
                continue;
            };
            match command {
                DispatcherCommand::Load(request) => {
                    let request = *request;
                    if !runtime_ready {
                        due.refuse(LoadRefusal::NoRuntime);
                        continue;
                    }
                    if request.class != ServiceClass::Idle
                        && self
                            .lanes
                            .iter()
                            .filter(|resident| resident.class != ServiceClass::Idle)
                            .count()
                            + self.playing_opens
                            >= self.capacity.get()
                    {
                        due.refuse(LoadRefusal::Capacity {
                            capacity: self.capacity.get(),
                        });
                        continue;
                    }
                    let seq = due.defer();
                    let lane = LaneId(seq.get());
                    if request.class != ServiceClass::Idle {
                        self.playing_opens += 1;
                    }
                    let span = tracing::debug_span!(
                        "dispatcher_load",
                        ?seq,
                        ?lane,
                        class = ?request.class,
                    );
                    let class = request.class;
                    self.opening.push(
                        async move {
                            let result = AssertUnwindSafe(async move {
                                request
                                    .item
                                    .open(request.position, request.start, request.inbox)
                                    .await
                            })
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|_| {
                                tracing::error!(?lane, "source open panicked");
                                Err(LoadRefusal::Source(kithara_audio::TrackFailureKind::Render))
                            });
                            tracing::debug!(
                                opened = result.is_ok(),
                                error = ?result.as_ref().err(),
                                "source open finished"
                            );
                            (seq, lane, class, result)
                        }
                        .instrument(span)
                        .boxed_local(),
                    );
                }
                DispatcherCommand::Release(lane) => {
                    due.apply(Dispatched::Released);
                    if let Some(index) = self.lanes.iter().position(|resident| resident.id == lane)
                    {
                        self.remove_lane(index, LoadRefusal::Cancelled);
                    }
                }
                DispatcherCommand::SetPriority(lane, class) => {
                    if let Some(resident) =
                        self.lanes.iter_mut().find(|resident| resident.id == lane)
                    {
                        resident.task.set_priority(class);
                        resident.class = class;
                        due.apply(Dispatched::Prioritized);
                    } else {
                        due.refuse(LoadRefusal::Cancelled);
                    }
                }
            }
        }
        self.poll_lanes(cx, progress);
        Poll::Pending
    }

    fn poll_lanes(&mut self, cx: &mut Context<'_>, mut progress: bool) {
        self.lanes.sort_unstable_by(|left, right| {
            right
                .task
                .priority()
                .cmp(&left.task.priority())
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut waiting = false;
        let mut upstream = !self.opening.is_empty();
        let mut index = 0;
        while index < self.lanes.len() {
            let resident = &mut self.lanes[index];
            let span = tracing::debug_span!(
                "dispatcher_lane",
                lane = ?resident.id,
                class = ?resident.class,
                seq = ?resident.admission.as_ref().map(|(seq, _, _)| *seq),
            );
            let _entered = span.enter();
            tracing::trace!(
                target: "kithara_render_probe",
                probe = "dispatcher_resident_poll",
                "polling resident lane",
            );
            let visited = catch_unwind(AssertUnwindSafe(|| {
                let lane = &mut resident.task;
                let _ = lane.poll_commands(cx);
                lane.recycle();
                let result = lane.tick();
                let preload = resident.admission.is_some().then(|| lane.preload_status());
                (result, preload)
            }));
            let Ok((result, preload)) = visited else {
                tracing::error!(lane = ?resident.id, "render lane panicked");
                self.remove_lane(
                    index,
                    LoadRefusal::Source(kithara_audio::TrackFailureKind::Render),
                );
                progress = true;
                continue;
            };
            tracing::trace!(
                target: "kithara_render_probe",
                probe = "dispatcher_resident_tick",
                ?result,
                "resident lane tick",
            );
            progress |= result == TickResult::Progress;
            waiting |= result == TickResult::Waiting;
            upstream |= result == TickResult::UpstreamPending;
            match preload {
                Some(Ok(true)) => {
                    progress = true;
                    if let Some((seq, opened, engine_latency)) = resident.admission.take()
                        && let Some(due) = self.inbox.resume(seq, (), ())
                    {
                        due.apply(Dispatched::Loaded(Loaded {
                            lane: resident.id,
                            opened,
                            engine_latency,
                        }));
                    }
                }
                Some(Err(refusal)) => {
                    progress = true;
                    self.remove_lane(index, refusal);
                    continue;
                }
                Some(Ok(false)) | None => {}
            }
            index += 1;
        }
        self.outcome = if progress {
            TickResult::Progress
        } else if waiting {
            TickResult::Waiting
        } else if upstream {
            TickResult::UpstreamPending
        } else {
            TickResult::Backpressured
        };
        if progress {
            cx.waker().wake_by_ref();
        }
    }

    fn priority(&self) -> Option<Priority> {
        self.lanes
            .iter()
            .filter_map(|resident| resident.task.priority())
            .max()
    }
}

/// Drive source opens, commands and resident lanes on the current owner thread.
/// The caller supplies the execution context required by its sources.
pub async fn dispatch<I>(inbox: Inbox<DispatcherProtocol<I>>)
where
    I: Open + 'static,
    I::Opened: 'static,
    I::Lane: LaneTask,
{
    let mut dispatcher = DispatchState::new(inbox, crate::consts::CAPACITY);
    poll_fn(|cx| dispatcher.poll(cx, true)).await;
}

pub(crate) struct DispatcherTask<I: Open>
where
    I::Lane: LaneTask,
{
    dispatcher: DispatchState<I>,
    waker: Waker,
    runtime: Option<Handle>,
}

impl<I> DispatcherTask<I>
where
    I: Open + 'static,
    I::Opened: 'static,
    I::Lane: LaneTask,
{
    pub(crate) fn new(
        inbox: Inbox<DispatcherProtocol<I>>,
        capacity: NonZeroUsize,
        wake: kithara_worker::Wake,
        runtime: Option<Handle>,
    ) -> Self {
        Self {
            dispatcher: DispatchState::new(inbox, capacity),
            waker: Waker::from(kithara_platform::sync::Arc::new(StreamWake::new(wake))),
            runtime,
        }
    }
}

impl<I> Task for DispatcherTask<I>
where
    I: Open + 'static,
    I::Opened: 'static,
    I::Lane: LaneTask,
{
    delegate::delegate! {
        to self.dispatcher {
            fn priority(&self) -> Option<Priority>;
            #[call(cancel_lanes)]
            fn on_cancel(&mut self);
        }
    }

    fn tick(&mut self) -> TickResult {
        #[cfg(not(target_arch = "wasm32"))]
        let _runtime = self.runtime.as_ref().map(Handle::enter);
        let mut context = Context::from_waker(&self.waker);
        let runtime_ready = cfg!(target_arch = "wasm32") || self.runtime.is_some();
        if self.dispatcher.poll(&mut context, runtime_ready).is_ready() {
            TickResult::Done
        } else {
            self.dispatcher.outcome
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "dispatcher_panic_tests.rs"]
mod panic_tests;

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroUsize,
        pin::pin,
        task::{Context, Poll, Waker},
    };

    use futures::channel::oneshot;
    use kithara_command::{
        Batch, ChannelConfig, Outcome, Receipt, Rejection, Sender, When, channel,
    };
    use kithara_test_utils::kithara;

    use super::*;

    /// An item whose open ends with what the test sends it.
    #[derive(Debug)]
    struct Gate(oneshot::Receiver<Result<u32, LoadRefusal>>);

    struct TestLane;
    impl Task for TestLane {
        fn tick(&mut self) -> TickResult {
            TickResult::Waiting
        }
    }
    impl LaneTask for TestLane {
        fn preload_status(&mut self) -> Result<bool, LoadRefusal> {
            Ok(true)
        }
        fn set_priority(&mut self, _class: ServiceClass) {}
        fn poll_commands(&mut self, _cx: &mut Context<'_>) -> Poll<()> {
            Poll::Pending
        }
    }

    impl Open for Gate {
        type Opened = u32;
        type Lane = TestLane;

        async fn open(
            self,
            _position: Duration,
            _start: LaneStart,
            _inbox: Inbox<LaneProtocol>,
        ) -> Result<(u32, TestLane, FrameCount), LoadRefusal> {
            self.0
                .await
                .expect("the test answers every open it lets run")
                .map(|value| (value, TestLane, FrameCount::new(0)))
        }
    }

    type Protocol = DispatcherProtocol<Gate>;

    #[kithara::test]
    fn an_idle_open_never_takes_a_playing_open_s_capacity() {
        let (mut sender, inbox) = channel::<Protocol>(ChannelConfig::builder().build());
        let mut dispatcher = DispatchState::new(inbox, NonZeroUsize::MIN);
        let mut answers = Vec::new();
        let mut sequences = Vec::new();
        for class in [
            ServiceClass::Warm,
            ServiceClass::Idle,
            ServiceClass::Idle,
            ServiceClass::Warm,
        ] {
            let (answer, input) = oneshot::channel();
            answers.push(answer);
            sequences.push(
                sender
                    .send(
                        When::Next,
                        Batch {
                            basis: Vec::new(),
                            commands: vec![DispatcherCommand::Load(Box::new(LoadRequest {
                                item: Gate(input),
                                class,
                                position: Duration::ZERO,
                                start: LaneStart {
                                    speed: SpeedCurve::Constant(1.0),
                                    keylock: false,
                                    backend: StretchKind::default(),
                                },
                                inbox: channel(ChannelConfig::builder().build()).1,
                            }))],
                        },
                    )
                    .expect("load channel room"),
            );
            let mut context = Context::from_waker(Waker::noop());
            assert!(dispatcher.poll(&mut context, true).is_pending());
        }
        assert_eq!(
            dispatcher.opening.len(),
            3,
            "one Warm and two Idle opens are admitted"
        );
        assert_eq!(dispatcher.playing_opens, 1);
        let refused = sender.receipts().next().expect("second Warm is refused");
        assert_eq!(refused.seq(), sequences[3]);
        assert!(matches!(
            refused.outcome(),
            Outcome::Rejected(Rejection::Refused(LoadRefusal::Capacity { capacity: 1 }))
        ));
        assert!(
            sender.receipts().next().is_none(),
            "all admitted opens stay pending"
        );
        drop(answers);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[derive(Debug)]
    struct ExternalSource {
        input: std::sync::mpsc::Receiver<()>,
        blocked: Option<oneshot::Sender<()>>,
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl kithara_audio::AudioSource for ExternalSource {
        type Chunk = kithara_signal::AudioChunk;

        fn seek(
            &mut self,
            target: Duration,
        ) -> Result<kithara_audio::SeekOutcome, kithara_audio::AudioReadError> {
            Ok(kithara_audio::SeekOutcome::Landed {
                target,
                landed_at: target,
            })
        }

        fn set_host_sample_rate(&mut self, _rate: std::num::NonZeroU32) {}

        fn host_sample_rate(&self) -> Option<std::num::NonZeroU32> {
            None
        }

        fn step_track(&mut self) -> kithara_audio::TrackStep<Self::Chunk> {
            if self.input.try_recv().is_ok() {
                kithara_audio::TrackStep::Produced(kithara_audio::Fetch::data(
                    crate::mock::pcm_fixture::chunk(kithara_signal::SegmentId::FIRST, &[1.0; 4096]),
                ))
            } else {
                if let Some(blocked) = self.blocked.take() {
                    blocked.send(()).expect("report first blocked decode");
                }
                kithara_audio::TrackStep::Blocked(kithara_audio::WaitingReason::Waiting)
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl Open for ExternalSource {
        type Opened = crate::PcmReceiver;
        type Lane = crate::DecoderNode<Self, crate::test_pools::TestPools>;

        async fn open(
            self,
            _position: Duration,
            _start: LaneStart,
            _inbox: Inbox<LaneProtocol>,
        ) -> OpenResult<Self::Opened, Self::Lane> {
            let spec =
                kithara_signal::AudioSpec::new(1, std::num::NonZeroU32::new(48_000).expect("rate"));
            let (lane, pcm, _commands) = crate::worker::terminal_node(self, spec, true);
            Ok((pcm, lane, FrameCount::new(0)))
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(tokio)]
    #[kithara::hang_watchdog(ctx = String)]
    async fn an_external_dispatcher_wake_completes_a_waiting_preload() {
        use crate::{PlayWorker, PlayWorkerConfig, test_pools::pools};

        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
        let (mut sender, inbox) =
            channel::<DispatcherProtocol<ExternalSource>>(ChannelConfig::builder().build());
        let _dispatcher = worker.start_dispatcher(inbox).expect("dispatcher starts");
        let (input, source) = std::sync::mpsc::channel();
        let (blocked, first_block) = oneshot::channel();
        let seq = sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![DispatcherCommand::Load(Box::new(LoadRequest {
                        item: ExternalSource {
                            input: source,
                            blocked: Some(blocked),
                        },
                        class: ServiceClass::Warm,
                        position: Duration::ZERO,
                        start: LaneStart {
                            speed: SpeedCurve::Constant(1.0),
                            keylock: false,
                            backend: StretchKind::default(),
                        },
                        inbox: worker.lane_channel().1,
                    }))],
                },
            )
            .expect("load batch");
        hang_tick!(format!(
            "dispatcher load {seq:?} waits for its source to block"
        ));
        kithara_platform::time::timeout(__hang_detector.remaining(), first_block)
            .await
            .expect("source reaches its first block")
            .expect("source blocks before producing");
        hang_reset!();
        assert!(sender.receipts().next().is_none(), "Load waits for preload");
        input
            .send(())
            .expect("make upstream input available without a source waker");
        worker.wake();
        let budget = Duration::from_secs(2).min(__hang_detector.remaining());
        let receipt = kithara_platform::time::timeout(
            budget,
            poll_fn(|cx| {
                hang_tick!(format!(
                    "dispatcher load {seq:?} waits for preload after wake"
                ));
                sender.hold(cx.waker().clone());
                sender.receipts().next().map_or(Poll::Pending, Poll::Ready)
            }),
        )
        .await
        .expect("dispatcher gate wake must complete preload");
        hang_reset!();
        sender.release();
        assert_eq!(receipt.seq(), seq);
        assert!(matches!(
            receipt.outcome(),
            Outcome::Applied {
                data: Dispatched::Loaded(_),
                ..
            }
        ));
    }

    fn gate() -> (oneshot::Sender<Result<u32, LoadRefusal>>, Gate) {
        let (answer, opening) = oneshot::channel();
        (answer, Gate(opening))
    }

    fn pair() -> (Sender<Protocol>, Inbox<Protocol>) {
        channel(
            ChannelConfig::builder()
                .capacity(NonZeroUsize::new(4).expect("four batches"))
                .build(),
        )
    }

    fn send(sender: &mut Sender<Protocol>, items: Vec<Gate>) -> Seq {
        sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: items
                        .into_iter()
                        .map(|item| {
                            DispatcherCommand::Load(Box::new(LoadRequest {
                                item,
                                class: ServiceClass::Warm,
                                position: Duration::ZERO,
                                start: LaneStart {
                                    speed: SpeedCurve::Constant(1.0),
                                    keylock: false,
                                    backend: StretchKind::default(),
                                },
                                inbox: channel(ChannelConfig::builder().build()).1,
                            }))
                        })
                        .collect(),
                },
            )
            .expect("the channel has room")
    }

    fn receipts(sender: &mut Sender<Protocol>) -> Vec<Receipt<Protocol>> {
        sender.receipts().collect()
    }

    #[kithara::test]
    fn unassembled_opens_are_upstream_pending() {
        let (mut sender, inbox) = pair();
        let (_answer, opening) = gate();
        send(&mut sender, vec![opening]);
        let mut dispatcher = DispatchState::new(inbox, NonZeroUsize::MIN);
        let mut context = Context::from_waker(Waker::noop());
        assert!(dispatcher.poll(&mut context, true).is_pending());
        assert!(dispatcher.poll(&mut context, true).is_pending());
        assert_eq!(dispatcher.outcome, TickResult::UpstreamPending);
        assert!(receipts(&mut sender).is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(tokio)]
    async fn releasing_an_admitting_lane_refuses_its_load_and_frees_capacity() {
        type Loads = DispatcherProtocol<ExternalSource>;

        let (mut sender, inbox) = channel::<Loads>(ChannelConfig::builder().build());
        let (_input, source) = std::sync::mpsc::channel();
        let (blocked, _first_block) = oneshot::channel();
        let seq = sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![DispatcherCommand::Load(Box::new(LoadRequest {
                        item: ExternalSource {
                            input: source,
                            blocked: Some(blocked),
                        },
                        class: ServiceClass::Warm,
                        position: Duration::ZERO,
                        start: LaneStart {
                            speed: SpeedCurve::Constant(1.0),
                            keylock: false,
                            backend: StretchKind::default(),
                        },
                        inbox: channel(ChannelConfig::builder().build()).1,
                    }))],
                },
            )
            .expect("load batch");
        let mut dispatcher = DispatchState::new(inbox, NonZeroUsize::MIN);
        let mut context = Context::from_waker(Waker::noop());
        assert!(dispatcher.poll(&mut context, true).is_pending());
        assert!(dispatcher.poll(&mut context, true).is_pending());
        assert!(
            sender.receipts().next().is_none(),
            "admission retains the Load receipt"
        );
        assert_eq!(dispatcher.lanes.len(), 1, "admission occupies capacity");
        let release = sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![DispatcherCommand::Release(LaneId(seq.get()))],
                },
            )
            .expect("release admitting lane");
        assert!(dispatcher.poll(&mut context, true).is_pending());
        let answered: Vec<_> = sender.receipts().collect();
        assert_eq!(answered.len(), 2);
        assert!(answered.iter().any(|receipt| receipt.seq() == seq
            && matches!(
                receipt.outcome(),
                Outcome::Rejected(Rejection::Refused(LoadRefusal::Cancelled))
            )));
        assert!(answered.iter().any(|receipt| receipt.seq() == release
            && matches!(
                receipt.outcome(),
                Outcome::Applied {
                    data: Dispatched::Released,
                    ..
                }
            )));
        assert!(
            dispatcher.lanes.is_empty(),
            "release frees the admission slot"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    #[kithara::hang_watchdog(ctx = String)]
    fn a_play_worker_without_a_runtime_refuses_an_open() {
        use crate::{PlayWorker, PlayWorkerConfig, test_pools::pools};

        assert!(Handle::try_current().is_err());
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
        let (mut sender, inbox) = pair();
        let _task = worker
            .start_dispatcher(inbox)
            .expect("the dispatcher starts");
        let (_answer, opening) = gate();
        let seq = send(&mut sender, vec![opening]);
        let deadline = kithara_platform::time::WallInstant::now() + Duration::from_secs(5);

        loop {
            hang_tick!(format!("dispatcher load {seq:?} waits for NoRuntime"));
            if let Some(receipt) = sender.receipts().next() {
                hang_reset!();
                assert_eq!(receipt.seq(), seq);
                assert!(matches!(
                    receipt.outcome(),
                    Outcome::Rejected(Rejection::Refused(LoadRefusal::NoRuntime))
                ));
                break;
            }
            assert!(
                kithara_platform::time::WallInstant::now() < deadline,
                "the load must settle"
            );
            hang_park!(|remaining| {
                kithara_platform::thread::sleep(Duration::from_millis(1).min(remaining));
            });
        }
    }

    #[kithara::test]
    fn opens_run_together_and_each_receipt_carries_its_own_open() {
        let (mut sender, inbox) = pair();
        let (first_answer, first_gate) = gate();
        let (second_answer, second_gate) = gate();
        let first = send(&mut sender, vec![first_gate]);
        let second = send(&mut sender, vec![second_gate]);
        let mut dispatcher = pin!(dispatch(inbox));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(dispatcher.as_mut().poll(&mut cx).is_pending());
        assert!(receipts(&mut sender).is_empty(), "both opens still run");

        second_answer.send(Ok(2)).expect("the second open waits");
        assert!(dispatcher.as_mut().poll(&mut cx).is_pending());
        let answered = receipts(&mut sender);
        assert!(
            matches!(
                answered.as_slice(),
                [receipt] if receipt.seq() == second
                    && matches!(receipt.outcome(), Outcome::Applied { data: Dispatched::Loaded(Loaded { opened: 2, .. }), .. })
            ),
            "the second open ends first and answers its own batch: {answered:?}"
        );

        first_answer
            .send(Err(LoadRefusal::Capacity { capacity: 1 }))
            .expect("the first open still waits");
        assert!(dispatcher.as_mut().poll(&mut cx).is_pending());
        let answered = receipts(&mut sender);
        assert!(
            matches!(
                answered.as_slice(),
                [receipt] if receipt.seq() == first
                    && matches!(
                        receipt.outcome(),
                        Outcome::Rejected(Rejection::Refused(LoadRefusal::Capacity { capacity: 1 }))
                    )
            ),
            "a refused open answers its batch with the refusal: {answered:?}"
        );
    }

    #[kithara::test]
    fn a_batch_of_two_items_opens_neither() {
        let (mut sender, inbox) = pair();
        let (_first_answer, first_gate) = gate();
        let (_second_answer, second_gate) = gate();
        let both = send(&mut sender, vec![first_gate, second_gate]);
        let mut dispatcher = pin!(dispatch(inbox));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(dispatcher.as_mut().poll(&mut cx).is_pending());

        let answered = receipts(&mut sender);
        let [receipt] = answered.as_slice() else {
            panic!("the batch is answered at once: {answered:?}");
        };
        assert_eq!(receipt.seq(), both);
        assert!(matches!(
            receipt.outcome(),
            Outcome::Rejected(Rejection::Unanswered)
        ));
        let Some(receipt) = answered.into_iter().next() else {
            unreachable!("one receipt matched above");
        };
        let (_, returned): (Outcome<Protocol>, Batch<Protocol>) = receipt.into();
        assert_eq!(returned.commands.len(), 2, "both items come back unopened");
    }

    #[kithara::test]
    fn a_dropped_sender_ends_the_dispatcher_and_its_opens() {
        let (mut sender, inbox) = pair();
        let (answer, opening) = gate();
        send(&mut sender, vec![opening]);
        let mut dispatcher = pin!(dispatch(inbox));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(dispatcher.as_mut().poll(&mut cx).is_pending());
        assert!(!answer.is_canceled(), "the open runs");

        drop(sender);

        assert_eq!(dispatcher.as_mut().poll(&mut cx), Poll::Ready(()));
        assert!(answer.is_canceled(), "the dispatcher drops the open it ran");
    }
}
