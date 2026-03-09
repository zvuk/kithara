//! Bridge between resource events and item-level observer callbacks.

use std::sync::Arc;

use kithara_events::{AudioEvent, Event, FileEvent, HlsEvent};
use kithara_platform::tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{observer::ItemObserver, types::FfiError};

/// Forwards resource events to an item observer on background tasks.
pub(crate) struct ItemEventBridge {
    cancel: CancellationToken,
}

impl ItemEventBridge {
    /// Spawn a task that translates resource events into item callbacks.
    pub(crate) fn spawn(
        rx: broadcast::Receiver<Event>,
        observer: Arc<dyn ItemObserver>,
        duration_seconds: Option<f64>,
        cancel: CancellationToken,
    ) -> Self {
        observer.on_status_changed(1);
        if let Some(duration) = duration_seconds {
            observer.on_duration_changed(duration);
        }
        Self::spawn_event_task(rx, observer, duration_seconds, cancel.clone());
        Self { cancel }
    }

    fn spawn_event_task(
        mut rx: broadcast::Receiver<Event>,
        observer: Arc<dyn ItemObserver>,
        mut duration_seconds: Option<f64>,
        cancel: CancellationToken,
    ) {
        crate::FFI_RUNTIME.spawn(async move {
            let mut last_buffered = None;
            loop {
                kithara_platform::tokio::select! {
                    () = cancel.cancelled() => break,
                    event = rx.recv() => {
                        match event {
                            Ok(event) => Self::dispatch(
                                &observer,
                                &event,
                                &mut duration_seconds,
                                &mut last_buffered,
                            ),
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
    }

    fn dispatch(
        observer: &Arc<dyn ItemObserver>,
        event: &Event,
        duration_seconds: &mut Option<f64>,
        last_buffered: &mut Option<f64>,
    ) {
        if let Some(duration) = Self::duration_from_event(event)
            && duration_seconds.is_none_or(|current| (current - duration).abs() > 0.01)
        {
            *duration_seconds = Some(duration);
            observer.on_duration_changed(duration);
        }

        if let Some(buffered) = Self::buffered_seconds_from_event(event, *duration_seconds)
            && last_buffered.is_none_or(|current| (current - buffered).abs() > 0.01)
        {
            *last_buffered = Some(buffered);
            observer.on_buffered_duration_changed(buffered);
        }

        if let Some(error) = Self::error_from_event(event) {
            observer.on_status_changed(2);
            observer.on_error(error.observer_code(), error.to_string());
        }
    }

    fn duration_from_event(event: &Event) -> Option<f64> {
        match event {
            Event::Audio(AudioEvent::PlaybackProgress {
                total_ms: Some(total_ms),
                ..
            }) => Some(Self::u64_to_f64(*total_ms)? / 1000.0),
            _ => None,
        }
    }

    fn buffered_seconds_from_event(event: &Event, duration_seconds: Option<f64>) -> Option<f64> {
        let duration_seconds = duration_seconds?;
        match event {
            Event::File(FileEvent::DownloadProgress {
                offset,
                total: Some(total),
            })
            | Event::Hls(HlsEvent::DownloadProgress {
                offset,
                total: Some(total),
            }) => Self::scaled_seconds(*offset, *total, duration_seconds),
            Event::File(FileEvent::DownloadComplete { .. })
            | Event::Hls(HlsEvent::DownloadComplete { .. }) => Some(duration_seconds),
            _ => None,
        }
    }

    fn scaled_seconds(progress: u64, total: u64, duration_seconds: f64) -> Option<f64> {
        if total == 0 {
            return None;
        }
        let ratio = Self::u64_to_f64(progress)? / Self::u64_to_f64(total)?;
        Some((duration_seconds * ratio).clamp(0.0, duration_seconds))
    }

    fn u64_to_f64(value: u64) -> Option<f64> {
        let hi = u32::try_from(value >> 32).ok()?;
        let lo = u32::try_from(value & u64::from(u32::MAX)).ok()?;
        Some(f64::from(hi) * 4_294_967_296.0 + f64::from(lo))
    }

    fn error_from_event(event: &Event) -> Option<FfiError> {
        match event {
            Event::File(FileEvent::DownloadError { error })
            | Event::File(FileEvent::Error { error, .. })
            | Event::Hls(HlsEvent::DownloadError { error })
            | Event::Hls(HlsEvent::Error { error, .. }) => Some(FfiError::ItemFailed {
                reason: error.clone(),
            }),
            _ => None,
        }
    }
}

impl Drop for ItemEventBridge {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::{Event, FileEvent};

    use super::ItemEventBridge;
    use crate::types::FfiError;

    #[kithara::test]
    fn scaled_seconds_clamps_to_duration() {
        let buffered = ItemEventBridge::scaled_seconds(150, 100, 10.0);
        assert_eq!(buffered, Some(10.0));
    }

    #[kithara::test]
    fn scaled_seconds_rejects_zero_total() {
        let buffered = ItemEventBridge::scaled_seconds(10, 0, 10.0);
        assert_eq!(buffered, None);
    }

    #[kithara::test]
    fn file_download_progress_maps_to_buffered_seconds() {
        let event = Event::File(FileEvent::DownloadProgress {
            offset: 50,
            total: Some(100),
        });
        let buffered = ItemEventBridge::buffered_seconds_from_event(&event, Some(12.0));
        assert_eq!(buffered, Some(6.0));
    }

    #[kithara::test]
    fn file_error_maps_to_item_failed() {
        let event = Event::File(FileEvent::DownloadError {
            error: "boom".into(),
        });
        let error = ItemEventBridge::error_from_event(&event);
        assert!(matches!(
            error,
            Some(FfiError::ItemFailed { reason }) if reason == "boom"
        ));
    }
}
