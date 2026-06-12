//! Typed engine ids. Condvar ids, waiter ids and thread keys live in three
//! distinct key spaces; the newtypes keep them from crossing at compile time.

use std::thread::ThreadId;

use crate::common::thread_id::thread_id_hash;

/// Engine condvar id: one per stateful-primitive latch (Condvar/Notify/channel
/// halves). Allocated only by the engine's `next_condvar_id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::flash) struct CvId(pub(super) u64);

/// Engine waiter id: one per parked entry. Allocated only by the scheduler's
/// `fresh_id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::flash) struct WaiterId(pub(super) u64);

/// Hashed [`ThreadId`] key targeting a parked thread. [`ThreadKey::of`] is the
/// ONLY constructor, so the park side and the unpark side always derive the key
/// through the SAME hasher (parity contract of
/// [`thread_id_hash`](crate::common::thread_id::thread_id_hash)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::flash) struct ThreadKey(u64);

impl ThreadKey {
    pub(in crate::flash) fn of(id: ThreadId) -> Self {
        Self(thread_id_hash(id))
    }
}
