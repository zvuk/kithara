use kithara_platform::sync::Arc;
use kithara_warp::BeatGridId;

use crate::{
    SyncAdmission, SyncCapability, SyncError, SyncExecutionStamp, SyncGroup, SyncIntent,
    SyncOperation, SyncPreparation, SyncTransition,
};

/// The command side of one member's executor: admits the preparations the
/// executor must stage and follows what the member's group issues and
/// withdraws. Clones command one executor.
#[derive(Clone)]
pub struct SyncExecution(pub(super) Arc<dyn Execute>);

impl SyncExecution {
    /// Refuses `operation` when it asks for a preparation the executor has
    /// to stage and cannot.
    pub(super) fn admit<G: SyncGroup>(
        &self,
        operation: &SyncOperation<G>,
    ) -> Result<(), SyncError> {
        match operation {
            SyncOperation::Relocate { target, .. } | SyncOperation::Prepare { target, .. } => {
                self.0.admit(*target)
            }
            SyncOperation::Sync {
                intent: SyncIntent::Enable | SyncIntent::AlignNow,
                ..
            } => self.0.admit_own_member(),
            SyncOperation::Sync {
                intent: SyncIntent::Free,
                ..
            } => Err(SyncError::CapabilityUnavailable {
                capability: SyncCapability::Free,
            }),
            _ => Ok(()),
        }
    }

    /// Follows what the group issued for an admitted operation.
    pub(super) fn follow_admission(&self, admission: &SyncAdmission) {
        match admission {
            SyncAdmission::Prepared(preparation) => {
                Arc::clone(&self.0).follow(preparation);
            }
            SyncAdmission::TopologyChanged { transition, .. }
            | SyncAdmission::StateChanged { transition, .. } => self.follow_transition(transition),
            _ => {}
        }
    }

    /// Follows the preparations one committed change withdrew and issued.
    pub(super) fn follow_transition(&self, transition: &SyncTransition) {
        for stamp in transition.withdrawn() {
            self.0.withdraw(*stamp);
        }
        for preparation in transition.issued() {
            Arc::clone(&self.0).follow(preparation);
        }
    }
}

/// An executor as its member's group commands it, whatever its stage port.
pub(super) trait Execute: Send + Sync {
    /// Refuses a staged preparation for `target` the executor cannot carry
    /// out.
    fn admit(&self, target: BeatGridId) -> Result<(), SyncError>;

    /// Validates the one direct track of this executed deck before its public
    /// Sync operation changes the owning group's mode.
    fn admit_own_member(&self) -> Result<(), SyncError>;

    /// Stages the lane `preparation` asks for, superseding the held one.
    fn follow(self: Arc<Self>, preparation: &SyncPreparation);

    /// Drops the lane staged for `stamp`, if it is the held one.
    fn withdraw(&self, stamp: SyncExecutionStamp);
}
