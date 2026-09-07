#[cfg(target_arch = "wasm32")]
use futures::future::LocalBoxFuture;
#[cfg(not(target_arch = "wasm32"))]
use kithara::platform::sync::Mutex;
#[cfg(target_arch = "wasm32")]
use kithara::platform::{
    sync::mpsc::{Sender, channel},
    thread::{keep_worker_alive, spawn_named},
    tokio::task,
};

use super::OfflinePlayer;

/// Owner of the offline player on the thread the product allows it on: the
/// caller's on native, a Web Worker on wasm.
pub struct OfflineWorker {
    #[cfg(not(target_arch = "wasm32"))]
    player: Mutex<OfflinePlayer>,
    #[cfg(target_arch = "wasm32")]
    commands: Sender<Command>,
}

/// One operation applied to the offline player wherever it lives. The future
/// runs on the thread that made it, so it is not `Send`.
#[cfg(target_arch = "wasm32")]
type Command = Box<dyn for<'a> FnOnce(&'a mut OfflinePlayer) -> LocalBoxFuture<'a, ()> + Send>;

#[cfg(not(target_arch = "wasm32"))]
impl OfflineWorker {
    /// Build the offline player on the calling thread.
    pub async fn new<F>(build: F) -> Self
    where
        F: AsyncFnOnce() -> OfflinePlayer + Send + 'static,
    {
        Self {
            player: Mutex::new(build().await),
        }
    }

    /// Apply one operation to the offline player and return its result.
    pub async fn call<F, R>(&self, f: F) -> R
    where
        F: AsyncFnOnce(&mut OfflinePlayer) -> R + Send + 'static,
        R: Send + 'static,
    {
        f(&mut self.player.lock()).await
    }
}

#[cfg(target_arch = "wasm32")]
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
