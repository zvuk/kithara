use kithara_play::{PlayWorker, PlayWorkerConfig};
use kithara_test_utils::{
    TestTempDir,
    bufpool::{TestPools, pools},
};

/// Owns the queue fixture's worker through native completion before directory cleanup.
#[derive(derive_more::Deref, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct QueueFixture {
    #[deref]
    directory: TestTempDir,
    #[field(get, vis = "pub(super)")]
    worker: PlayWorker<TestPools>,
}

impl QueueFixture {
    pub(super) fn new() -> Self {
        Self {
            directory: TestTempDir::new(),
            worker: PlayWorker::new(PlayWorkerConfig::builder(pools()).build()),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for QueueFixture {
    fn drop(&mut self) {
        if let Err(error) = self.worker.shutdown_and_join() {
            if std::thread::panicking() {
                tracing::error!(%error, "queue fixture shutdown failed during unwinding");
            } else {
                panic!("queue fixture shutdown failed: {error}");
            }
        }
    }
}
