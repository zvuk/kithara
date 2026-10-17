use kithara_events::{SlotId, TrackId};
use kithara_platform::sync::Arc;

/// Where an item sits in the player's arena and what it renders,
/// alongside the identity the queue gave it.
///
/// A slot is a processor holding an arena of items, not a single item:
/// arming the successor loads it into the *current* slot, and a crossfade
/// promotes it there (`CrossfadeStarted { from: slot, to: slot }`). So
/// `slot` alone does not say which item this is, and `src` names a
/// rendered resource that two queue entries may share. `id` is the only
/// part that answers "which entry"; the other two are what makes a log
/// line readable.
#[derive(Clone, Debug, PartialEq, Eq, Hash, derive_more::Display)]
#[display("{id}@slot{} {src}", slot.value())]
#[non_exhaustive]
pub struct TrackRef {
    /// The rendered resource behind the item. Not an identity: a playlist
    /// may repeat one URL.
    pub src: Arc<str>,
    /// The processor slot the item is loaded into.
    pub slot: SlotId,
    /// The queue's identity for this item, set when it was handed to the
    /// player. The same value the FFI reports as `audioId`.
    pub id: TrackId,
}

impl TrackRef {
    #[must_use]
    pub const fn new(id: TrackId, slot: SlotId, src: Arc<str>) -> Self {
        Self { src, slot, id }
    }
}

/// An item in the player's arena, named together with the role it holds
/// there. Only the player can fill this in.
///
/// The subject of a player event is placed by two answers — which slot it
/// came from, and which item inside that slot — and `src` answers neither:
/// it names a rendered resource, not a queue entry.
///
/// [`TrackRef`] lives *inside* the role rather than beside it, so a
/// consumer has to say which item it is holding before it can use its
/// identity — the omission that once let a background slot's end advance
/// the queue.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ItemRole {
    /// The item the listener is hearing. The only role that drives
    /// auto-advance.
    Leading(TrackRef),
    /// The outgoing half of a crossfade: still inside the current slot,
    /// but the incoming item has already been promoted over it. Its end
    /// is expected and carries no instruction.
    Outgoing(TrackRef),
    /// An item in a slot the phase no longer holds — an orphan draining
    /// the last of its notifications until it is unregistered, while a
    /// different item plays. Acting on it cuts an item still going.
    Background(TrackRef),
}

impl ItemRole {
    /// The queue's identity for this item.
    #[must_use]
    pub const fn id(&self) -> TrackId {
        self.track().id
    }

    /// The item this role is about.
    #[must_use]
    pub const fn track(&self) -> &TrackRef {
        match self {
            Self::Leading(track) | Self::Outgoing(track) | Self::Background(track) => track,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ItemStatus {
    #[default]
    Unknown,
    ReadyToPlay,
    Failed,
}
