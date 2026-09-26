use std::fmt;

use kithara_sync::{
    ExecutedGroup, GroupState, ParentFact, SyncAdmission, SyncAttachment, SyncError, SyncGroup,
    SyncGroupSnapshot, SyncMode, SyncOperation, SyncReceipt, SyncRejected, SyncStaged,
    SyncStatusSnapshot, SyncTransition,
};
use kithara_warp::{BeatGrid, BeatGridId, BeatGridSnapshot};

use super::HeldPlayer;

/// One deck as the Host's session group owns it: the synchronization group
/// its player attached, and the deck's Host level.
pub struct PlayerMember {
    group: ExecutedGroup<GroupState<Self>>,
    player: HeldPlayer,
}

impl PlayerMember {
    /// The member of a player that attached `attachment`, the Host holding
    /// `player` of it.
    #[must_use]
    pub(crate) fn new(attachment: SyncAttachment, player: HeldPlayer) -> Self {
        Self {
            group: attachment.into_group(),
            player,
        }
    }

    delegate::delegate! {
        to self.player {
            /// Commits the Host-applied level after its graph batch succeeds.
            pub(crate) fn commit_host_level(&self, level: f32);
            /// Reads the desired Host level used for later graph registration.
            #[must_use]
            pub(crate) fn host_level(&self) -> f32;
        }
    }
}

impl fmt::Debug for PlayerMember {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlayerMember")
            .field("grid_id", &self.id())
            .finish_non_exhaustive()
    }
}

impl BeatGrid for PlayerMember {
    delegate::delegate! {
        to self.group {
            fn id(&self) -> BeatGridId;
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

impl SyncGroup for PlayerMember {
    type NestedGroup = Self;

    delegate::delegate! {
        to self.group {
            fn stage_fact(&self, fact: ParentFact) -> Result<SyncStaged, SyncError>;
            fn status(&self) -> SyncStatusSnapshot;
            fn mode(&self) -> SyncMode;
            fn apply_staged(&mut self, staged: SyncStaged) -> SyncTransition;
            fn topology(&self) -> Result<SyncGroupSnapshot, SyncError>;
            fn transact(
                &mut self,
                operation: SyncOperation<Self>,
            ) -> Result<SyncAdmission, SyncRejected<Self>>;
            fn acknowledge(&mut self, receipt: SyncReceipt) -> Result<SyncStatusSnapshot, SyncError>;
        }
    }
}
