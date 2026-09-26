use crate::offline::OfflinePlayer;

/// Owner of the offline player on the thread the product allows it on: the
/// caller's. The player's Host already runs on its own owner thread.
pub struct OfflineWorker {
    player: OfflinePlayer,
}

impl OfflineWorker {
    /// Build the offline player on the calling thread.
    pub async fn new<F>(build: F) -> Self
    where
        F: AsyncFnOnce() -> OfflinePlayer + Send + 'static,
    {
        Self {
            player: build().await,
        }
    }

    /// Apply one operation to the offline player and return its result.
    pub async fn call<F, R>(&self, f: F) -> R
    where
        F: AsyncFnOnce(&OfflinePlayer) -> R + Send + 'static,
        R: Send + 'static,
    {
        f(&self.player).await
    }
}
