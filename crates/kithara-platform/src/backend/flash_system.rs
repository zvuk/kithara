#[path = "../system/env.rs"]
pub mod env;
#[path = "../system/support/logging.rs"]
pub mod logging;
#[path = "../system/support/maybe_send.rs"]
pub mod maybe_send;
#[path = "flash_system/sync.rs"]
pub(crate) mod sync;
#[path = "flash_system/thread.rs"]
pub(crate) mod thread;
#[path = "flash/time.rs"]
pub(crate) mod time;
#[path = "flash/tokio.rs"]
pub(crate) mod tokio;
pub(crate) use std::thread_local;

#[inline]
pub fn model<F>(f: F)
where
    F: FnOnce(),
{
    f();
}
