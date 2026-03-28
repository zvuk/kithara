#![forbid(unsafe_code)]

use delegate::delegate;
use kithara_platform::tokio::sync::broadcast;

use crate::Event;

/// Event receiver for a specific bus scope.
///
/// Created by [`EventBus::subscribe`](crate::EventBus::subscribe).
/// Each scope has its own broadcast channel, so no filtering overhead —
/// the receiver only sees events published to its scope's subtree.
pub struct EventReceiver {
    rx: broadcast::Receiver<Event>,
}

impl EventReceiver {
    pub(crate) fn new(rx: broadcast::Receiver<Event>) -> Self {
        Self { rx }
    }

    delegate! {
        to self.rx {
            /// Receive the next event in this scope.
            ///
            /// # Errors
            ///
            /// Returns `RecvError::Lagged(n)` if this receiver fell behind, or
            /// `RecvError::Closed` if the bus was dropped.
            pub async fn recv(&mut self) -> Result<Event, broadcast::error::RecvError>;

            /// Try to receive the next event without blocking.
            ///
            /// # Errors
            ///
            /// Returns `TryRecvError::Empty` when no event is available,
            /// `TryRecvError::Lagged(n)` if fell behind, or
            /// `TryRecvError::Closed` if the bus was dropped.
            pub fn try_recv(&mut self) -> Result<Event, broadcast::error::TryRecvError>;
        }
    }
}

impl std::fmt::Debug for EventReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventReceiver").finish_non_exhaustive()
    }
}
