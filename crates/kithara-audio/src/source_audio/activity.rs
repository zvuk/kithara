use std::fmt;

use kithara_platform::sync::{Arc, ThreadGate, WaitGate};

/// Edge-triggered readiness signal for one source-audio reader.
/// Clones share one gate, which supports one waiting thread at a time.
#[derive(Clone)]
#[non_exhaustive]
pub struct SourceAudioActivity {
    gate: Arc<ThreadGate>,
}

impl SourceAudioActivity {
    pub(super) fn for_connection() -> Self {
        Self {
            gate: Arc::new(ThreadGate::default()),
        }
    }

    /// Construct an isolated activity edge for tests and probe builds.
    #[cfg(any(test, feature = "probe"))]
    #[must_use]
    pub fn for_test() -> Self {
        Self::for_connection()
    }

    delegate::delegate! {
        to self.gate {
            /// Capture the edge observed before a nonblocking read.
            #[must_use]
            #[call(current)]
            pub fn snapshot(&self) -> u64;
            /// Block the native caller until activity advances past `snapshot`.
            #[cfg(not(target_arch = "wasm32"))]
            pub fn wait(&self, snapshot: u64);
            /// Advance the edge without blocking or allocating; safe on real-time paths.
            pub fn signal(&self);
        }
    }
}

impl fmt::Debug for SourceAudioActivity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceAudioActivity")
            .finish_non_exhaustive()
    }
}

impl PartialEq for SourceAudioActivity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.gate, &other.gate)
    }
}

impl Eq for SourceAudioActivity {}
