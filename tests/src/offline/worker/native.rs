use kithara::platform::sync::Mutex;

use crate::offline::OfflinePlayer;

/// Owner of the offline player on the thread the product allows it on: the
/// caller's.
pub struct OfflineWorker {
    player: Mutex<OfflinePlayer>,
}

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
