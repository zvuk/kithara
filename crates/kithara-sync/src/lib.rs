#![forbid(unsafe_code)]

//! Recursive synchronization-group ownership and its control-plane protocol.

mod execution;
mod owner;
mod protocol;

pub use execution::{
    ArmPermit, AudioClaim, ClaimError, ControlEnterError, ControlError, ControlGuard,
    ExecutedGroup, PermitCell, PreparedRevocation, ReceiptSink, StagePort, SyncArbiter,
    SyncAttachment, SyncExecution, SyncExecutor, SyncGateBinding, SyncReceiptAck,
};
pub use owner::{GroupState, SyncStaged};
pub use protocol::{
    AlignmentSource, LoadGeneration, ParentFact, ParentGridUpdate, ParentWithdrawal,
    SessionAxisUpdate, SyncAdmission, SyncApplied, SyncCapability, SyncEffect, SyncError,
    SyncExecutionReject, SyncExecutionStamp, SyncGroup, SyncGroupSnapshot, SyncGroupTopologyError,
    SyncIntent, SyncMember, SyncMemberKind, SyncMemberSnapshot, SyncMode, SyncOperation,
    SyncOperationId, SyncPreparation, SyncReceipt, SyncRejected, SyncStatusSnapshot,
    SyncTransition, TopologyOperation, TopologyRevision, TopologyStamp, TransportOperation,
};
