#[path = "../system/env.rs"]
pub mod env;
#[path = "../system/support/logging.rs"]
pub mod logging;
#[path = "../system/support/maybe_send.rs"]
pub mod maybe_send;
pub(crate) use crate::loom::sync;
#[path = "flash_loom/thread.rs"]
pub(crate) mod thread;
#[path = "flash/time.rs"]
pub(crate) mod time;
#[path = "flash/tokio.rs"]
pub(crate) mod tokio;
pub(crate) use ::loom::thread_local;

struct Reset;

impl Drop for Reset {
    fn drop(&mut self) {
        crate::flash::reset();
    }
}

pub fn model<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    crate::loom::model(move || {
        crate::flash::reset();
        let _reset = Reset;
        f();
    });
}
