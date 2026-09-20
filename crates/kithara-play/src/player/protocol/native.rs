use kithara_platform::time::Duration;
use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridSnapshot, BeatGridState, SegmentSet, SessionAnchor, SessionFrame,
    SyncAdmission, SyncApplied, SyncError, SyncGroup, SyncGroupSnapshot, SyncOperation,
    SyncRejected, SyncStatusSnapshot, TransportRevision,
};

use super::Player;
use crate::{api::TrackId, sync::GroupState};

#[derive(Clone)]
pub(crate) struct PreparedHostSeek {
    pub(crate) activation_floor: SessionFrame,
    pub(crate) item: TrackId,
    pub(crate) grid_stamp: kithara_warp::BeatGridStamp,
    pub(crate) slot: crate::api::SlotId,
    pub(crate) source_frame: u64,
}

pub(crate) fn duration_for_source(source: u64, sample_rate: u32) -> Duration {
    let sample_rate = u64::from(sample_rate);
    Duration::from_secs(source / sample_rate)
        + Duration::from_nanos(source % sample_rate * 1_000_000_000 / sample_rate)
}

pub(crate) fn seek_outcome(
    target: Duration,
    landed_at: Duration,
    duration: Option<f64>,
) -> kithara_audio::SeekOutcome {
    match duration {
        Some(duration) if landed_at.as_secs_f64() >= duration => {
            kithara_audio::SeekOutcome::PastEof {
                target,
                duration: Duration::from_secs_f64(duration),
            }
        }
        _ => kithara_audio::SeekOutcome::Landed { target, landed_at },
    }
}

/// Chooses how a Host seek reaches the deck.
///
/// An audible deck keeps its old stream until the activation, so only the
/// decoder seek is scheduled. A deck that is not yet audible has no stream to
/// preserve: it waits for the quantized activation and launches at the cue.
pub(crate) const fn host_seek_disposition(
    activation: SessionFrame,
    warp_map: kithara_warp::WarpMapRevision,
    audible: bool,
) -> crate::bridge::ScheduledSeekDisposition {
    if audible {
        crate::bridge::ScheduledSeekDisposition::SeekOnly { activation }
    } else {
        crate::bridge::ScheduledSeekDisposition::PreparedLaunch(
            crate::bridge::PreparedLaunchIdentity {
                activation,
                warp_map,
            },
        )
    }
}

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
            /// Validates and commits a Host-owned deck seek.
            #[doc(hidden)]
            pub fn seek_from_host(
                &mut self,
                seconds: f64,
                transport: TransportRevision,
            ) -> Result<kithara_audio::SeekOutcome, crate::PlayError>;
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
            /// Reconciles the initial host-synced track and prepares queued launches.
            pub fn prepare_sync_launches(
                &mut self,
                output_now: SessionFrame,
            ) -> Result<(), crate::PlayError>;
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
