use kithara_platform::{CancelToken, maybe_send::MaybeSendFuture, tokio::runtime::Handle};
use kithara_warp::WarpPlan;

use crate::{ArmPermit, SyncExecutionReject, SyncReceipt};

/// Opens the staged lanes of one load of a member's media.
pub trait StagePort: Clone + Send + 'static {
    /// Identifies one load of the member's media.
    type Media: Copy + Eq + Send + 'static;
    /// Holds a staged lane's prepared PCM until the preparation it serves
    /// ends.
    type Lane: Send + 'static;

    /// The runtime staging and receipt delivery run on.
    fn runtime(&self) -> &Handle;

    /// Opens a lane that plays `plan` and resolves once its prepared PCM is
    /// proven, or with the reason the lane cannot be held.
    fn stage(
        self,
        plan: WarpPlan,
        cancel: CancelToken,
    ) -> impl MaybeSendFuture<Output = Result<Self::Lane, SyncExecutionReject>> + 'static;

    /// Transfer one exact installed lane and its cancel custody to the
    /// member's audio path before its activation can be claimed.
    ///
    /// # Errors
    ///
    /// Returns a pre-Armed rejection if the load, ready span, or bounded
    /// handoff capacity is no longer available. The lane is retired off RT.
    fn handoff(
        self,
        media: Self::Media,
        lane: Self::Lane,
        cancel: CancelToken,
        permit: ArmPermit,
    ) -> Result<(), SyncExecutionReject>;
}

/// The owner's answer to one executor receipt. An Installed answer carries
/// the exact permit minted in the same owner acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncReceiptAck {
    /// A non-installation receipt was recorded.
    Recorded,
    /// The installed preparation was recorded and may enter the audio path.
    Installed(ArmPermit),
    /// The owner refused the receipt.
    Refused,
    /// The owner gate or session failed before recording the receipt.
    GateFailed,
}

/// The group owner an executor reports the outcome of each staged lane to.
pub trait ReceiptSink: Send + Sync {
    /// Whether an owner is bound to take receipts at all.
    fn is_bound(&self) -> bool;

    /// Hands one receipt to the owner and blocks until its exact answer.
    fn acknowledge(&self, receipt: SyncReceipt) -> SyncReceiptAck;
}
