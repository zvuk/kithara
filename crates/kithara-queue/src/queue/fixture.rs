use kithara_play::{PlayWorker, PlayWorkerConfig};
use kithara_test_utils::{
    TestTempDir,
    bufpool::{TestPools, pools},
};
use kithara_worker::Worker;

/// Owns the queue fixture's worker and directory through explicit completion.
#[derive(derive_more::Deref, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct QueueFixture {
    #[deref]
    directory: TestTempDir,
    #[field(get, vis = "pub(super)")]
    worker: PlayWorker<TestPools>,
}

impl QueueFixture {
    pub(super) fn new(worker: &Worker) -> Self {
        Self {
            directory: TestTempDir::new(),
            worker: PlayWorker::new(
                PlayWorkerConfig::builder(pools())
                    .worker(worker.clone())
                    .build(),
            ),
        }
    }

    pub(super) async fn finish(self) {
        #[cfg(not(target_arch = "wasm32"))]
        kithara_platform::tokio::task::spawn_blocking(move || {
            let result = self.worker.shutdown_and_join();
            drop(self);
            result.expect("queue fixture dispatcher completion");
        })
        .await
        .expect("queue fixture completion task");

        #[cfg(target_arch = "wasm32")]
        drop(self);
    }
}
