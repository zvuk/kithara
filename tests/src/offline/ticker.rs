use kithara::{
    bufpool::HasPool,
    platform::{
        sync::mpsc::{self, RecvTimeoutError},
        thread::{JoinHandle, spawn_named},
        time::{Duration, Instant, timeout},
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
            Self::tick_until_stopped(queue, stop_receiver, interval);
        });
        Self {
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
    fn tick_until_stopped<S>(queue: QueueControl<S>, stop: mpsc::Receiver<()>, interval: Duration)
    where
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        loop {
            hang_tick!("queue ticker: waiting for stop or next tick".to_owned());
            let mut stopped = false;
            hang_park!(|remaining| {
                stopped = !matches!(
                    stop.recv_timeout(Instant::now() + interval.min(remaining)),
                    Err(RecvTimeoutError::Timeout)
                );
            });
            if stopped {
                break;
            }
            hang_tick!("queue ticker: settling the queue".to_owned());
            if queue.tick().is_err() {
                break;
            }
            hang_reset!();
        }
    }

    /// Whether the thread exited: the queue closed or a tick panicked.
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_some_and(JoinHandle::is_finished)
    }

    /// Stops ticking and joins the thread; `Err` is the message of a tick panic.
    #[kithara_test_utils::kithara::flash(io)]
    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
    pub async fn join(&mut self) -> Result<(), String> {
        drop(self.stop.take());
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        hang_tick!("queue ticker: joining stopped thread".to_owned());
        let joined = timeout(
            __hang_detector.remaining(),
            spawn_blocking(move || thread.join()),
        )
        .await;
        hang_tick!("queue ticker: joining stopped thread".to_owned());
        let joined = joined.expect("queue ticker stops within its liveness budget");
        hang_reset!();
        joined.expect("join the queue ticker").map_err(|payload| {
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
