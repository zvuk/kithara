use futures::future::LocalBoxFuture;
use kithara::platform::{
    sync::mpsc::{Sender, channel},
    thread::{keep_worker_alive, spawn_named},
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
type Command = Box<dyn for<'a> FnOnce(&'a mut OfflinePlayer) -> LocalBoxFuture<'a, ()> + Send>;

impl OfflineWorker {
    /// Build the offline player on a Web Worker that then serves commands until
    /// this owner drops.
    pub async fn new<F>(build: F) -> Self
    where
        F: AsyncFnOnce() -> OfflinePlayer + Send + 'static,
    {
        let (commands, rx) = channel::<Command>();
        spawn_named("offline-harness", move || {
            keep_worker_alive();
            task::spawn(async move {
                let mut player = build().await;
                while let Ok(command) = rx.recv_async().await {
                    command(&mut player).await;
                }
            });
        });
        Self { commands }
    }

    /// Apply one operation to the offline player and return its result,
    /// leaving the browser's event loop free while the Worker runs it.
    ///
    /// # Panics
    ///
    /// Panics if the harness Worker is gone.
    pub async fn call<F, R>(&self, f: F) -> R
    where
        F: AsyncFnOnce(&mut OfflinePlayer) -> R + Send + 'static,
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
        reply_rx
            .recv_async()
            .await
            .unwrap_or_else(|error| panic!("offline harness Worker is gone: {error}"))
    }
}
