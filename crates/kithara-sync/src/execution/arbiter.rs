use kithara_platform::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, WallInstant},
};
use kithara_warp::BeatGridId;

use crate::SyncExecutionStamp;

mod consts {
    pub(super) const OPEN: u64 = 0;
    pub(super) const CONTROL: u64 = 1;
    pub(super) const AUDIO_CLAIMED: u64 = 2;
    pub(super) const CLOSED: u64 = 3;
}

/// Arbitrates one session's owner mutations and audio claims without owning
/// the synchronization ledger. The Host owns this value for the session.
pub struct SyncArbiter {
    phase: AtomicU64,
    owner_waiting: AtomicBool,
}

impl Default for SyncArbiter {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncArbiter {
    /// Create an open session arbiter before attaching any member.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: AtomicU64::new(consts::OPEN),
            owner_waiting: AtomicBool::new(false),
        }
    }

    /// Enter the owner phase with one attempt. A busy owner may retry off RT.
    #[must_use]
    pub fn try_control(&self) -> Option<ControlGuard<'_>> {
        self.phase
            .compare_exchange(
                consts::OPEN,
                consts::CONTROL,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| ControlGuard { arbiter: self })
    }

    /// Enter the sole Host dispatcher phase after an in-progress audio claim.
    /// A bounded wait lets a shutdown command run if a callback abandoned its
    /// claim. Waiting blocks later RT claims so new candidates cannot starve
    /// an owner receipt or member retirement.
    ///
    /// # Errors
    /// Returns `Busy` when the callback has not finished its claim within the
    /// owner wait budget; the caller may retry off RT. Returns `Closed` after
    /// callback quiescence has tombstoned this session.
    pub fn enter_host_control(&self) -> Result<ControlGuard<'_>, ControlEnterError> {
        let deadline = WallInstant::now() + Duration::from_millis(18);
        self.owner_waiting.store(true, Ordering::Release);
        loop {
            if self.phase.load(Ordering::Acquire) == consts::CLOSED {
                self.owner_waiting.store(false, Ordering::Release);
                return Err(ControlEnterError::Closed);
            }
            if let Some(control) = self.try_control() {
                self.owner_waiting.store(false, Ordering::Release);
                return Ok(control);
            }
            if WallInstant::now() >= deadline {
                self.owner_waiting.store(false, Ordering::Release);
                return Err(ControlEnterError::Busy);
            }
            thread::yield_now();
        }
    }

    /// Claim a ready audio candidate with one gate CAS and no wait or lock.
    /// Capacity, first-span validity, and the activation frame are the RT
    /// caller's preconditions before this method is invoked.
    ///
    /// # Errors
    ///
    /// Returns the precise busy, closed, wrong-member, or stale-permit refusal
    /// before an audio change.
    pub fn try_claim(
        &self,
        permit: &ArmPermit,
        cell: &PermitCell,
    ) -> Result<AudioClaim<'_>, ClaimError> {
        if permit.stamp.member().grid_id() != cell.member {
            return Err(ClaimError::WrongMember);
        }
        if cell.retired.load(Ordering::Acquire) {
            return Err(ClaimError::CellRetired);
        }
        if self.owner_waiting.load(Ordering::Acquire) {
            return Err(ClaimError::Busy);
        }
        if !permit.matches(cell) {
            return Err(ClaimError::StalePermit);
        }
        self.phase
            .compare_exchange(
                consts::OPEN,
                consts::AUDIO_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|phase| {
                if phase == consts::CLOSED {
                    ClaimError::Closed
                } else {
                    ClaimError::Busy
                }
            })?;

        // Control could have entered and left between the first check and
        // the CAS. No owner mutation can run after this check until release.
        let result = if cell.retired.load(Ordering::Acquire) {
            Err(ClaimError::CellRetired)
        } else if self.owner_waiting.load(Ordering::Acquire) {
            Err(ClaimError::Busy)
        } else if !permit.matches(cell) {
            Err(ClaimError::StalePermit)
        } else {
            Ok(AudioClaim { arbiter: self })
        };
        if result.is_err() {
            let _ = self.phase.compare_exchange(
                consts::AUDIO_CLAIMED,
                consts::OPEN,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        result
    }

    /// Tombstone the session after its audio callback and owner work have
    /// quiesced. This also closes a claim abandoned before both receipts.
    pub fn close_quiescent(&self) {
        self.phase.store(consts::CLOSED, Ordering::Release);
    }
}

/// An owner entry failed before any group state was changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ControlEnterError {
    /// An audio callback did not release its claim within the owner budget.
    #[error("audio claim did not complete within the owner wait budget")]
    Busy,
    /// Callback quiescence closed this session's gate.
    #[error("session claim gate is closed")]
    Closed,
}

/// The Host's stable gate and member cell carried to one attached player.
#[derive(Clone)]
pub struct SyncGateBinding {
    arbiter: Arc<SyncArbiter>,
    cell: Arc<PermitCell>,
}

impl SyncGateBinding {
    /// Bind the Host-owned arbiter to the cell of one attached member.
    #[must_use]
    pub fn new(arbiter: Arc<SyncArbiter>, cell: Arc<PermitCell>) -> Self {
        Self { arbiter, cell }
    }

    /// Session arbiter shared with the audio callback.
    #[must_use]
    pub fn arbiter(&self) -> &SyncArbiter {
        &self.arbiter
    }

    /// Stable cell for this player's track member.
    #[must_use]
    pub fn cell(&self) -> &PermitCell {
        &self.cell
    }

    /// Whether this exact owner-issued permit still belongs to this member.
    /// The audio claim repeats this check under the gate before changing PCM.
    #[must_use]
    pub fn still_permits(&self, permit: &ArmPermit) -> bool {
        permit.stamp.member().grid_id() == self.cell.member
            && !self.cell.retired.load(Ordering::Acquire)
            && permit.matches(&self.cell)
    }
}

/// Stable per-member claim identity. The Host allocates and retains the cell;
/// an RT ticket carries a reference to that same allocation.
pub struct PermitCell {
    member: BeatGridId,
    permit_revision: AtomicU64,
    retired: AtomicBool,
}

impl PermitCell {
    /// Allocate one cell for the lifetime of `member`'s Host registration.
    #[must_use]
    pub const fn new(member: BeatGridId) -> Self {
        Self {
            member,
            permit_revision: AtomicU64::new(1),
            retired: AtomicBool::new(false),
        }
    }

    /// Identity this cell was allocated for.
    #[must_use]
    pub const fn member(&self) -> BeatGridId {
        self.member
    }
}

/// Owner-minted authority for one exact installed preparation and source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmPermit {
    stamp: SyncExecutionStamp,
    permit_revision: u64,
}

impl ArmPermit {
    /// The preparation admitted by the owner in the Installed reply.
    #[must_use]
    pub const fn stamp(&self) -> SyncExecutionStamp {
        self.stamp
    }

    fn matches(&self, cell: &PermitCell) -> bool {
        self.permit_revision == cell.permit_revision.load(Ordering::Acquire)
    }
}

/// A checked revocation held under the same Control phase as the owner
/// transaction. Preflight every affected cell before mutating owner state.
#[must_use]
pub struct PreparedRevocation<'cell, 'guard, 'arbiter> {
    cell: &'cell PermitCell,
    _guard: &'guard ControlGuard<'arbiter>,
    next_revision: u64,
}

impl PreparedRevocation<'_, '_, '_> {
    /// Publish the preflighted revocation after the owner transition commits.
    pub fn revoke(self) {
        self.cell
            .permit_revision
            .store(self.next_revision, Ordering::Release);
    }
}

/// Refusal of an owner-side permit or source mutation operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ControlError {
    #[error("permit cell belongs to another member")]
    WrongMember,
    #[error("member cell is retired")]
    CellRetired,
    #[error("member revision space is exhausted")]
    RevisionExhausted,
}

/// Refusal of an RT claim before any audio state changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClaimError {
    #[error("session commit gate is busy")]
    Busy,
    #[error("session is closed")]
    Closed,
    #[error("permit cell belongs to another member")]
    WrongMember,
    #[error("member cell is retired")]
    CellRetired,
    #[error("arm permit is stale")]
    StalePermit,
}

/// Short owner phase. Host drains available RT receipts before transacting the
/// synchronization group under this guard.
#[must_use]
pub struct ControlGuard<'a> {
    arbiter: &'a SyncArbiter,
}

impl<'arbiter> ControlGuard<'arbiter> {
    /// Mint the permit in the same owner reply that accepts Installed.
    ///
    /// # Errors
    ///
    /// Returns an error when the cell belongs to another member or is retired.
    pub fn mint_permit(
        &self,
        cell: &PermitCell,
        stamp: SyncExecutionStamp,
    ) -> Result<ArmPermit, ControlError> {
        if stamp.member().grid_id() != cell.member {
            return Err(ControlError::WrongMember);
        }
        if cell.retired.load(Ordering::Acquire) {
            return Err(ControlError::CellRetired);
        }
        Ok(ArmPermit {
            stamp,
            permit_revision: cell.permit_revision.load(Ordering::Acquire),
        })
    }

    /// Check one affected member before the owner changes group state. The
    /// caller must preflight every distinct affected cell exactly once before
    /// that transaction, then consume every token without interleaving other
    /// mutations of those cells.
    ///
    /// # Errors
    ///
    /// Returns an error when this cell is retired or its revision is spent.
    pub fn preflight_revoke<'cell, 'guard>(
        &'guard self,
        cell: &'cell PermitCell,
    ) -> Result<PreparedRevocation<'cell, 'guard, 'arbiter>, ControlError> {
        if cell.retired.load(Ordering::Acquire) {
            return Err(ControlError::CellRetired);
        }
        let next = cell
            .permit_revision
            .load(Ordering::Relaxed)
            .checked_add(1)
            .ok_or(ControlError::RevisionExhausted)?;
        Ok(PreparedRevocation {
            cell,
            _guard: self,
            next_revision: next,
        })
    }

    /// Permanently retire one member cell after that player's RT users have
    /// quiesced. Other member cells remain available in this session.
    ///
    /// # Errors
    ///
    /// Returns an error if the cell is already retired.
    pub fn retire_cell(&self, cell: &PermitCell) -> Result<(), ControlError> {
        if cell.retired.load(Ordering::Relaxed) {
            return Err(ControlError::CellRetired);
        }
        cell.retired.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for ControlGuard<'_> {
    fn drop(&mut self) {
        let _ = self.arbiter.phase.compare_exchange(
            consts::CONTROL,
            consts::OPEN,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// The sole RT owner of the session gate until both claim receipts are queued.
/// Dropping this value without finishing leaves the gate claimed, so no owner
/// may proceed on an audio change it has not heard about.
#[must_use]
pub struct AudioClaim<'a> {
    arbiter: &'a SyncArbiter,
}

impl AudioClaim<'_> {
    /// Complete only after a nonempty prefetched span was consumed and both
    /// Armed and Presented were written into pre-reserved receipt slots.
    pub fn finish_after_receipts(self) {
        let _ = self.arbiter.phase.compare_exchange(
            consts::AUDIO_CLAIMED,
            consts::OPEN,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_signal::TransportRevision;
    use kithara_test_utils::kithara;
    use kithara_warp::{BeatGridId, BeatGridRevision, BeatGridStamp};

    use super::*;
    use crate::{LoadGeneration, SyncOperationId, TopologyRevision, TopologyStamp};

    fn stamp(member: BeatGridId) -> SyncExecutionStamp {
        let group = BeatGridId::allocate().expect("group id");
        SyncExecutionStamp::new(
            SyncOperationId::first(),
            BeatGridStamp::new(member, BeatGridRevision::first()),
            BeatGridStamp::new(group, BeatGridRevision::first()),
            TopologyStamp::new(group, TopologyRevision::first()),
            LoadGeneration::first(),
            TransportRevision::first(),
        )
    }

    fn cell() -> PermitCell {
        PermitCell::new(BeatGridId::allocate().expect("member id"))
    }

    #[kithara::test]
    fn control_and_audio_claims_have_one_winner() {
        let arbiter = SyncArbiter::new();
        let a = cell();
        let b = cell();
        let control = arbiter.try_control().expect("owner enters");
        let a_permit = control
            .mint_permit(&a, stamp(a.member()))
            .expect("a permit");
        let b_permit = control
            .mint_permit(&b, stamp(b.member()))
            .expect("b permit");
        assert!(arbiter.try_control().is_none());
        assert!(matches!(
            arbiter.try_claim(&a_permit, &a),
            Err(ClaimError::Busy)
        ));
        drop(control);

        let b_claim = arbiter.try_claim(&b_permit, &b).expect("b claims");
        assert!(arbiter.try_control().is_none());
        assert!(matches!(
            arbiter.try_claim(&a_permit, &a),
            Err(ClaimError::Busy)
        ));
        b_claim.finish_after_receipts();
        let _control = arbiter.try_control().expect("owner follows b");
    }

    #[kithara::test]
    fn revocation_is_affected_member_only() {
        let arbiter = SyncArbiter::new();
        let a = cell();
        let b = cell();
        let control = arbiter.try_control().expect("owner enters");
        let a_permit = control
            .mint_permit(&a, stamp(a.member()))
            .expect("a permit");
        let b_permit = control
            .mint_permit(&b, stamp(b.member()))
            .expect("b permit");
        control.preflight_revoke(&a).expect("preflight a").revoke();
        drop(control);

        assert!(matches!(
            arbiter.try_claim(&a_permit, &a),
            Err(ClaimError::StalePermit)
        ));
        arbiter
            .try_claim(&b_permit, &b)
            .expect("unaffected b remains valid")
            .finish_after_receipts();
    }

    #[kithara::test]
    fn retired_member_cannot_rearm_while_another_member_claims() {
        let arbiter = SyncArbiter::new();
        let a = cell();
        let b = cell();
        let control = arbiter.try_control().expect("owner enters");
        let a_stamp = stamp(a.member());
        let a_permit = control.mint_permit(&a, a_stamp).expect("a permit");
        let b_permit = control
            .mint_permit(&b, stamp(b.member()))
            .expect("b permit");
        control.retire_cell(&a).expect("a is quiescent");
        assert!(matches!(
            control.mint_permit(&a, a_stamp),
            Err(ControlError::CellRetired)
        ));
        drop(control);
        assert!(matches!(
            arbiter.try_claim(&a_permit, &a),
            Err(ClaimError::CellRetired)
        ));
        arbiter
            .try_claim(&b_permit, &b)
            .expect("b still claims")
            .finish_after_receipts();
    }

    #[kithara::test]
    fn claim_releases_owner_only_after_explicit_receipt_completion() {
        let arbiter = SyncArbiter::new();
        let cell = cell();
        let control = arbiter.try_control().expect("owner enters");
        let permit = control
            .mint_permit(&cell, stamp(cell.member()))
            .expect("permit");
        drop(control);
        let claim = arbiter.try_claim(&permit, &cell).expect("audio claims");
        assert!(arbiter.try_control().is_none());
        claim.finish_after_receipts();
        let _control = arbiter.try_control().expect("owner enters after receipts");
    }

    #[kithara::test]
    fn revision_exhaustion_is_rejected_before_revocation() {
        let arbiter = SyncArbiter::new();
        let permit_spent = cell();
        let control = arbiter.try_control().expect("owner enters");
        permit_spent
            .permit_revision
            .store(u64::MAX, Ordering::Release);
        assert!(matches!(
            control.preflight_revoke(&permit_spent),
            Err(ControlError::RevisionExhausted)
        ));
    }

    #[kithara::test]
    fn abandoned_claim_can_be_tombstoned_after_audio_quiesces() {
        let arbiter = SyncArbiter::new();
        let cell = cell();
        let control = arbiter.try_control().expect("owner enters");
        let permit = control
            .mint_permit(&cell, stamp(cell.member()))
            .expect("permit");
        drop(control);
        let claim = arbiter.try_claim(&permit, &cell).expect("audio claims");
        drop(claim);
        assert!(arbiter.try_control().is_none());
        assert!(matches!(
            arbiter.enter_host_control(),
            Err(ControlEnterError::Busy)
        ));
        arbiter.close_quiescent();
        assert!(arbiter.try_control().is_none());
        assert!(matches!(
            arbiter.enter_host_control(),
            Err(ControlEnterError::Closed)
        ));
        assert!(matches!(
            arbiter.try_claim(&permit, &cell),
            Err(ClaimError::Closed)
        ));
    }
}
