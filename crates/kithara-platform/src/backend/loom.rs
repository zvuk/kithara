#[path = "loom/sync.rs"]
pub mod sync;
#[path = "loom/thread.rs"]
pub mod thread;

pub use crate::system::{env, logging, maybe_send, time, tokio};

pub fn model<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    crate::loom::model(f);
}
