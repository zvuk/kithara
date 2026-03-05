//! Bridge between kithara broadcast events and FFI observer callbacks.
//!
//! Subscribes to [`PlayerEvent`] channels, translates events into
//! [`PlayerObserver`] callback invocations on a dedicated tokio task.
//! A secondary **OS thread** polls `position_seconds()` / `duration_seconds()`
//! for periodic time updates — avoiding blocking `Mutex::lock_sync()` inside async.

use std::sync::Arc;

use kithara::play::{PlayerEvent, PlayerImpl, PlayerStatus};
use kithara_platform::{Duration, JoinHandle, Mutex, sleep, spawn, tokio::sync::broadcast};
use tokio_util::sync::CancellationToken;

use crate::observer::PlayerObserver;

/// Forwards player events to an observer on background tasks.
pub(crate) struct EventBridge {
    cancel: CancellationToken,
    time_thread: Option<JoinHandle<()>>,
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
        let time_thread = Self::spawn_time_thread(player, observer, cancel.clone());
        Self {
            cancel,
            time_thread: Some(time_thread),
        }
    }

    /// Task that listens to `PlayerEvent` broadcast channel.
    fn spawn_event_task(
        mut rx: broadcast::Receiver<PlayerEvent>,
        observer: Arc<dyn PlayerObserver>,
        cancel: CancellationToken,
    ) {
        crate::FFI_RUNTIME.spawn(async move {
            loop {
                kithara_platform::tokio::select! {
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

    /// Dedicated OS thread that polls current time and duration at ~10 Hz.
    ///
    /// Uses a plain thread instead of an async task to avoid blocking the
    /// single-threaded tokio runtime with `Mutex::lock_sync()`.
    fn spawn_time_thread(
        player: Arc<Mutex<PlayerImpl>>,
        observer: Arc<dyn PlayerObserver>,
        cancel: CancellationToken,
    ) -> JoinHandle<()> {
        spawn(move || {
            let interval = Duration::from_millis(100);
            let mut last_time: Option<f64> = None;
            let mut last_duration: Option<f64> = None;

            while !cancel.is_cancelled() {
                sleep(interval);

                let inner = player.lock_sync();
                let time = inner.position_seconds();
                let duration = inner.duration_seconds();
                drop(inner);

                match time {
                    Some(t) if last_time.is_none_or(|prev| (prev - t).abs() > 0.01) => {
                        observer.on_time_changed(t);
                        last_time = Some(t);
                    }
                    None if last_time.is_some() => {
                        last_time = None;
                    }
                    _ => {}
                }

                match duration {
                    Some(d) if last_duration.is_none_or(|prev| (prev - d).abs() > 0.01) => {
                        observer.on_duration_changed(d);
                        last_duration = Some(d);
                    }
                    None if last_duration.is_some() => {
                        last_duration = None;
                    }
                    _ => {}
                }
            }
        })
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
        if let Some(handle) = self.time_thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[kithara::test]
    fn event_bridge_is_send() {
        assert_send::<EventBridge>();
    }
}
