use std::sync::Arc;

use kithara::abr::AbrMode;
use kithara_events::{AbrEvent, AudioEvent, DownloaderEvent, Event, FileEvent, HlsEvent};
use kithara_platform::{CancelToken, Mutex, tokio, tokio::sync::broadcast};

use crate::{
    item::ItemView,
    observer::ItemObserver,
    types::{FfiError, FfiItemEvent, FfiItemStatus, FfiTimeRange},
};

pub(crate) struct ItemEventBridge {
    cancel: CancelToken,
}

impl ItemEventBridge {
    /// Milliseconds per second.
    const MS_PER_SECOND: f64 = 1000.0;

    /// 2^32 for splitting u64 into two u32 halves for lossless f64 conversion.
    const U32_MAX_PLUS_ONE: f64 = 4_294_967_296.0;

    /// Bit shift width for extracting the high 32 bits of a u64.
    const U64_HIGH_SHIFT: u32 = 32;

    /// Threshold for suppressing redundant duration/buffered updates (seconds).
    const UPDATE_THRESHOLD: f64 = 0.01;

    fn buffered_seconds_from_event(event: &Event, duration_seconds: Option<f64>) -> Option<f64> {
        let duration_seconds = duration_seconds?;
        match event {
            Event::File(FileEvent::ReadProgress {
                position,
                total: Some(total),
            })
            | Event::Hls(HlsEvent::ReadProgress {
                position,
                total: Some(total),
            }) => Self::scaled_seconds(*position, *total, duration_seconds),
            Event::Downloader(DownloaderEvent::RequestCompleted { .. }) => Some(duration_seconds),
            _ => None,
        }
    }

    fn dispatch(
        observer: &Arc<dyn ItemObserver>,
        event: &Event,
        duration_seconds: &mut Option<f64>,
        last_buffered: &mut Option<f64>,
        variants: &mut Vec<crate::types::FfiVariant>,
        state: &Arc<Mutex<ItemView>>,
    ) {
        if let Some(duration) = Self::duration_from_event(event)
            && duration_seconds
                .is_none_or(|current| (current - duration).abs() > Self::UPDATE_THRESHOLD)
        {
            *duration_seconds = Some(duration);
            state.lock_sync().resolve_duration(duration);
            observer.on_event(FfiItemEvent::DurationChanged { seconds: duration });
        }

        if let Some(buffered) = Self::buffered_seconds_from_event(event, *duration_seconds)
            && last_buffered
                .is_none_or(|current| (current - buffered).abs() > Self::UPDATE_THRESHOLD)
        {
            *last_buffered = Some(buffered);
            let ranges = if buffered > 0.0 {
                vec![FfiTimeRange {
                    start_seconds: 0.0,
                    duration_seconds: buffered,
                }]
            } else {
                Vec::new()
            };
            observer.on_event(FfiItemEvent::LoadedRangesChanged { ranges });
        }

        Self::dispatch_variant_events(observer, event, variants);

        if let Some(error) = Self::error_from_event(event) {
            state.lock_sync().mark_failed();
            observer.on_event(FfiItemEvent::StatusChanged {
                status: FfiItemStatus::Failed,
            });
            observer.on_event(FfiItemEvent::Error {
                error: error.to_string(),
            });
        }
    }

    fn dispatch_variant_events(
        observer: &Arc<dyn ItemObserver>,
        event: &Event,
        variants: &mut Vec<crate::types::FfiVariant>,
    ) {
        match event {
            Event::Abr(AbrEvent::VariantsRegistered {
                variants: v,
                initial,
            }) => {
                let ffi_variants: Vec<crate::types::FfiVariant> = v
                    .iter()
                    .filter_map(|vi| {
                        let Ok(index) = u32::try_from(vi.variant_index.get()) else {
                            tracing::error!(
                                idx = vi.variant_index.get(),
                                "BUG: HLS variant index exceeds u32::MAX, dropped from FFI list"
                            );
                            return None;
                        };
                        Some(crate::types::FfiVariant {
                            index,
                            bandwidth_bps: vi.bandwidth_bps.unwrap_or(0),
                            name: vi.name.clone(),
                        })
                    })
                    .collect();
                variants.clone_from(&ffi_variants);
                observer.on_event(FfiItemEvent::VariantsDiscovered {
                    variants: ffi_variants,
                });
                let Ok(initial_u32) = u32::try_from(initial.get()) else {
                    tracing::error!(
                        idx = initial.get(),
                        "BUG: initial HLS variant index exceeds u32::MAX, skipping initial VariantApplied"
                    );
                    return;
                };
                if let Some(initial) = variants.iter().find(|v| v.index == initial_u32) {
                    observer.on_event(FfiItemEvent::VariantApplied {
                        variant: initial.clone(),
                    });
                }
            }
            Event::Abr(AbrEvent::ModeChanged {
                mode: AbrMode::Manual(idx),
            }) => {
                let Ok(idx_u32) = u32::try_from(idx.get()) else {
                    tracing::error!(
                        idx = idx.get(),
                        "BUG: manual variant index exceeds u32::MAX, skipping VariantSelected"
                    );
                    return;
                };
                let variant = variants
                    .iter()
                    .find(|v| v.index == idx_u32)
                    .cloned()
                    .unwrap_or(crate::types::FfiVariant {
                        index: idx_u32,
                        bandwidth_bps: 0,
                        name: None,
                    });
                observer.on_event(FfiItemEvent::VariantSelected { variant });
            }
            Event::Abr(AbrEvent::VariantApplied { to, .. }) => {
                let Ok(idx_u32) = u32::try_from(to.get()) else {
                    tracing::error!(
                        idx = to.get(),
                        "BUG: applied variant index exceeds u32::MAX, skipping VariantApplied"
                    );
                    return;
                };
                let variant = variants
                    .iter()
                    .find(|v| v.index == idx_u32)
                    .cloned()
                    .unwrap_or(crate::types::FfiVariant {
                        index: idx_u32,
                        bandwidth_bps: 0,
                        name: None,
                    });
                observer.on_event(FfiItemEvent::VariantApplied { variant });
            }
            _ => {}
        }
    }

    fn duration_from_event(event: &Event) -> Option<f64> {
        match event {
            Event::Audio(AudioEvent::PlaybackProgress {
                total_ms: Some(total_ms),
                ..
            }) => Some(Self::u64_to_f64(*total_ms)? / Self::MS_PER_SECOND),
            _ => None,
        }
    }

    fn error_from_event(event: &Event) -> Option<FfiError> {
        match event {
            Event::File(FileEvent::Error { error }) => Some(FfiError::ItemFailed {
                reason: error.to_string(),
            }),
            Event::Hls(HlsEvent::Error { error }) => Some(FfiError::ItemFailed {
                reason: error.to_string(),
            }),
            Event::Downloader(DownloaderEvent::RequestFailed { error, .. }) => {
                Some(FfiError::ItemFailed {
                    reason: error.to_string(),
                })
            }
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

    /// Spawn a task that translates resource events into item callbacks
    /// and refreshes the shared [`ItemView`] cache backing the item's
    /// synchronous getters (`duration_sec`, `is_live_stream`, …).
    pub(crate) fn spawn(
        rx: kithara_events::EventReceiver,
        observer: Arc<dyn ItemObserver>,
        duration_seconds: Option<f64>,
        state: Arc<Mutex<ItemView>>,
        cancel: CancelToken,
    ) -> Self {
        if let Some(duration) = duration_seconds {
            state.lock_sync().resolve_duration(duration);
            observer.on_event(FfiItemEvent::DurationChanged { seconds: duration });
        }
        Self::spawn_event_task(rx, observer, duration_seconds, state, cancel.clone());
        Self { cancel }
    }

    fn spawn_event_task(
        mut rx: kithara_events::EventReceiver,
        observer: Arc<dyn ItemObserver>,
        mut duration_seconds: Option<f64>,
        state: Arc<Mutex<ItemView>>,
        cancel: CancelToken,
    ) {
        crate::FFI_RUNTIME.spawn(async move {
            let mut last_buffered = None;
            let mut variants: Vec<crate::types::FfiVariant> = Vec::new();
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
                                &mut variants,
                                &state,
                            ),
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
    }

    fn u64_to_f64(value: u64) -> Option<f64> {
        let hi = u32::try_from(value >> Self::U64_HIGH_SHIFT).ok()?;
        let lo = u32::try_from(value & u64::from(u32::MAX)).ok()?;
        Some(f64::from(hi) * Self::U32_MAX_PLUS_ONE + f64::from(lo))
    }
}

impl Drop for ItemEventBridge {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::{Event, FileError, FileEvent};

    use super::ItemEventBridge;
    use crate::types::FfiError;

    #[kithara::test]
    #[case::clamps_to_duration(150, 100, 10.0, Some(10.0))]
    #[case::rejects_zero_total(10, 0, 10.0, None)]
    fn scaled_seconds_handles_boundary_inputs(
        #[case] position: u64,
        #[case] total: u64,
        #[case] duration: f64,
        #[case] expected: Option<f64>,
    ) {
        let buffered = ItemEventBridge::scaled_seconds(position, total, duration);
        assert_eq!(buffered, expected);
    }

    #[kithara::test]
    fn file_read_progress_maps_to_buffered_seconds() {
        let event = Event::File(FileEvent::ReadProgress {
            position: 50,
            total: Some(100),
        });
        let buffered = ItemEventBridge::buffered_seconds_from_event(&event, Some(12.0));
        assert_eq!(buffered, Some(6.0));
    }

    #[kithara::test]
    fn file_error_maps_to_item_failed() {
        let event = Event::File(FileEvent::Error {
            error: FileError::Io("boom".into()),
        });
        let error = ItemEventBridge::error_from_event(&event);
        assert!(matches!(
            error,
            Some(FfiError::ItemFailed { reason }) if reason == "io: boom"
        ));
    }
}
