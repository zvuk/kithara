use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridSnapshot, BeatGridState, SegmentSet, SessionAnchor,
    SyncAdmission, SyncApplied, SyncError, SyncGroup, SyncGroupSnapshot, SyncOperation,
    SyncRejected, SyncStatusSnapshot,
};

use super::Player;
use crate::{api::TrackId, sync::GroupState};

pub(crate) type PlayerSync = GroupState<PlayerMember>;

/// Host-owned synchronization member that retains one native player.
pub struct PlayerMember {
    inner: Box<dyn Player>,
}

impl PlayerMember {
    /// Erases one concrete player while retaining exclusive ownership.
    #[must_use]
    pub fn new<P: Player>(player: P) -> Self {
        Self {
            inner: Box::new(player),
        }
    }

    delegate::delegate! {
        to self.inner.as_ref() {
            /// Commits the Host-applied level after its graph batch succeeds.
            #[call(set_host_level)]
            pub fn commit_host_level(&self, level: f32);
            /// Reads the desired Host level used for later graph registration.
            #[must_use]
            pub fn host_level(&self) -> f32;
        }
        to self.inner.as_mut() {
            /// Pushes the Host's committed session anchor into the player.
            ///
            /// # Errors
            ///
            /// Returns the player's grid publication error.
            pub fn commit_session_anchor(&mut self, anchor: SessionAnchor) -> Result<(), SyncError>;
            /// Publishes one queued track's asset grid on the deck.
            ///
            /// # Errors
            ///
            /// Returns the deck's rejection.
            pub fn publish_item_grid(
                &mut self,
                item: TrackId,
                segments: SegmentSet,
                state: BeatGridState,
            ) -> Result<SyncAdmission, SyncError>;
            /// Acknowledges the deck's prepared warp map once it is due.
            ///
            /// # Errors
            ///
            /// Returns the deck's acknowledgement error.
            pub fn acknowledge_prepared(&mut self) -> Result<Option<SyncStatusSnapshot>, SyncError>;
        }
    }
}

impl BeatGrid for PlayerMember {
    delegate::delegate! {
        to self.inner.as_ref() {
            fn id(&self) -> BeatGridId;
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

impl SyncGroup for PlayerMember {
    type NestedGroup = Self;

    fn status(&self) -> SyncStatusSnapshot {
        SyncGroup::status(self.inner.as_ref())
    }

    fn topology(&self) -> Result<SyncGroupSnapshot, SyncError> {
        self.inner.topology()
    }

    delegate::delegate! {
        to self.inner.as_mut() {
            fn transact(
                &mut self,
                operation: SyncOperation<Self>,
            ) -> Result<SyncAdmission, SyncRejected<Self>>;
            fn acknowledge(&mut self, applied: SyncApplied) -> Result<SyncStatusSnapshot, SyncError>;
        }
    }
}
