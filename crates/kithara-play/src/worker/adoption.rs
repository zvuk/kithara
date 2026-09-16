use std::sync::atomic::{AtomicU64, Ordering};

use kithara_events::TrackId;
use kithara_platform::sync::{Arc, Mutex};
use kithara_warp::{
    BeatGridSnapshot, LoadGeneration, RateTarget, RegionPlan, SyncOperationId, TransportRevision,
    WarpMapRevision,
};

mod receipt;

pub(crate) use receipt::{
    FreeAdoptionInstalled, FreeAdoptionReceipt, FreeAdoptionRejectReason, FreeAdoptionRejected,
};

/// Immutable control request that the Warp worker alone adopts at an input boundary.
#[derive(Clone, Debug)]
pub(crate) struct FreeAdoptionRequest {
    pub(crate) operation: SyncOperationId,
    pub(crate) warp_map: WarpMapRevision,
    pub(crate) item: TrackId,
    pub(crate) load: LoadGeneration,
    pub(crate) transport: TransportRevision,
    pub(crate) decode_epoch: u64,
    pub(crate) manual_rate: RateTarget,
    pub(crate) owner: BeatGridSnapshot,
    pub(crate) plan: Arc<RegionPlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FreeAdoptionCommit {
    Skipped,
    Rejected,
    Installed,
}

impl FreeAdoptionRequest {
    fn rejected(&self, reason: FreeAdoptionRejectReason) -> FreeAdoptionReceipt {
        FreeAdoptionReceipt::Rejected(FreeAdoptionRejected {
            operation: self.operation,
            warp_map: self.warp_map,
            item: self.item,
            load: self.load,
            transport: self.transport,
            decode_epoch: self.decode_epoch,
            reason,
        })
    }

    fn same_identity(&self, other: &Self) -> bool {
        (
            self.operation,
            self.warp_map,
            self.item,
            self.load,
            self.transport,
            self.decode_epoch,
        ) == (
            other.operation,
            other.warp_map,
            other.item,
            other.load,
            other.transport,
            other.decode_epoch,
        )
    }
}

#[derive(Default)]
struct State {
    closed: bool,
    generation: u64,
    latest: Option<FreeAdoptionRequest>,
    receipt: Option<FreeAdoptionReceipt>,
}

#[derive(Default)]
struct Shared {
    decode_epoch: AtomicU64,
    pending_generation: AtomicU64,
    state: Mutex<State>,
}

/// The sole control capability for one loaded playlist lane.
pub(crate) struct FreeAdoptionControl(Arc<Shared>);

#[derive(Clone)]
pub(crate) struct FreeAdoptionTransition(Arc<Shared>);

#[derive(Clone, Default)]
pub(crate) struct FreeAdoptionWorker(Arc<Shared>);

pub(crate) fn free_adoption() -> (FreeAdoptionControl, FreeAdoptionWorker) {
    let shared = Arc::new(Shared::default());
    (
        FreeAdoptionControl(Arc::clone(&shared)),
        FreeAdoptionWorker(shared),
    )
}

impl FreeAdoptionControl {
    pub(crate) fn transition_endpoint(&self) -> FreeAdoptionTransition {
        FreeAdoptionTransition(Arc::clone(&self.0))
    }
    pub(crate) fn publish(&self, mut request: FreeAdoptionRequest) {
        let mut state = self.0.state.lock();
        if state.closed {
            return;
        }
        request.decode_epoch = self.0.decode_epoch.load(Ordering::Acquire);
        state.generation = state.generation.wrapping_add(1).max(1);
        state.latest = Some(request);
        state.receipt = None;
        self.0
            .pending_generation
            .store(state.generation, Ordering::Release);
    }

    pub(crate) fn receipt(&self) -> Option<FreeAdoptionReceipt> {
        self.0.state.lock().receipt
    }

    pub(crate) fn cancel_pending(&self) {
        let mut state = self.0.state.lock();
        if state.latest.is_some() {
            Self::revoke_locked(&self.0, &mut state, FreeAdoptionRejectReason::Superseded);
        }
    }

    /// Serializes one canonical Player transition with worker commit. The
    /// worker uses `try_lock`, so it defers rather than installing through a
    /// control transition. A rejected transition retains the request.
    pub(crate) fn transition<R>(
        &self,
        operation: SyncOperationId,
        warp_map: WarpMapRevision,
        mutate: impl FnOnce() -> (R, bool),
    ) -> R {
        FreeAdoptionTransition(Arc::clone(&self.0)).transition(operation, warp_map, mutate)
    }
}

impl FreeAdoptionTransition {
    pub(crate) fn transition<R>(
        &self,
        operation: SyncOperationId,
        warp_map: WarpMapRevision,
        mutate: impl FnOnce() -> (R, bool),
    ) -> R {
        {
            let mut state = self.0.state.lock();
            let guards_request = state.latest.as_ref().is_some_and(|request| {
                request.operation == operation && request.warp_map == warp_map
            });
            let (result, revoke) = mutate();
            if guards_request && revoke {
                FreeAdoptionControl::revoke_locked(
                    &self.0,
                    &mut state,
                    FreeAdoptionRejectReason::Superseded,
                );
            }
            drop(state);
            result
        }
    }
}

impl FreeAdoptionControl {
    pub(crate) fn close(&self) {
        let mut state = self.0.state.lock();
        state.closed = true;
        Self::revoke_locked(&self.0, &mut state, FreeAdoptionRejectReason::Closed);
        drop(state);
    }

    fn revoke_locked(shared: &Shared, state: &mut State, reason: FreeAdoptionRejectReason) {
        state.generation = state.generation.wrapping_add(1).max(1);
        state.receipt = state.latest.take().map(|request| request.rejected(reason));
        shared.pending_generation.store(0, Ordering::Release);
    }
}

impl Drop for FreeAdoptionControl {
    fn drop(&mut self) {
        self.close();
    }
}

impl FreeAdoptionWorker {
    /// Publishes a new decoder epoch only when it changes; idle worker steps do
    /// not take the control mutex.
    pub(crate) fn publish_epoch(&self, epoch: u64) {
        if self.0.decode_epoch.swap(epoch, Ordering::AcqRel) == epoch {
            return;
        }
        let mut state = self.0.state.lock();
        if state.latest.is_some() {
            FreeAdoptionControl::revoke_locked(&self.0, &mut state, FreeAdoptionRejectReason::DecodeEpoch);
        } else if state.receipt.is_some_and(|receipt| matches!(receipt, FreeAdoptionReceipt::Installed(value) if value.decode_epoch != epoch)) {
            state.receipt = None;
        }
    }

    pub(crate) fn pending_generation(&self) -> u64 {
        self.0.pending_generation.load(Ordering::Acquire)
    }

    /// Takes a short, non-blocking snapshot for geometry work outside the
    /// control lock.
    pub(crate) fn snapshot(&self, generation: u64, epoch: u64) -> Option<FreeAdoptionRequest> {
        let state = self.0.state.try_lock().ok()?;
        (!state.closed && state.generation == generation)
            .then(|| state.latest.as_ref())
            .flatten()
            .filter(|request| request.decode_epoch == epoch)
            .cloned()
    }

    /// The linearization point. A concurrent supersede changes `generation`;
    /// its request therefore cannot install this already-computed plan.
    pub(crate) fn commit(
        &self,
        generation: u64,
        request: &FreeAdoptionRequest,
        epoch: u64,
        valid_epochs: impl FnOnce() -> bool,
        install: impl FnOnce(),
        installed: FreeAdoptionInstalled,
    ) -> FreeAdoptionCommit {
        let Ok(mut state) = self.0.state.try_lock() else {
            return FreeAdoptionCommit::Skipped;
        };
        if state.closed {
            state.receipt = Some(request.rejected(FreeAdoptionRejectReason::Closed));
            return FreeAdoptionCommit::Rejected;
        }
        if state.generation != generation
            || !state
                .latest
                .as_ref()
                .is_some_and(|current| current.same_identity(request))
        {
            return FreeAdoptionCommit::Skipped;
        }
        if self.0.decode_epoch.load(Ordering::Acquire) != epoch {
            FreeAdoptionControl::revoke_locked(
                &self.0,
                &mut state,
                FreeAdoptionRejectReason::DecodeEpoch,
            );
            return FreeAdoptionCommit::Rejected;
        }
        if !valid_epochs() {
            FreeAdoptionControl::revoke_locked(
                &self.0,
                &mut state,
                FreeAdoptionRejectReason::SeekEpoch,
            );
            return FreeAdoptionCommit::Rejected;
        }
        install();
        state.latest = None;
        state.receipt = Some(FreeAdoptionReceipt::Installed(installed));
        self.0.pending_generation.store(0, Ordering::Release);
        FreeAdoptionCommit::Installed
    }

    pub(crate) fn reject_geometry(&self, generation: u64, request: &FreeAdoptionRequest) -> bool {
        let Ok(mut state) = self.0.state.try_lock() else {
            return false;
        };
        if state.closed
            || state.generation != generation
            || !state
                .latest
                .as_ref()
                .is_some_and(|current| current.same_identity(request))
        {
            return false;
        }
        FreeAdoptionControl::revoke_locked(&self.0, &mut state, FreeAdoptionRejectReason::Geometry);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, num::NonZeroU32};

    use kithara_test_utils::kithara;
    use kithara_warp::{
        BeatGridId, BeatGridRevision, BeatGridSnapshot, GridSegment, RegionPlan, SessionAnchor,
        SessionAxis, SessionBeat, SessionEpoch, SessionFrame,
    };

    use super::*;

    /// The rate the fixture plans count asset frames of.
    fn fixture_rate() -> NonZeroU32 {
        NonZeroU32::new(48_000).expect("invariant: fixture rate is non-zero")
    }

    fn request(item: TrackId) -> FreeAdoptionRequest {
        let epoch = SessionEpoch::new(0);
        let sample_rate = NonZeroU32::new(48_000).expect("fixture sample rate is non-zero");
        let anchor = SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::default(),
            2.0,
            SessionAxis::new(sample_rate, SessionEpoch::new(0)),
        )
        .expect("fixture anchor is valid");
        let owner = BeatGridSnapshot::session(
            BeatGridId::allocate().expect("fixture identity"),
            BeatGridRevision::first(),
            epoch,
            anchor,
            None,
        );
        FreeAdoptionRequest {
            operation: SyncOperationId::first(),
            warp_map: WarpMapRevision::first(),
            item,
            load: LoadGeneration::first(),
            transport: TransportRevision::first(),
            decode_epoch: 0,
            manual_rate: RateTarget::default(),
            owner,
            plan: Arc::new(
                RegionPlan::new(fixture_rate(), vec![GridSegment::new(0, 48_000, 2.0)])
                    .expect("fixture plan is valid"),
            ),
        }
    }

    fn installed(request: &FreeAdoptionRequest) -> FreeAdoptionInstalled {
        FreeAdoptionInstalled {
            operation: request.operation,
            warp_map: request.warp_map,
            item: request.item,
            load: request.load,
            transport: request.transport,
            decode_epoch: request.decode_epoch,
            source: 48_000,
            output: SessionFrame::new(48_000),
            activation_beat: SessionBeat::default(),
        }
    }

    #[kithara::test(native)]
    fn closing_control_prevents_worker_installation() {
        let (control, worker) = free_adoption();
        control.publish(request(TrackId(7)));
        control.close();
        assert_eq!(worker.pending_generation(), 0);
        assert!(worker.snapshot(1, 0).is_none());
        assert!(matches!(
            control.receipt(),
            Some(FreeAdoptionReceipt::Rejected(FreeAdoptionRejected {
                reason: FreeAdoptionRejectReason::Closed,
                ..
            }))
        ));
    }

    #[kithara::test(native)]
    fn dropping_control_revokes_worker_request() {
        let (control, worker) = free_adoption();
        control.publish(request(TrackId(7)));
        drop(control);
        assert_eq!(worker.pending_generation(), 0);
        assert!(worker.snapshot(1, 0).is_none());
    }

    #[kithara::test(native)]
    fn newest_request_is_the_only_request_worker_can_claim() {
        let (control, worker) = free_adoption();
        control.publish(request(TrackId(7)));
        control.publish(request(TrackId(8)));
        let generation = worker.pending_generation();
        let request = worker.snapshot(generation, 0).expect("latest request");
        assert_eq!(request.item, TrackId(8));
        let committed = Cell::new(false);
        assert_eq!(
            worker.commit(
                generation,
                &request,
                0,
                || true,
                || committed.set(true),
                installed(&request),
            ),
            FreeAdoptionCommit::Installed
        );
        assert!(committed.get());
    }

    #[kithara::test(native)]
    fn stale_decode_epoch_never_reaches_installation() {
        let (control, worker) = free_adoption();
        control.publish(request(TrackId(7)));
        worker.publish_epoch(1);
        assert_eq!(worker.pending_generation(), 0);
        assert!(matches!(
            control.receipt(),
            Some(FreeAdoptionReceipt::Rejected(FreeAdoptionRejected {
                reason: FreeAdoptionRejectReason::DecodeEpoch,
                ..
            }))
        ));
    }

    #[kithara::test(native)]
    fn commit_time_decode_epoch_rejection_never_installs() {
        let (control, worker) = free_adoption();
        control.publish(request(TrackId(7)));
        let generation = worker.pending_generation();
        let request = worker.snapshot(generation, 0).expect("pending request");
        worker.0.decode_epoch.store(1, Ordering::Release);
        let installed_flag = Cell::new(false);

        assert_eq!(
            worker.commit(
                generation,
                &request,
                0,
                || true,
                || installed_flag.set(true),
                installed(&request),
            ),
            FreeAdoptionCommit::Rejected
        );
        assert!(!installed_flag.get());
        assert!(matches!(
            control.receipt(),
            Some(FreeAdoptionReceipt::Rejected(FreeAdoptionRejected {
                reason: FreeAdoptionRejectReason::DecodeEpoch,
                ..
            }))
        ));
    }

    #[kithara::test(native)]
    fn supersede_before_commit_never_installs_the_stale_plan() {
        let (control, worker) = free_adoption();
        control.publish(request(TrackId(7)));
        let generation = worker.pending_generation();
        let stale = worker.snapshot(generation, 0).expect("first request");
        control.publish(request(TrackId(8)));
        let ran_install = Cell::new(false);

        assert_eq!(
            worker.commit(
                generation,
                &stale,
                0,
                || true,
                || ran_install.set(true),
                installed(&stale),
            ),
            FreeAdoptionCommit::Skipped
        );
        assert!(!ran_install.get());
        let latest = worker
            .snapshot(worker.pending_generation(), 0)
            .expect("superseding request");
        assert_eq!(latest.item, TrackId(8));
    }

    #[kithara::test(native)]
    fn stale_control_invalidation_preserves_a_newer_request() {
        let (control, worker) = free_adoption();
        let stale = request(TrackId(7));
        let mut current = request(TrackId(8));
        current.warp_map = current
            .warp_map
            .checked_next()
            .expect("fixture revision advances");
        control.publish(stale.clone());
        control.publish(current.clone());

        control.transition(stale.operation, stale.warp_map, || ((), true));

        let request = worker
            .snapshot(worker.pending_generation(), 0)
            .expect("newer request remains pending");
        assert_eq!(request.item, current.item);
        assert_eq!(request.warp_map, current.warp_map);
    }

    #[kithara::test(native)]
    fn worker_snapshot_skips_a_contended_control_transition() {
        let (control, worker) = free_adoption();
        let request = request(TrackId(7));
        control.publish(request.clone());
        let generation = worker.pending_generation();

        control.transition(request.operation, request.warp_map, || {
            assert!(worker.snapshot(generation, 0).is_none());
            ((), false)
        });

        assert!(worker.snapshot(generation, 0).is_some());
    }
}
