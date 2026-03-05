//! Bridge between kithara broadcast events and FFI observer callbacks.
//!
//! Subscribes to [`PlayerEvent`] channels, translates events into
//! [`PlayerObserver`] callback invocations on a dedicated tokio task.
//! A secondary task polls `position_seconds()` / `duration_seconds()`
//! for periodic time updates.

use std::{
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use kithara::play::{PlayerEvent, PlayerImpl, PlayerStatus};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::observer::PlayerObserver;

/// Forwards player events to an observer on background tasks.
pub(crate) struct EventBridge {
    cancel: CancellationToken,
}

impl EventBridge {
    /// Spawn background tasks that translate player events into observer
    /// callbacks. Returns a bridge handle; dropping it cancels the tasks.
    pub(crate) fn spawn(
        rx: broadcast::Receiver<PlayerEvent>,
        observer: Arc<dyn PlayerObserver>,
        player: Arc<Mutex<PlayerImpl>>,
        cancel: CancellationToken,
    ) -> Self {
        Self::spawn_event_task(rx, Arc::clone(&observer), cancel.clone());
        Self::spawn_time_task(player, observer, cancel.clone());
        Self { cancel }
    }

    /// Task that listens to `PlayerEvent` broadcast channel.
    fn spawn_event_task(
        mut rx: broadcast::Receiver<PlayerEvent>,
        observer: Arc<dyn PlayerObserver>,
        cancel: CancellationToken,
    ) {
        crate::FFI_RUNTIME.spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
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
    }

    /// Task that polls current time and duration at ~10 Hz.
    fn spawn_time_task(
        player: Arc<Mutex<PlayerImpl>>,
        observer: Arc<dyn PlayerObserver>,
        cancel: CancellationToken,
    ) {
        crate::FFI_RUNTIME.spawn(async move {
            let mut last_time: Option<f64> = None;
            let mut last_duration: Option<f64> = None;
            let interval = Duration::from_millis(100);

            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(interval) => {
                        let inner = player.lock().unwrap_or_else(PoisonError::into_inner);
                        let time = inner.position_seconds();
                        let duration = inner.duration_seconds();
                        drop(inner);

                        if let Some(t) = time
                            && last_time.is_none_or(|prev| (prev - t).abs() > 0.01)
                        {
                            observer.on_time_changed(t);
                            last_time = Some(t);
                        }

                        if let Some(d) = duration
                            && last_duration.is_none_or(|prev| (prev - d).abs() > 0.01)
                        {
                            observer.on_duration_changed(d);
                            last_duration = Some(d);
                        }
                    }
                }
            }
        });
    }

    fn dispatch(observer: &Arc<dyn PlayerObserver>, event: &PlayerEvent) {
        match event {
            PlayerEvent::RateChanged { rate } => observer.on_rate_changed(*rate),
            PlayerEvent::StatusChanged { status } => {
                let code = match status {
                    PlayerStatus::ReadyToPlay => 1,
                    PlayerStatus::Failed => 2,
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
