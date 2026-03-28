//! Bridge between resource events and item-level observer callbacks.

use std::sync::Arc;

use kithara_events::{AudioEvent, Event, FileEvent, HlsEvent};
use kithara_platform::{tokio, tokio::sync::broadcast};
use tokio_util::sync::CancellationToken;

use crate::{
    observer::ItemObserver,
    types::{FfiError, FfiItemEvent, FfiItemStatus},
};

/// Threshold for suppressing redundant duration/buffered updates (seconds).
const UPDATE_THRESHOLD: f64 = 0.01;

/// Milliseconds per second.
const MS_PER_SECOND: f64 = 1000.0;

/// 2^32 for splitting u64 into two u32 halves for lossless f64 conversion.
const U32_MAX_PLUS_ONE: f64 = 4_294_967_296.0;

/// Bit shift width for extracting the high 32 bits of a u64.
const U64_HIGH_SHIFT: u32 = 32;

/// Forwards resource events to an item observer on background tasks.
pub(crate) struct ItemEventBridge {
    cancel: CancellationToken,
}

impl ItemEventBridge {
    /// Spawn a task that translates resource events into item callbacks.
    pub(crate) fn spawn(
        rx: kithara_events::EventReceiver,
        observer: Arc<dyn ItemObserver>,
        duration_seconds: Option<f64>,
        cancel: CancellationToken,
    ) -> Self {
        observer.on_event(FfiItemEvent::StatusChanged {
            status: FfiItemStatus::ReadyToPlay,
        });
        if let Some(duration) = duration_seconds {
            observer.on_event(FfiItemEvent::DurationChanged { seconds: duration });
        }
        Self::spawn_event_task(rx, observer, duration_seconds, cancel.clone());
        Self { cancel }
    }

    fn spawn_event_task(
        mut rx: kithara_events::EventReceiver,
        observer: Arc<dyn ItemObserver>,
        mut duration_seconds: Option<f64>,
        cancel: CancellationToken,
    ) {
        crate::FFI_RUNTIME.spawn(async move {
            let mut last_buffered = None;
            loop {
                tokio::select! {
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
            && duration_seconds.is_none_or(|current| (current - duration).abs() > UPDATE_THRESHOLD)
        {
            *duration_seconds = Some(duration);
            observer.on_event(FfiItemEvent::DurationChanged { seconds: duration });
        }

        if let Some(buffered) = Self::buffered_seconds_from_event(event, *duration_seconds)
            && last_buffered.is_none_or(|current| (current - buffered).abs() > UPDATE_THRESHOLD)
        {
            *last_buffered = Some(buffered);
            observer.on_event(FfiItemEvent::BufferedDurationChanged { seconds: buffered });
        }

        Self::dispatch_variant_events(observer, event);

        if let Some(error) = Self::error_from_event(event) {
            observer.on_event(FfiItemEvent::StatusChanged {
                status: FfiItemStatus::Failed,
            });
            observer.on_event(FfiItemEvent::Error {
                error: error.to_string(),
            });
        }
    }

    fn duration_from_event(event: &Event) -> Option<f64> {
        match event {
            Event::Audio(AudioEvent::PlaybackProgress {
                total_ms: Some(total_ms),
                ..
            }) => Some(Self::u64_to_f64(*total_ms)? / MS_PER_SECOND),
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
        let hi = u32::try_from(value >> U64_HIGH_SHIFT).ok()?;
        let lo = u32::try_from(value & u64::from(u32::MAX)).ok()?;
        Some(f64::from(hi) * U32_MAX_PLUS_ONE + f64::from(lo))
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "variant index/count fits u32"
    )]
    fn dispatch_variant_events(observer: &Arc<dyn ItemObserver>, event: &Event) {
        match event {
            Event::Hls(HlsEvent::VariantsDiscovered { variants, .. }) => {
                let ffi_variants = variants
                    .iter()
                    .map(|v| crate::types::FfiVariant {
                        index: v.index as u32,
                        bandwidth_bps: v.bandwidth_bps.unwrap_or(0),
                        name: v.name.clone(),
                    })
                    .collect();
                observer.on_event(FfiItemEvent::VariantsDiscovered {
                    variants: ffi_variants,
                });
            }
            Event::Hls(HlsEvent::AbrModeChanged {
                mode: kithara::abr::AbrMode::Manual(idx),
            }) => {
                observer.on_event(FfiItemEvent::VariantSelected {
                    variant: crate::types::FfiVariant {
                        index: *idx as u32,
                        bandwidth_bps: 0,
                        name: None,
                    },
                });
            }
            Event::Hls(HlsEvent::VariantApplied { to_variant, .. }) => {
                observer.on_event(FfiItemEvent::VariantApplied {
                    variant: crate::types::FfiVariant {
                        index: *to_variant as u32,
                        bandwidth_bps: 0,
                        name: None,
                    },
                });
            }
            _ => {}
        }
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
