use std::num::NonZeroU64;

use crate::BeatGridId;

fn checked_next_revision(revision: NonZeroU64) -> Option<NonZeroU64> {
    revision.get().checked_add(1).and_then(NonZeroU64::new)
}

/// Monotonic revision of one synchronization-group topology.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    derive_more::Display,
    derive_more::Into,
)]
#[display("{_0}")]
#[into(u64)]
#[repr(transparent)]
pub struct TopologyRevision(NonZeroU64);

impl TopologyRevision {
    /// Returns the next owner-assigned revision, or `None` on exhaustion.
    #[must_use]
    pub fn checked_next(self) -> Option<Self> {
        checked_next_revision(self.0).map(Self)
    }

    /// Returns the first revision assigned by a group owner.
    #[must_use]
    pub const fn first() -> Self {
        Self(NonZeroU64::MIN)
    }
}

/// Monotonic identity of one synchronization operation.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    derive_more::Display,
    derive_more::Into,
)]
#[display("{_0}")]
#[into(u64)]
#[repr(transparent)]
pub struct SyncOperationId(NonZeroU64);

impl SyncOperationId {
    /// Returns the next owner-assigned identity, or `None` on exhaustion.
    #[must_use]
    pub fn checked_next(self) -> Option<Self> {
        checked_next_revision(self.0).map(Self)
    }

    /// Returns the first operation identity assigned by a group owner.
    #[must_use]
    pub const fn first() -> Self {
        Self(NonZeroU64::MIN)
    }
}

/// Monotonic revision of one immutable warp map.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    derive_more::Display,
    derive_more::Into,
)]
#[display("{_0}")]
#[into(u64)]
#[repr(transparent)]
pub struct WarpMapRevision(NonZeroU64);

impl WarpMapRevision {
    /// Returns the next owner-assigned revision, or `None` on exhaustion.
    #[must_use]
    pub fn checked_next(self) -> Option<Self> {
        checked_next_revision(self.0).map(Self)
    }

    /// Returns the first revision assigned by a warp-map owner.
    #[must_use]
    pub const fn first() -> Self {
        Self(NonZeroU64::MIN)
    }
}

/// Monotonic identity of one track load into a stable deck.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    derive_more::Display,
    derive_more::Into,
)]
#[display("{_0}")]
#[into(u64)]
#[repr(transparent)]
pub struct LoadGeneration(NonZeroU64);

impl LoadGeneration {
    /// Returns the next owner-assigned generation, or `None` on exhaustion.
    #[must_use]
    pub fn checked_next(self) -> Option<Self> {
        checked_next_revision(self.0).map(Self)
    }

    /// Returns the first generation assigned by a deck owner.
    #[must_use]
    pub const fn first() -> Self {
        Self(NonZeroU64::MIN)
    }
}

/// Monotonic revision of committed session transport state.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    derive_more::Display,
    derive_more::Into,
)]
#[display("{_0}")]
#[into(u64)]
#[repr(transparent)]
pub struct TransportRevision(NonZeroU64);

impl TransportRevision {
    pub(crate) const fn from_raw(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Returns the first committed transport revision.
    #[must_use]
    pub const fn first() -> Self {
        Self(NonZeroU64::MIN)
    }

    /// Returns the next committed revision, or `None` on exhaustion.
    #[must_use]
    pub fn checked_next(self) -> Option<Self> {
        checked_next_revision(self.0).map(Self)
    }
}

/// Identity and immutable revision of one group topology snapshot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct TopologyStamp {
    /// Returns the stable identity of the group grid.
    #[field(get, copy)]
    pub(super) group_id: BeatGridId,
    /// Returns the immutable topology revision.
    #[field(get, copy)]
    revision: TopologyRevision,
}

impl TopologyStamp {
    /// Creates a composite topology stamp.
    #[must_use]
    pub const fn new(group_id: BeatGridId, revision: TopologyRevision) -> Self {
        Self { group_id, revision }
    }
}
