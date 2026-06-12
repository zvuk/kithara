//! Typed engine ids. Condvar ids, waiter ids and thread keys live in three
//! distinct key spaces; the newtypes keep them from crossing at compile time.
//! [`CvId`] and [`WaiterId`] are DEFINED inside `system/` with a private inner
//! field — only the engine's `Registry` mints them — and re-exported here for
//! the wrappers that store keys and pass them back.

use std::thread::ThreadId;

pub(in crate::flash) use super::system::{CvId, WaiterId};
use crate::common::thread_id::thread_id_hash;

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
