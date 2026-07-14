use std::{backtrace::Backtrace, fmt};

use crate::flash::ids::ThreadKey;

/// Snapshot identity of an OS thread, captured for the hang dump. `key` matches
/// the engine's [`ThreadKey`] so a thread blocked acquiring a lock here can be
/// correlated with the same thread parked on an engine waiter.
#[derive(Clone, Debug, derive_more::Display)]
#[display("{name} [{id} {key:?}]", name = self.name.as_deref().unwrap_or("<unnamed>"), id = self.id, key = self.key)]
pub(in crate::flash) struct ThreadDesc {
    pub(in crate::flash) key: ThreadKey,
    name: Option<String>,
    id: String,
}

impl ThreadDesc {
    pub(in crate::flash) fn current() -> Self {
        let t = std::thread::current();
        Self {
            key: ThreadKey::of(t.id()),
            name: t.name().map(str::to_owned),
            id: format!("{:?}", t.id()),
        }
    }
}

/// Context of the thread taking the dump: its identity plus, when
/// `KITHARA_FLASH_SYNC_BT=1`, its full backtrace (where this thread is stuck).
pub(in crate::flash) fn current_thread_context() -> String {
    let desc = ThreadDesc::current();
    if super::bt_enabled() {
        format!(
            "current thread: {desc}\nbacktrace:\n{bt}",
            bt = Backtrace::force_capture(),
        )
    } else {
        format!("current thread: {desc}  (set KITHARA_FLASH_SYNC_BT=1 for a backtrace)")
    }
}
