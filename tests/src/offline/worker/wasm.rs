use futures::{channel::oneshot, future::LocalBoxFuture};
use kithara::platform::{
    sync::mpsc::{Receiver, Sender, channel},
    thread::{keep_worker_alive, spawn_named},
    time::{Duration, timeout},
    tokio::task,
};

use crate::offline::OfflinePlayer;

/// Owner of the offline player on the thread the product allows it on: a Web
/// Worker.
pub struct OfflineWorker {
    commands: Sender<Command>,
}

/// One operation applied to the offline player wherever it lives. The future
/// runs on the thread that made it, so it is not `Send`.
type Command = Box<dyn for<'a> FnOnce(&'a OfflinePlayer) -> LocalBoxFuture<'a, ()> + Send>;

impl OfflineWorker {
    /// Build the player on a Web Worker before returning its command endpoint.
    pub async fn new<F>(build: F) -> Self
    where
        F: AsyncFnOnce() -> OfflinePlayer + Send + 'static,
    {
        let (commands, rx) = channel::<Command>();
        let (ready, initialized) = oneshot::channel();
        spawn_named("offline-harness", move || {
            keep_worker_alive();
            task::spawn(serve(rx, ready, build));
        });
        initialized
            .await
            .expect("offline harness Worker builds its player");
        Self { commands }
    }

    /// Apply one operation to the offline player and return its result,
    /// leaving the browser's event loop free while the Worker runs it.
    ///
    /// # Panics
    ///
    /// Panics if the harness Worker is gone.
    #[kithara_test_utils::kithara::flash(io)]
    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
    pub async fn call<F, R>(&self, f: F) -> R
    where
        F: AsyncFnOnce(&OfflinePlayer) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (reply_tx, reply_rx) = channel::<R>();
        self.commands
            .send(Box::new(move |player| {
                Box::pin(async move {
                    let _ = reply_tx.send(f(player).await);
                })
            }))
            .unwrap_or_else(|_| panic!("offline harness Worker is gone"));
        hang_tick!("offline Worker awaits its command reply".to_owned());
        let answer = timeout(__hang_detector.remaining(), reply_rx.recv_async()).await;
        hang_tick!("offline Worker awaits its command reply".to_owned());
        hang_reset!();
        answer
            .expect("offline Worker command made no progress")
            .unwrap_or_else(|error| panic!("offline harness Worker is gone: {error}"))
    }
}

#[kithara_test_utils::kithara::hang_watchdog(ctx = String)]
async fn serve<F>(rx: Receiver<Command>, ready: oneshot::Sender<()>, build: F)
where
    F: AsyncFnOnce() -> OfflinePlayer + Send + 'static,
{
    let player = build().await;
    if ready.send(()).is_err() {
        return;
    }
    while let Ok(command) = rx.recv_async().await {
        hang_reset!();
        hang_tick!("offline Worker applies an accepted command".to_owned());
        let completed = {
            let _real_io = kithara::platform::flash::real_io();
            timeout(__hang_detector.remaining(), command(&player)).await
        };
        hang_tick!("offline Worker completes an accepted command".to_owned());
        completed.expect("offline Worker accepted command made no progress");
        hang_reset!();
    }
}
