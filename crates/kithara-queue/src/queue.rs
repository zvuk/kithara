use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicU64, Ordering},
};

use kithara_events::EventReceiver;
use kithara_play::PlayerImpl;
use tokio::sync::broadcast;

use crate::{
    config::QueueConfig,
    events::QueueEvent,
    loader::Loader,
    track::{TrackEntry, TrackId, TrackSource, TrackStatus},
};

const QUEUE_EVENT_CHANNEL_CAPACITY: usize = 128;

/// AVQueuePlayer-analogue orchestration facade.
///
/// C.4 provides construction, event subscription, and `append` that drives
/// the [`Loader`]. C.5 expands the surface with `select` / `advance_to_next`
/// / `return_to_previous` / `tick` / `delegate!`ed player methods.
pub struct Queue {
    player: Arc<PlayerImpl>,
    loader: Arc<Loader>,
    next_id: AtomicU64,
    tracks: Mutex<Vec<TrackEntry>>,
    event_tx: broadcast::Sender<QueueEvent>,
}

impl Queue {
    /// Create a new queue with a fresh [`PlayerImpl`] from `config.player`.
    #[must_use]
    pub fn new(config: QueueConfig) -> Self {
        let QueueConfig {
            player: player_config,
            net,
            store,
            max_concurrent_loads,
            autoplay: _,
        } = config;
        let player = Arc::new(PlayerImpl::new(player_config));
        let (event_tx, _rx) = broadcast::channel(QUEUE_EVENT_CHANNEL_CAPACITY);
        let loader = Arc::new(Loader::new(
            Arc::clone(&player),
            net,
            store,
            max_concurrent_loads,
            event_tx.clone(),
        ));
        Self {
            player,
            loader,
            next_id: AtomicU64::new(0),
            tracks: Mutex::new(Vec::new()),
            event_tx,
        }
    }

    /// Access the underlying [`PlayerImpl`]. Escape hatch for Rust callers;
    /// iOS/Android SDKs should not need this.
    #[must_use]
    pub fn player(&self) -> &Arc<PlayerImpl> {
        &self.player
    }

    /// Subscribe to [`QueueEvent`] notifications. Multiple subscribers are
    /// supported. Messages are dropped for slow subscribers.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<QueueEvent> {
        self.event_tx.subscribe()
    }

    /// Subscribe to the underlying player / audio / hls event stream. C.6
    /// will unify this with `subscribe()` through `Event::Queue`.
    #[must_use]
    pub fn subscribe_player(&self) -> EventReceiver {
        self.player.bus().subscribe()
    }

    /// Number of tracks currently in the queue.
    #[must_use]
    pub fn len(&self) -> usize {
        let guard = self.tracks.lock().unwrap_or_else(PoisonError::into_inner);
        guard.len()
    }

    /// Whether the queue has zero tracks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Append a track to the end of the queue. Loading starts asynchronously.
    pub fn append<S: Into<TrackSource>>(&self, source: S) -> TrackId {
        let id = TrackId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let source = source.into();
        let entry = TrackEntry {
            id,
            name: extract_track_name(&source),
            url: source.uri().map(str::to_string),
            status: TrackStatus::Pending,
        };

        let index = {
            let mut guard = self.tracks.lock().unwrap_or_else(PoisonError::into_inner);
            guard.push(entry);
            guard.len() - 1
        };

        let _ = self.event_tx.send(QueueEvent::TrackAdded { id, index });
        // JoinHandle dropped — the spawned task runs to completion in the
        // background and emits status events via the broadcast channel.
        let _handle = self.loader.spawn_load(id, source);
        id
    }
}

fn extract_track_name(source: &TrackSource) -> String {
    source
        .uri()
        .and_then(|s| s.rsplit('/').next())
        .unwrap_or("Unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use kithara_play::PlayerConfig;

    use super::*;

    fn make_queue() -> Queue {
        let cfg = QueueConfig::new(PlayerConfig::default());
        Queue::new(cfg)
    }

    #[test]
    fn queue_new_constructs_without_panic() {
        let _queue = make_queue();
    }

    #[tokio::test]
    async fn len_is_empty_reflect_append() {
        let queue = make_queue();
        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
        let _ = queue.append("https://example.com/a.mp3");
        let _ = queue.append("https://example.com/b.mp3");
        assert!(!queue.is_empty());
        assert_eq!(queue.len(), 2);
    }

    #[tokio::test]
    async fn append_returns_monotonic_ids_and_emits_track_added() {
        let queue = make_queue();
        let mut rx = queue.subscribe();
        let a = queue.append("https://example.com/a.mp3");
        let b = queue.append("https://example.com/b.mp3");
        assert_ne!(a, b);
        assert!(a.as_u64() < b.as_u64());

        // Drain at least two TrackAdded events.
        let mut seen = 0;
        while let Ok(Ok(ev)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
        {
            if matches!(ev, QueueEvent::TrackAdded { .. }) {
                seen += 1;
                if seen == 2 {
                    break;
                }
            }
        }
        assert_eq!(seen, 2);
    }
}
