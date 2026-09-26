use kithara_platform::{
    CancelToken,
    maybe_send::MaybeSendFuture,
    sync::{Arc, Mutex, mpsc as blocking},
    tokio::{
        runtime::Handle,
        sync::{mpsc, oneshot},
    },
};
use kithara_signal::{SessionFrame, TransportRevision};
use kithara_test_utils::kithara;
use kithara_warp::{BeatGrid, BeatGridId, BeatGridQuery, PresentationFrontier, WarpPlan};

use super::{
    TestGrid, TestGroup,
    modes::{Group, anchor_at_rate, parent_id, parent_stamp, parent_update, rate, synced_deck},
    preparation::{asset_grid, attach_grid, cue, window},
};
use crate::{
    AlignmentSource, ArmPermit, ExecutedGroup, LoadGeneration, ParentFact, PermitCell, ReceiptSink,
    StagePort, SyncAdmission, SyncApplied, SyncArbiter, SyncAttachment, SyncCapability, SyncEffect,
    SyncError, SyncExecutionReject, SyncExecutionStamp, SyncExecutor, SyncGroup, SyncIntent,
    SyncOperation, SyncReceipt, SyncReceiptAck,
};

/// The first session frame no caller can use.
const OPEN_END: i64 = i64::MAX;

/// A lane that reports when it is released.
struct Lane(Option<oneshot::Sender<()>>);

impl Drop for Lane {
    fn drop(&mut self) {
        if let Some(released) = self.0.take() {
            let _ = released.send(());
        }
    }
}

/// Stages every plan at once and announces each staged lane.
#[derive(Clone)]
struct Port {
    runtime: Handle,
    staged: mpsc::UnboundedSender<oneshot::Receiver<()>>,
    installed: Arc<Mutex<Vec<Lane>>>,
}

impl StagePort for Port {
    type Media = u64;
    type Lane = Lane;

    fn runtime(&self) -> &Handle {
        &self.runtime
    }

    fn stage(
        self,
        _plan: WarpPlan,
        _cancel: CancelToken,
    ) -> impl MaybeSendFuture<Output = Result<Lane, SyncExecutionReject>> + 'static {
        async move {
            let (released, release) = oneshot::channel();
            let _ = self.staged.send(release);
            Ok(Lane(Some(released)))
        }
    }

    fn handoff(
        self,
        _media: Self::Media,
        lane: Self::Lane,
        _cancel: CancelToken,
        _permit: ArmPermit,
    ) -> Result<(), SyncExecutionReject> {
        self.installed.lock().push(lane);
        Ok(())
    }
}

/// How the fake owner answers receipts.
#[derive(Clone, Copy)]
enum Answer {
    Record,
    RefuseInstalled,
}

/// An owner that records every receipt it is handed; the first answer waits
/// for `gate` when one is set.
struct Owner {
    answer: Answer,
    bound: bool,
    gate: Mutex<Option<blocking::Receiver<()>>>,
    heard: mpsc::UnboundedSender<SyncReceipt>,
    arbiter: Arc<SyncArbiter>,
    cell: Arc<PermitCell>,
}

impl ReceiptSink for Owner {
    fn is_bound(&self) -> bool {
        self.bound
    }

    fn acknowledge(&self, receipt: SyncReceipt) -> SyncReceiptAck {
        let _ = self.heard.send(receipt);
        if let Some(gate) = self.gate.lock().take() {
            let _ = gate.recv();
        }
        match (self.answer, receipt) {
            (Answer::RefuseInstalled, SyncReceipt::Installed(_)) => SyncReceiptAck::Refused,
            (_, SyncReceipt::Installed(stamp)) => {
                let control = self.arbiter.try_control().expect("fake owner enters");
                let permit = control
                    .mint_permit(&self.cell, stamp)
                    .expect("exact permit");
                SyncReceiptAck::Installed(permit)
            }
            _ => SyncReceiptAck::Recorded,
        }
    }
}

struct Fixture {
    group: ExecutedGroup<Group>,
    track: BeatGridId,
    executor: SyncExecutor<Port>,
    heard: mpsc::UnboundedReceiver<SyncReceipt>,
    staged: mpsc::UnboundedReceiver<oneshot::Receiver<()>>,
}

impl Fixture {
    /// A synced deck whose loaded track the executor stages, reporting to
    /// an owner that answers with `answer`.
    fn new(answer: Answer, gate: Option<blocking::Receiver<()>>) -> Self {
        let track = BeatGridId::allocate().expect("grid id");
        let (heard_tx, heard) = mpsc::unbounded_channel();
        let installed = Arc::new(Mutex::new(Vec::new()));
        let owner = Owner {
            answer,
            bound: true,
            gate: Mutex::new(gate),
            heard: heard_tx,
            arbiter: Arc::new(SyncArbiter::new()),
            cell: Arc::new(PermitCell::new(track)),
        };
        let executor = SyncExecutor::new(track, Some(Arc::new(owner)), CancelToken::root());
        let (staged_tx, staged) = mpsc::unbounded_channel();
        executor.load(
            1,
            Some(Port {
                runtime: Handle::current(),
                staged: staged_tx,
                installed,
            }),
        );
        Self {
            group: ExecutedGroup::new(deck_holding(track), executor.execution()),
            track,
            executor,
            heard,
            staged,
        }
    }

    /// Prepares the track from `frame`; returns the preparation's stamp once
    /// its lane is staged.
    async fn prepare(&mut self, frame: u64) -> (SyncExecutionStamp, oneshot::Receiver<()>) {
        let admission = self
            .group
            .transact(cue_at(self.track, frame))
            .expect("the cue is admitted");
        let SyncAdmission::Prepared(preparation) = admission else {
            panic!("expected a preparation, got {admission:?}");
        };
        let released = self.staged.recv().await.expect("the lane is staged");
        (preparation.stamp(), released)
    }

    async fn next_receipt(&mut self) -> SyncReceipt {
        self.heard.recv().await.expect("the owner hears a receipt")
    }
}

/// A synced deck holding the geometry of `track`.
fn deck_holding(track: BeatGridId) -> Group {
    let mut deck = synced_deck();
    let _ = attach_grid(&mut deck, asset_grid(track, 960_000, 24_000));
    deck
}

fn cue_at(track: BeatGridId, frame: u64) -> SyncOperation<TestGroup> {
    SyncOperation::Prepare {
        target: track,
        load: LoadGeneration::first(),
        transport: TransportRevision::first(),
        source: cue(frame),
        window: window(0, OPEN_END),
    }
}

fn cancelled(stamp: SyncExecutionStamp) -> SyncReceipt {
    SyncReceipt::Rejected {
        stamp,
        reason: SyncExecutionReject::Cancelled,
    }
}

#[kithara::test(tokio)]
async fn an_installed_lane_the_owner_refuses_is_dropped_and_reported_cancelled() {
    let mut fixture = Fixture::new(Answer::RefuseInstalled, None);

    let (stamp, released) = fixture.prepare(24_000).await;

    assert_eq!(fixture.next_receipt().await, SyncReceipt::Installed(stamp));
    assert_eq!(
        fixture.next_receipt().await,
        cancelled(stamp),
        "an owner that kept the preparation pending hears that its lane is gone"
    );
    assert_eq!(released.await, Ok(()), "the refused lane is released");
}

#[kithara::test(tokio)]
async fn a_lane_dropped_before_its_turn_is_reported_only_by_its_cancellation() {
    let (open, gate) = blocking::channel();
    let mut fixture = Fixture::new(Answer::Record, Some(gate));

    let (first, _) = fixture.prepare(24_000).await;
    assert_eq!(fixture.next_receipt().await, SyncReceipt::Installed(first));
    // The owner is still answering `first`: what follows queues behind it.
    let (superseded, _) = fixture.prepare(48_000).await;
    let (dropped, _) = fixture.prepare(72_000).await;
    fixture.executor.unload();
    let _ = open.send(());

    assert_eq!(
        fixture.next_receipt().await,
        cancelled(dropped),
        "neither the superseded {superseded:?} nor the dropped lane is reported installed"
    );
}

#[kithara::test(tokio)]
async fn a_staged_preparation_needs_an_owner_and_a_stageable_load() {
    let track = BeatGridId::allocate().expect("grid id");
    let (heard, _) = mpsc::unbounded_channel();
    let unbound = Owner {
        answer: Answer::Record,
        bound: false,
        gate: Mutex::new(None),
        heard,
        arbiter: Arc::new(SyncArbiter::new()),
        cell: Arc::new(PermitCell::new(track)),
    };
    let refused = |executor: &SyncExecutor<Port>| {
        ExecutedGroup::new(deck_holding(track), executor.execution())
            .transact(cue_at(track, 24_000))
            .expect_err("the executor refuses the cue")
            .error()
            .clone()
    };
    let refused_sync = |executor: &SyncExecutor<Port>| {
        let deck = deck_holding(track);
        let target = deck.id();
        ExecutedGroup::new(deck, executor.execution())
            .transact(SyncOperation::Sync {
                target,
                load: LoadGeneration::first(),
                transport: TransportRevision::first(),
                source: cue(24_000),
                activation: SessionFrame::new(0),
                intent: SyncIntent::Enable,
            })
            .expect_err("the executor refuses public Enable")
            .error()
            .clone()
    };

    let orphan = SyncExecutor::<Port>::new(track, None, CancelToken::root());
    assert_eq!(refused(&orphan), SyncError::OwnerUnavailable);
    assert_eq!(refused_sync(&orphan), SyncError::OwnerUnavailable);
    let detached = SyncExecutor::<Port>::new(track, Some(Arc::new(unbound)), CancelToken::root());
    assert_eq!(refused(&detached), SyncError::OwnerUnavailable);
    assert_eq!(refused_sync(&detached), SyncError::OwnerUnavailable);

    let (heard, _) = mpsc::unbounded_channel();
    let owner = Owner {
        answer: Answer::Record,
        bound: true,
        gate: Mutex::new(None),
        heard,
        arbiter: Arc::new(SyncArbiter::new()),
        cell: Arc::new(PermitCell::new(track)),
    };
    let unstageable = SyncExecutor::<Port>::new(track, Some(Arc::new(owner)), CancelToken::root());
    unstageable.load(1, None);
    assert_eq!(
        refused(&unstageable),
        SyncError::CapabilityUnavailable {
            capability: SyncCapability::Alignment,
        }
    );
    assert_eq!(
        refused_sync(&unstageable),
        SyncError::CapabilityUnavailable {
            capability: SyncCapability::Alignment,
        }
    );
}

#[kithara::test(tokio)]
async fn mapped_public_reenable_stages_a_replacement_after_a_tempo_ramp() {
    let mut fixture = Fixture::new(Answer::Record, None);
    let deck = fixture.group.id();
    let _ = fixture
        .group
        .transact(SyncOperation::Tempo {
            target: deck,
            tempo: kithara_warp::BeatsPerMinute::try_from(180.0).expect("tempo"),
            commit: SessionFrame::new(0),
            smoothing: 1.0,
        })
        .expect("a one-second local ramp is admitted");
    let initial = fixture
        .group
        .transact(cue_at(fixture.track, 0))
        .expect("initial local map is prepared");
    let SyncAdmission::Prepared(initial) = initial else {
        panic!("one initial projection: {initial:?}");
    };
    let _ = fixture.staged.recv().await.expect("initial lane stages");
    assert_eq!(
        fixture.next_receipt().await,
        SyncReceipt::Installed(initial.stamp())
    );
    let _ = fixture
        .group
        .acknowledge(SyncReceipt::Installed(initial.stamp()))
        .expect("owner installs the initial lane");
    let _ = fixture
        .group
        .acknowledge(SyncReceipt::Armed(initial.stamp()))
        .expect("initial lane is claimed");
    let SyncEffect::Projection { plan: old, .. } = initial.effect() else {
        panic!("initial decision must be a projection");
    };
    let previous = old.activation();
    let _ = fixture
        .group
        .acknowledge(SyncReceipt::Presented(
            SyncApplied::builder()
                .stamp(initial.stamp())
                .frontier(
                    PresentationFrontier::builder()
                        .warp_map(previous.revision())
                        .source(previous.source())
                        .output(previous.output())
                        .build(),
                )
                .build(),
        ))
        .expect("the ramped local map is sounding");
    let parent = parent_id();
    let staged = fixture
        .group
        .stage_fact(ParentFact::Segment(parent_update(
            parent_stamp(parent, 1),
            anchor_at_rate(2.0, 48_000),
        )))
        .expect("parent is staged");
    let _ = fixture.group.apply_staged(staged);
    let BeatGridQuery::Resolved(rate) = old.rate_at(previous.output()) else {
        panic!("the sounding ramp has a rate");
    };
    let admission = fixture
        .group
        .transact(SyncOperation::Sync {
            target: deck,
            load: initial.stamp().load(),
            transport: initial.stamp().transport(),
            source: AlignmentSource::Audible {
                frontier: PresentationFrontier::builder()
                    .warp_map(previous.revision())
                    .source(previous.source())
                    .output(previous.output())
                    .build(),
                speed: rate,
            },
            activation: SessionFrame::new(96_000),
            intent: SyncIntent::Enable,
        })
        .expect("mapped public ON is admitted");
    let SyncAdmission::StateChanged { transition, .. } = admission else {
        panic!("mapped public ON issues a replacement: {admission:?}");
    };
    let [replacement] = transition.issued() else {
        panic!("one replacement");
    };
    let SyncEffect::Projection {
        alignment,
        replaces: Some(map),
        ..
    } = replacement.effect()
    else {
        panic!("the mapped ON must project over its sounding map");
    };
    assert_eq!(*map, previous.revision());
    let activation = replacement.activation().1;
    let BeatGridQuery::Resolved(future_source) = old.source_at(activation) else {
        panic!("the old ramp covers the replacement boundary");
    };
    let true_next_beat = (f64::from(future_source) / 24_000.0).ceil();
    let scalar_source = previous.source() as f64
        + (i64::from(activation) - i64::from(previous.output())) as f64 * rate;
    let scalar_next_beat = (scalar_source / 24_000.0).ceil();
    assert_ne!(
        true_next_beat, scalar_next_beat,
        "the active ramp must distinguish source_at from scalar extrapolation"
    );
    assert_eq!(f64::from(*alignment.source().value()), true_next_beat);
    assert_eq!(f64::from(*alignment.target().value()), 4.0);
    let _ = kithara_platform::time::timeout(
        kithara_platform::time::Duration::from_secs(2),
        fixture.staged.recv(),
    )
    .await
    .expect("the mapped replacement reaches its staging port")
    .expect("the staging port stays open");
    assert_eq!(
        fixture.next_receipt().await,
        SyncReceipt::Installed(replacement.stamp())
    );
}

/// A group built from a player's attachment owns the track geometry as its
/// only member from birth, so no load has to change its topology.
#[kithara::test]
fn an_attached_group_owns_its_track_geometry_as_its_only_member() {
    let deck = BeatGridId::allocate().expect("grid id");
    let track = BeatGridId::allocate().expect("grid id");
    let executor = SyncExecutor::<Port>::new(track, None, CancelToken::root());
    let group = SyncAttachment::new(
        deck,
        rate(48_000),
        Box::new(TestGrid(asset_grid(track, 960_000, 24_000))),
        executor.execution(),
    )
    .into_group::<TestGroup>();

    let topology = group.topology().expect("an attached group has a topology");
    assert_eq!(topology.group_grid().id(), deck);
    let [member] = topology.members().as_ref() else {
        panic!("the group owns exactly its track grid");
    };
    assert!(
        member.group_topology().is_none(),
        "a track grid is an ordinary member, not a nested group"
    );
    assert_eq!(member.grid().id(), track);
}
