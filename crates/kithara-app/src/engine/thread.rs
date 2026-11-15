use kithara::platform::{
    CancelToken,
    thread::{self, JoinHandle},
    tokio::{
        runtime::Handle,
        sync::{mpsc::UnboundedReceiver, oneshot},
    },
};

use super::{Engine, EngineError, Envelope, serve};

pub(crate) struct Driver {
    thread: JoinHandle<()>,
}

impl Driver {
    pub(crate) fn finish(self) -> Result<(), EngineError> {
        let joined = self.thread.join();
        joined.map_err(|_| EngineError::Panicked)
    }
}

pub(crate) fn spawn(
    runtime: &Handle,
    build: impl FnOnce() -> Result<Engine, EngineError> + Send + 'static,
    commands: UnboundedReceiver<Envelope>,
    cancel: CancelToken,
) -> Result<Driver, EngineError> {
    let driven = runtime.clone();
    let (built_tx, built) = oneshot::channel();
    let thread = thread::spawn_named("kithara-app-engine", move || {
        driven.block_on(serve(build, built_tx, commands, cancel));
    });
    match runtime
        .block_on(built)
        .unwrap_or(Err(EngineError::Panicked))
    {
        Ok(()) => Ok(Driver { thread }),
        Err(error) => {
            thread.join().map_err(|_| EngineError::Panicked)?;
            Err(error)
        }
    }
}
