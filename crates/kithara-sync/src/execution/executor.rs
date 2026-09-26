use std::{collections::VecDeque, mem};

use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    tokio::{
        runtime::Handle,
        task::{spawn_blocking_on, spawn_on},
    },
};
use kithara_warp::{BeatGridId, supports_playback_rate};
use tracing::warn;

use super::{ReceiptSink, StagePort, SyncExecution, command::Execute};
use crate::{
    SyncCapability, SyncEffect, SyncError, SyncExecutionReject, SyncExecutionStamp,
    SyncPreparation, SyncReceipt,
};

struct Loaded<P: StagePort> {
    media: P::Media,
    port: Option<P>,
}

struct Held<P: StagePort> {
    stamp: SyncExecutionStamp,
    media: P::Media,
    cancel: CancelToken,
    runtime: Handle,
    lane: Option<P::Lane>,
}

/// What the owner hears about one preparation: `rejected` is the reason its
/// lane was dropped, `None` for an installed lane.
#[derive(Clone, Copy)]
struct Outcome {
    stamp: SyncExecutionStamp,
    rejected: Option<SyncExecutionReject>,
}

struct State<P: StagePort> {
    loaded: Option<Loaded<P>>,
    held: Option<Held<P>>,
    /// Outcomes not yet handed to the owner, in the order the executor
    /// committed them.
    outcomes: VecDeque<Outcome>,
    /// Whether a drainer is handing `outcomes` to the owner.
    delivering: bool,
}

impl<P: StagePort> State<P> {
    /// Queues `outcome` behind every outcome committed before it; returns
    /// the runtime to start a drainer on when none is running.
    fn commit(&mut self, outcome: Outcome, runtime: &Handle) -> Option<Handle> {
        self.outcomes.push_back(outcome);
        (!mem::replace(&mut self.delivering, true)).then(|| runtime.clone())
    }

    /// Queues the cancellation of a lane the executor dropped.
    fn retire(&mut self, held: &Held<P>) -> Option<Handle> {
        let outcome = Outcome {
            stamp: held.stamp,
            rejected: Some(SyncExecutionReject::Cancelled),
        };
        self.commit(outcome, &held.runtime)
    }

    /// The next outcome the owner still has to hear; an installed lane that
    /// was superseded or dropped before its turn is skipped, since its
    /// cancellation, if any, follows it.
    fn next_outcome(&mut self) -> Option<Outcome> {
        while let Some(outcome) = self.outcomes.pop_front() {
            let current = outcome.rejected.is_some()
                || self
                    .held
                    .as_ref()
                    .is_some_and(|held| held.stamp == outcome.stamp);
            if current {
                return Some(outcome);
            }
        }
        self.delivering = false;
        None
    }
}

struct Shared<P: StagePort> {
    member: BeatGridId,
    /// `None` where the member has no owner to report to, so no preparation
    /// is ever admitted.
    sink: Option<Arc<dyn ReceiptSink>>,
    cancel: CancelToken,
    state: Mutex<State<P>>,
}

/// Carries out the preparations a group issues for one member: at most one
/// staged lane at a time, opened beside the sounding lane and reported to the
/// group owner once its prepared PCM is proven or refused.
///
/// Receipts reach the owner in the order the executor committed them, never
/// under the executor's lock. Clones share one executor.
pub struct SyncExecutor<P: StagePort>(Arc<Shared<P>>);

impl<P: StagePort> Clone for SyncExecutor<P> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<P: StagePort> SyncExecutor<P> {
    /// An executor for the preparations issued for `member`, reporting to
    /// `sink`; its lanes are cancelled with `cancel`.
    #[must_use]
    pub fn new(
        member: BeatGridId,
        sink: Option<Arc<dyn ReceiptSink>>,
        cancel: CancelToken,
    ) -> Self {
        Self(Arc::new(Shared {
            member,
            sink,
            cancel,
            state: Mutex::new(State {
                loaded: None,
                held: None,
                outcomes: VecDeque::new(),
                delivering: false,
            }),
        }))
    }

    /// The media the member now holds, staged through `port` when it can be
    /// staged at all; a preparation staged for another load is dropped and
    /// reported cancelled.
    pub fn load(&self, media: P::Media, port: Option<P>) {
        let mut state = self.0.state.lock();
        let stale = state.held.take_if(|held| held.media != media);
        state.loaded = Some(Loaded { media, port });
        let drain = stale.as_ref().and_then(|held| state.retire(held));
        drop(state);
        cancel_silently(stale);
        self.0.drain(drain);
    }

    /// The loaded media that can currently open a prepared lane.
    #[must_use]
    pub fn stageable_media(&self) -> Option<P::Media> {
        self.0
            .state
            .lock()
            .loaded
            .as_ref()
            .and_then(|loaded| loaded.port.as_ref().map(|_| loaded.media))
    }

    /// The member holds no media any more, or is closing.
    pub fn unload(&self) {
        let mut state = self.0.state.lock();
        state.loaded = None;
        let stale = state.held.take();
        let drain = stale.as_ref().and_then(|held| state.retire(held));
        drop(state);
        cancel_silently(stale);
        self.0.drain(drain);
    }

    /// The command side of this executor, which the member's group follows.
    #[must_use]
    pub fn execution(&self) -> SyncExecution {
        SyncExecution(Arc::clone(&self.0) as Arc<dyn Execute>)
    }
}

fn cancel_silently<P: StagePort>(held: Option<Held<P>>) {
    if let Some(held) = held {
        held.cancel.cancel();
    }
}

impl<P: StagePort> Execute for Shared<P> {
    fn admit(&self, target: BeatGridId) -> Result<(), SyncError> {
        if target != self.member {
            return Ok(());
        }
        let unsupported = SyncError::CapabilityUnavailable {
            capability: SyncCapability::Alignment,
        };
        if !supports_playback_rate() {
            return Err(unsupported);
        }
        if !self.sink.as_ref().is_some_and(|sink| sink.is_bound()) {
            return Err(SyncError::OwnerUnavailable);
        }
        let unstageable = !self
            .state
            .lock()
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.port.is_some());
        if unstageable {
            return Err(unsupported);
        }
        Ok(())
    }

    fn admit_own_member(&self) -> Result<(), SyncError> {
        self.admit(self.member)
    }

    fn follow(self: Arc<Self>, preparation: &SyncPreparation) {
        let stamp = preparation.stamp();
        if stamp.member().grid_id() != self.member {
            return;
        }
        let mut state = self.state.lock();
        if state.held.as_ref().is_some_and(|held| held.stamp == stamp) {
            return;
        }
        let superseded = state.held.take();
        let SyncEffect::Projection { plan, .. } = preparation.effect() else {
            drop(state);
            cancel_silently(superseded);
            return;
        };
        let plan = plan.clone();
        let Some((media, port)) = state.loaded.as_ref().and_then(|loaded| {
            let port = loaded.port.clone()?;
            Some((loaded.media, port))
        }) else {
            drop(state);
            cancel_silently(superseded);
            warn!(?stamp, "sync: no loaded media to stage the preparation on");
            return;
        };
        let cancel = self.cancel.child();
        let runtime = port.runtime().clone();
        state.held = Some(Held {
            stamp,
            media,
            cancel: cancel.clone(),
            runtime: runtime.clone(),
            lane: None,
        });
        drop(state);
        cancel_silently(superseded);
        drop(spawn_on(&runtime, async move {
            let outcome = port.stage(plan, cancel.clone()).await;
            self.settle(stamp, &cancel, outcome);
        }));
    }

    fn withdraw(&self, stamp: SyncExecutionStamp) {
        let withdrawn = self.state.lock().held.take_if(|held| held.stamp == stamp);
        cancel_silently(withdrawn);
    }
}

impl<P: StagePort> Shared<P> {
    /// Reports the outcome of the lane staged for `stamp`, unless that
    /// preparation was superseded, withdrawn, or unloaded meanwhile.
    fn settle(
        self: &Arc<Self>,
        stamp: SyncExecutionStamp,
        cancel: &CancelToken,
        outcome: Result<P::Lane, SyncExecutionReject>,
    ) {
        let mut state = self.state.lock();
        let Some(held) = state
            .held
            .as_mut()
            .filter(|held| held.stamp == stamp && !cancel.is_cancelled())
        else {
            return;
        };
        let runtime = held.runtime.clone();
        let rejected = match outcome {
            Ok(lane) => {
                held.lane = Some(lane);
                None
            }
            Err(reason) => {
                state.held = None;
                Some(reason)
            }
        };
        let drain = state.commit(Outcome { stamp, rejected }, &runtime);
        drop(state);
        self.drain(drain);
    }

    /// Starts handing queued outcomes to the owner off every caller's
    /// thread: the owner may be the very dispatcher that is running this
    /// executor's caller.
    fn drain(self: &Arc<Self>, runtime: Option<Handle>) {
        let (Some(runtime), Some(sink)) = (runtime, self.sink.clone()) else {
            return;
        };
        let shared = Arc::clone(self);
        drop(spawn_blocking_on(&runtime, move || {
            shared.deliver_queued(sink.as_ref());
        }));
    }

    /// Delivers queued outcomes one at a time, in commit order, without
    /// holding the executor across the owner's answer. An installed lane the
    /// owner refuses is dropped, unless a successor replaced it already.
    fn deliver_queued(&self, sink: &dyn ReceiptSink) {
        loop {
            let next = self.state.lock().next_outcome();
            let Some(Outcome { stamp, rejected }) = next else {
                return;
            };
            let receipt = rejected.map_or(SyncReceipt::Installed(stamp), |reason| {
                SyncReceipt::Rejected { stamp, reason }
            });
            if !sink.acknowledge(receipt) && rejected.is_none() {
                self.retire_refused(stamp);
            }
        }
    }

    /// Drops the lane installed for `stamp` once the owner refused it, and
    /// queues its cancellation behind the refusal, so an owner that kept the
    /// preparation pending hears that its lane is gone. A successor that
    /// replaced the lane meanwhile is left alone.
    fn retire_refused(&self, stamp: SyncExecutionStamp) {
        let mut state = self.state.lock();
        let refused = state.held.take_if(|held| held.stamp == stamp);
        if let Some(held) = &refused {
            // The drainer running this is the one that takes the
            // cancellation, so no second one starts.
            let _ = state.retire(held);
        }
        drop(state);
        cancel_silently(refused);
    }
}
