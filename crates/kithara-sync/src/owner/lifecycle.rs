use kithara_warp::{BeatGridId, BeatGridQuery, BeatGridStamp, WarpMapRevision, WarpPlan};
use num_traits::ToPrimitive;

use super::{
    preparation::{Pending, Phase, transition},
    state::GroupState,
};
use crate::{
    SyncApplied, SyncEffect, SyncError, SyncExecutionStamp, SyncGroup, SyncPreparation,
    SyncReceipt, SyncStatusSnapshot, SyncTransition,
};

/// The map one direct member sounds through, as its executor presented it.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get, vis = "pub(super)")]
pub(super) struct Applied {
    /// The receipt that presented the map.
    #[field(get, copy)]
    applied: SyncApplied,
    /// The presented map and its activation.
    #[field(get)]
    plan: WarpPlan,
    /// How far the presented source lies from the map, in output frames.
    #[field(get, copy)]
    phase_error_frames: f64,
    /// The owner grid this unchanged presented map is proven to follow.
    #[field(get, copy)]
    locked_grid: BeatGridStamp,
}

/// What one receipt does to the preparation its member holds.
enum Step {
    Phase(Phase),
    Drop,
    Present(SyncApplied),
}

impl Applied {
    pub(super) fn member(&self) -> BeatGridId {
        self.applied.stamp().member().grid_id()
    }

    pub(super) fn map(&self) -> WarpMapRevision {
        self.plan.activation().revision()
    }

    /// A rejected first Host entry returns to the exact Local trajectory
    /// this map already followed; only that prior stamp may be carried over.
    pub(super) fn restore_local_lock(&mut self, prior: BeatGridStamp, restored: BeatGridStamp) {
        if self.locked_grid == prior {
            self.locked_grid = restored;
        }
    }
}

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    /// Withdraw one member after its last audio processor has left the callback.
    ///
    /// The Host has drained that processor's receipts before calling this. An
    /// Armed preparation therefore signals a broken receipt or quiescence
    /// contract and must not be discarded as an unplayed lane.
    ///
    /// # Errors
    /// Returns an error for an unknown member, an Armed preparation, or a
    /// prior timeline that cannot be restored.
    pub(super) fn withdraw_quiesced_member(
        &mut self,
        member: BeatGridId,
    ) -> Result<SyncTransition, SyncError> {
        if self.direct_grid(member).is_none() {
            return Err(SyncError::MemberNotFound {
                group_id: self.grid.id(),
                member_id: member,
            });
        }
        if self.members.len() != 1 {
            return Err(SyncError::QuiescedMemberNotSoleGrid {
                group_id: self.grid.id(),
                member_id: member,
            });
        }
        if let Some(pending) = self
            .pending
            .iter()
            .find(|pending| pending.member() == member && pending.armed())
        {
            return Err(SyncError::ArmedOperation {
                member_id: member,
                operation: pending.operation(),
            });
        }

        let restored = self
            .before_entry
            .filter(|(operation, _)| {
                self.pending
                    .iter()
                    .any(|pending| pending.member() == member && pending.operation() == *operation)
            })
            .map(|(_, prior)| self.restored_entry_grid(prior).map(|grid| (prior, grid)))
            .transpose()?;
        let mut remaining = self.pending.clone();
        remaining.retain(|pending| pending.member() != member);
        let transition = transition(&self.pending, &remaining);
        self.blocked = None;
        self.pending = remaining;
        self.applied.retain(|lane| lane.member() != member);
        if let Some((prior, grid)) = restored {
            for lane in &mut self.applied {
                lane.restore_local_lock(prior.grid(), grid.stamp());
            }
            self.timeline = prior.timeline();
            self.grid = grid;
            self.before_entry = None;
            self.blocked = None;
        }
        Ok(transition)
    }

    /// Records one executor receipt for a preparation this group issued to a
    /// direct member.
    ///
    /// A receipt advances its preparation one phase at a time: installed,
    /// armed, presented; installing and arming need the member grid the
    /// preparation was placed on to still be current. A rejection drops a
    /// preparation that is not armed and leaves the member on the map it
    /// already sounds through. A presentation makes the preparation's map the
    /// member's applied one, or releases the member for a handoff. Nothing
    /// changes on a refusal.
    pub(super) fn record(&mut self, receipt: SyncReceipt) -> Result<SyncStatusSnapshot, SyncError> {
        let stamp = receipt.stamp();
        let member = stamp.member().grid_id();
        let current = self
            .direct_grid(member)
            .ok_or_else(|| SyncError::MemberNotFound {
                group_id: self.grid.id(),
                member_id: member,
            })?
            .stamp();
        let held = self
            .pending
            .iter()
            .enumerate()
            .find_map(|(index, held)| match held {
                Pending::Prepared {
                    preparation, phase, ..
                } if held.member() == member => Some((index, preparation, *phase)),
                Pending::Prepared { .. } | Pending::Waiting { .. } => None,
            });
        let Some((index, preparation, phase)) = held else {
            return Err(if self.sounds(stamp) {
                SyncError::DuplicateAcknowledgement {
                    operation: stamp.operation(),
                }
            } else {
                SyncError::NoPreparedOperation
            });
        };
        let expected = preparation.stamp();
        let operation = expected.operation();
        if operation != stamp.operation() {
            return Err(if self.sounds(stamp) {
                SyncError::DuplicateAcknowledgement {
                    operation: stamp.operation(),
                }
            } else {
                SyncError::StaleAcknowledgement {
                    expected: operation,
                    given: stamp.operation(),
                }
            });
        }
        if expected != stamp {
            return Err(SyncError::ReceiptMismatch {
                expected: Box::new(expected),
                given: Box::new(stamp),
            });
        }
        let step = match (receipt, phase) {
            (SyncReceipt::Installed(_), Phase::Issued) => Step::Phase(Phase::Installed),
            (SyncReceipt::Armed(_), Phase::Installed) => Step::Phase(Phase::Armed),
            (SyncReceipt::Rejected { .. }, Phase::Issued | Phase::Installed) => Step::Drop,
            (SyncReceipt::Presented(applied), Phase::Armed) => Step::Present(applied),
            (SyncReceipt::Installed(_), Phase::Installed | Phase::Armed)
            | (SyncReceipt::Armed(_), Phase::Armed) => {
                return Err(SyncError::DuplicateAcknowledgement { operation });
            }
            _ => return Err(SyncError::ReceiptOutOfOrder { operation }),
        };
        if matches!(step, Step::Phase(_)) && current != expected.member() {
            return Err(SyncError::StaleGridRevision {
                current,
                given: expected.member(),
            });
        }
        let restoration = match (self.before_entry, &step) {
            (Some((held, prior)), Step::Drop) if held == operation => {
                Some((prior, self.restored_entry_grid(prior)?))
            }
            _ => None,
        };
        match step {
            Step::Phase(next) => {
                if let Some(Pending::Prepared { phase, .. }) = self.pending.get_mut(index) {
                    *phase = next;
                }
            }
            Step::Drop => {
                self.pending.remove(index);
                if let Some((prior, grid)) = restoration {
                    for lane in &mut self.applied {
                        lane.restore_local_lock(prior.grid(), grid.stamp());
                    }
                    self.timeline = prior.timeline();
                    self.grid = grid;
                    self.before_entry = None;
                    self.blocked = None;
                }
            }
            Step::Present(applied) => {
                let lane = presented(preparation, applied)?;
                self.pending.remove(index);
                self.applied.retain(|held| held.member() != member);
                self.applied.extend(lane);
                if self.before_entry.is_some_and(|(held, _)| held == operation) {
                    self.before_entry = None;
                }
            }
        }
        Ok(self.status())
    }

    /// Whether `stamp` names the presentation a member already sounds through.
    fn sounds(&self, stamp: SyncExecutionStamp) -> bool {
        self.applied
            .iter()
            .any(|lane| lane.applied.stamp() == stamp)
    }

    pub(super) fn applied_of(&self, member: BeatGridId) -> Option<&Applied> {
        self.applied.iter().find(|lane| lane.member() == member)
    }
}

/// The lane `applied` leaves the member on once `preparation` sounds: its
/// projected map, or none after a handoff.
fn presented(
    preparation: &SyncPreparation,
    applied: SyncApplied,
) -> Result<Option<Applied>, SyncError> {
    let (warp_map, activation) = preparation.activation();
    let frontier = applied.frontier();
    let mismatch = || SyncError::PresentationMismatch {
        operation: preparation.stamp().operation(),
        expected: warp_map,
        given: frontier,
    };
    if frontier.warp_map() != warp_map || frontier.output() < activation {
        return Err(mismatch());
    }
    let plan = match preparation.effect() {
        SyncEffect::Projection { plan, .. } => plan,
        SyncEffect::Handoff { .. } => return Ok(None),
    };
    let (BeatGridQuery::Resolved(source), BeatGridQuery::Resolved(rate)) = (
        plan.source_at(frontier.output()),
        plan.rate_at(frontier.output()),
    ) else {
        return Err(mismatch());
    };
    let heard = frontier.source().to_f64().ok_or_else(mismatch)?;
    Ok(Some(Applied {
        applied,
        plan: plan.clone(),
        phase_error_frames: (heard - f64::from(source)) / rate,
        locked_grid: preparation.stamp().group(),
    }))
}
