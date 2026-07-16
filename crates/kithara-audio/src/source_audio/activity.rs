use kithara_platform::sync::{Arc, ThreadGate, WaitGate};

/// Edge-triggered readiness signal for one source-audio reader.
/// Clones share one gate, which supports one waiting thread at a time.
#[derive(Clone)]
#[non_exhaustive]
pub struct SourceAudioActivity {
    gate: Arc<ThreadGate>,
}

impl SourceAudioActivity {
    pub(crate) fn new() -> Self {
        Self {
            gate: Arc::new(ThreadGate::default()),
        }
    }

    /// Construct an isolated activity edge for tests and probe builds.
    #[cfg(any(test, feature = "probe"))]
    #[must_use]
    pub fn for_test() -> Self {
        Self::new()
    }

    /// Capture the edge observed before a nonblocking read.
    #[must_use]
    pub fn snapshot(&self) -> u64 {
        self.gate.current()
    }

    /// Block the native caller until activity advances past `snapshot`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn wait(&self, snapshot: u64) {
        self.gate.wait(snapshot);
    }

    /// Advance the edge without blocking or allocating; safe on real-time paths.
    pub fn signal(&self) {
        self.gate.signal();
    }
}
