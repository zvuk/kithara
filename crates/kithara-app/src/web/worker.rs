use std::convert::Infallible;

use arc_swap::ArcSwap;
use kithara::{
    host::wasm,
    platform::{
        CancelToken,
        sync::Arc,
        thread::{keep_worker_alive, spawn_named},
        tokio::{
            runtime::Handle,
            sync::{mpsc::UnboundedReceiver, oneshot},
            task,
        },
    },
};

use crate::{
    config::AppConfig,
    document::Config,
    engine::{self, Engine, EngineError, EngineSnapshot, Envelope},
    pools::{AppPools, Pools},
};

/// Builds the engine on a Worker over the page's host and runs it there. The
/// first receiver hears whether the build succeeded before the loop starts,
/// the second closes once the Worker is done with the engine.
pub(super) fn spawn(
    document: Config,
    pools: Pools,
    host: wasm::HostSender<AppPools>,
    snapshots: Arc<ArcSwap<EngineSnapshot>>,
    commands: UnboundedReceiver<Envelope>,
    shutdown: CancelToken,
) -> (
    oneshot::Receiver<Result<(), EngineError>>,
    oneshot::Receiver<Infallible>,
) {
    let (built_tx, built) = oneshot::channel();
    let (stopped_tx, stopped) = oneshot::channel();
    drop(spawn_named("kithara-app-engine", move || {
        keep_worker_alive();
        task::spawn(async move {
            let cancel = shutdown.child();
            let build = move || -> Result<Engine, EngineError> {
                let config = AppConfig::assemble()
                    .document(&document)
                    .pools(pools)
                    .shutdown(shutdown)
                    .runtime(Handle::try_current()?)
                    .call()?;
                engine::build(config, wasm::remote_host(host), snapshots)
            };
            engine::serve(build, built_tx, commands, cancel).await;
            drop(stopped_tx);
        });
    }));
    (built, stopped)
}
