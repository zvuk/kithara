use kithara::{
    bufpool::HasPool,
    platform::{
        sync::mpsc::{self, RecvTimeoutError},
        thread::{JoinHandle, spawn_named},
        time::{Duration, Instant},
        tokio::{runtime::Handle, task::spawn_blocking},
    },
    queue::QueueControl,
};

/// Drives `QueueControl::tick` from a dedicated thread, the way the FFI
/// bridge and the app update loop do, until the queue closes or `stop`.
pub struct QueueTicker {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl QueueTicker {
    /// Spawns the ticker; the thread enters the caller's runtime like the
    /// product app thread, so a tick may spawn loads.
    pub fn spawn<S>(queue: QueueControl<S>, interval: Duration) -> Self
    where
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        let (stop, stop_receiver) = mpsc::channel::<()>();
        let runtime = Handle::current();
        let thread = spawn_named("queue-ticks", move || {
            let _runtime = runtime.enter();
            while let Err(RecvTimeoutError::Timeout) =
                stop_receiver.recv_timeout(Instant::now() + interval)
            {
                if queue.tick().is_err() {
                    break;
                }
            }
        });
        Self {
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    /// Whether the thread exited: the queue closed or a tick panicked.
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_some_and(JoinHandle::is_finished)
    }

    /// Stops ticking and joins the thread; `Err` is the message of a tick panic.
    pub async fn join(&mut self) -> Result<(), String> {
        drop(self.stop.take());
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        spawn_blocking(move || thread.join())
            .await
            .expect("join the queue ticker")
            .map_err(|payload| {
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_else(|| "non-string panic payload".to_owned())
            })
    }

    /// Stops ticking and waits for the thread; a tick panic fails the caller.
    pub async fn stop(&mut self) {
        if let Err(panic) = self.join().await {
            panic!("queue ticker panicked: {panic}");
        }
    }
}

impl Drop for QueueTicker {
    fn drop(&mut self) {
        drop(self.stop.take());
    }
}
