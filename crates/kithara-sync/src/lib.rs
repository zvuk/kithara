#![forbid(unsafe_code)]

//! Recursive synchronization-group ownership and its control-plane protocol.

mod execution;
mod owner;
mod protocol;

pub use execution::{
    ExecutedGroup, ReceiptSink, StagePort, SyncAttachment, SyncExecution, SyncExecutor,
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
mod consts;
