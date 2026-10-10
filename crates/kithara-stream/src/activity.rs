use arc_swap::ArcSwap;
use kithara_platform::sync::Arc;

/// Published playback activity read by source loaders.
#[derive(Clone)]
pub struct Activity {
    snapshot: Arc<ArcSwap<bool>>,
}

impl Activity {
    /// Whether the owning chain is currently playing.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        **self.snapshot.load()
    }
}

/// The move-only publisher owned by one audio chain.
pub struct ActivityWriter {
    activity: Activity,
}

impl ActivityWriter {
    /// Create an inactive snapshot and its sole publisher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Clone a read-only loader end.
    #[must_use]
    pub fn reader(&self) -> Activity {
        self.activity.clone()
    }

    /// Publish activity from the chain's owning thread.
    pub fn set_playing(&mut self, playing: bool) {
        if self.activity.is_playing() != playing {
            self.activity.snapshot.store(Arc::new(playing));
        }
    }
}

impl Default for ActivityWriter {
    fn default() -> Self {
        Self {
            activity: Activity {
                snapshot: Arc::new(ArcSwap::from_pointee(false)),
            },
        }
    }
}
