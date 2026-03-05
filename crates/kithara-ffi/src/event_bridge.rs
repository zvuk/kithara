//! Bridge between kithara broadcast events and FFI observer callbacks.
//!
//! Subscribes to [`EventBus`] and [`PlayerEvent`] channels, translates
//! events into [`PlayerObserver`] / [`ItemObserver`] callback invocations
//! on a dedicated tokio task.

use std::sync::Arc;

use kithara::play::PlayerEvent;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::observer::PlayerObserver;

/// Forwards player events to an observer on a background task.
pub(crate) struct EventBridge {
    cancel: CancellationToken,
}

impl EventBridge {
    /// Spawn a background task that translates player events into observer
    /// callbacks. Returns a bridge handle; dropping it cancels the task.
    pub(crate) fn spawn(
        mut rx: broadcast::Receiver<PlayerEvent>,
        observer: Arc<dyn PlayerObserver>,
        cancel: CancellationToken,
    ) -> Self {
        let task_cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = task_cancel.cancelled() => break,
                    event = rx.recv() => {
                        match event {
                            Ok(pe) => Self::dispatch(&observer, &pe),
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });

        Self { cancel }
    }

    fn dispatch(observer: &Arc<dyn PlayerObserver>, event: &PlayerEvent) {
        match event {
            PlayerEvent::RateChanged { rate } => observer.on_rate_changed(*rate),
            PlayerEvent::StatusChanged { status } => {
                let code = match status {
                    kithara::play::PlayerStatus::ReadyToPlay => 1,
                    kithara::play::PlayerStatus::Failed => 2,
                    _ => 0,
                };
                observer.on_status_changed(code);
            }
            PlayerEvent::CurrentItemChanged => observer.on_current_item_changed(),
            _ => {}
        }
    }
}

impl Drop for EventBridge {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn event_bridge_is_send() {
        assert_send::<EventBridge>();
    }
}
