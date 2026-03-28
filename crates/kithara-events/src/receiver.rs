#![forbid(unsafe_code)]

use kithara_platform::tokio::sync::broadcast;

use crate::{Event, bus::BusMessage};

/// Filtered event receiver.
///
/// Only delivers events whose scope contains this receiver's bus id.
/// Created by [`EventBus::subscribe`](crate::EventBus::subscribe).
pub struct EventReceiver {
    rx: broadcast::Receiver<BusMessage>,
    bus_id: u64,
}

impl EventReceiver {
    pub(crate) fn new(rx: broadcast::Receiver<BusMessage>, bus_id: u64) -> Self {
        Self { rx, bus_id }
    }

    /// Receive the next event in this scope.
    ///
    /// Blocks until an event matching this receiver's scope arrives.
    /// Events from other scopes are silently skipped.
    ///
    /// # Errors
    ///
    /// Returns `RecvError::Lagged(n)` if this receiver fell behind, or
    /// `RecvError::Closed` if the bus was dropped.
    pub async fn recv(&mut self) -> Result<Event, broadcast::error::RecvError> {
        loop {
            let msg = self.rx.recv().await?;
            if msg.scope.contains(self.bus_id) {
                return Ok(msg.event);
            }
        }
    }

    /// Try to receive the next event in this scope without blocking.
    ///
    /// Events from other scopes are silently skipped.
    ///
    /// # Errors
    ///
    /// Returns `TryRecvError::Empty` when no matching event is available,
    /// `TryRecvError::Lagged(n)` if this receiver fell behind, or
    /// `TryRecvError::Closed` if the bus was dropped.
    pub fn try_recv(&mut self) -> Result<Event, broadcast::error::TryRecvError> {
        loop {
            let msg = self.rx.try_recv()?;
            if msg.scope.contains(self.bus_id) {
                return Ok(msg.event);
            }
        }
    }
}

impl std::fmt::Debug for EventReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventReceiver")
            .field("bus_id", &self.bus_id)
            .finish_non_exhaustive()
    }
}
