use kithara_sync::{
    ReceiptSink, SyncError, SyncExecutionReject, SyncExecutor, SyncReceipt, SyncReceiptAck,
};
use kithara_test_utils::kithara;
use tracing::debug;

use crate::{
    PlayError,
    resource::StagingRecipe,
    session::{SessionError, SessionHandle},
};

/// Executor of the preparations a player's group issues for its track.
pub(crate) type SyncStaging = SyncExecutor<StagingRecipe>;

impl<S: 'static> ReceiptSink for SessionHandle<S>
where
    Self: Send + Sync,
{
    fn is_bound(&self) -> bool {
        self.dispatcher().is_ok()
    }

    fn acknowledge(&self, receipt: SyncReceipt) -> SyncReceiptAck {
        let answer = self.acknowledge_sync(receipt);
        let delivered = match receipt {
            SyncReceipt::Installed(stamp) => Some((stamp, 0)),
            SyncReceipt::Rejected { stamp, reason } => Some((stamp, reject_code(reason))),
            _ => None,
        };
        if let Some((stamp, rejected)) = delivered {
            kithara::probe_event!(
                sync_receipt_delivered,
                operation = u64::from(stamp.operation()),
                rejected = rejected,
                accepted = u64::from(answer.is_ok())
            );
        }
        if let Err(error) = &answer {
            debug!(%error, "sync: the owner refused an executor receipt");
        }
        match answer {
            Ok(answer) => answer,
            Err(PlayError::Session(
                SessionError::SyncControlBusy | SessionError::Sync(SyncError::OwnerUnavailable),
            ))
            | Err(PlayError::SessionGone { .. }) => SyncReceiptAck::GateFailed,
            Err(_) => SyncReceiptAck::Refused,
        }
    }
}

/// Probe code of a rejection; `0` stands for an installed lane.
const fn reject_code(reason: SyncExecutionReject) -> u64 {
    match reason {
        SyncExecutionReject::Geometry => 1,
        SyncExecutionReject::Late => 2,
        SyncExecutionReject::Capacity => 3,
        SyncExecutionReject::ControlBusy => 6,
        SyncExecutionReject::Cancelled => 4,
        SyncExecutionReject::Media => 5,
        _ => u64::MAX,
    }
}
