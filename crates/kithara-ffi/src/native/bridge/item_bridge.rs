use kithara::abr::AbrMode;
use kithara_events::{
    AbrEvent, AudioEvent, DecoderEvent, DownloaderEvent, Envelope, Event, FileEvent, HlsEvent,
};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    tokio,
    tokio::sync::broadcast,
};

use crate::{
    item::ItemView,
    observer::ItemObserver,
    types::{FfiError, FfiItemEvent, FfiItemStatus},
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

    fn dispatch(
        observer: &Arc<dyn ItemObserver>,
        event: &Event,
        duration_seconds: &mut Option<f64>,
        variants: &mut Vec<crate::types::FfiVariant>,
        state: &Arc<Mutex<ItemView>>,
    ) {
        if let Some(duration) = Self::duration_from_event(event)
            && duration_seconds
                .is_none_or(|current| (current - duration).abs() > Self::UPDATE_THRESHOLD)
        {
            *duration_seconds = Some(duration);
            state.lock().resolve_duration(duration);
            observer.on_event(FfiItemEvent::DurationChanged { seconds: duration });
        }

        Self::dispatch_variant_events(observer, event, variants);

        if let Event::Decoder(decoder_event) = event
            && let Some(event) = decoder_event_to_ffi(decoder_event)
        {
            observer.on_event(event);
        }

        if let Event::Audio(audio_event) = event
            && let Some(event) = audio_event_to_ffi(audio_event)
        {
            observer.on_event(event);
        }

        if let Event::Hls(hls_event) = event
            && let Some(event) = hls_event_to_ffi(hls_event)
        {
            observer.on_event(event);
        }

        if let Event::Downloader(downloader_event) = event
            && let Some(event) = downloader_event_to_ffi(downloader_event)
        {
            observer.on_event(event);
        }

        if let Event::File(file_event) = event
            && let Some(event) = file_event_to_ffi(file_event)
        {
            observer.on_event(event);
        }

        if let Some(error) = Self::error_from_event(event) {
            state.lock().mark_failed();
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
            state.lock().resolve_duration(duration);
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
            let mut variants: Vec<crate::types::FfiVariant> = Vec::new();
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    event = rx.recv() => {
                        match event {
                            Ok(Envelope { event, .. }) => Self::dispatch(
                                &observer,
                                &event,
                                &mut duration_seconds,
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

fn decoder_event_to_ffi(event: &DecoderEvent) -> Option<FfiItemEvent> {
    match event {
        DecoderEvent::DecoderChanged {
            backend,
            codec,
            container,
            sample_rate,
            channels,
            bit_depth,
            bitrate,
            epoch,
            cause,
            variant,
            base_offset,
            duration,
            gapless,
        } => {
            let (gapless_leading, gapless_trailing, has_gapless) = gapless
                .map(|span| (span.leading_frames, span.trailing_frames, true))
                .unwrap_or((0, 0, false));
            Some(FfiItemEvent::DecoderChanged {
                backend: (*backend).into(),
                codec: codec.map(Into::into),
                container: container.map(Into::into),
                sample_rate: *sample_rate,
                channels: *channels,
                bit_depth: *bit_depth,
                bitrate: *bitrate,
                epoch: *epoch,
                cause: (*cause).into(),
                variant: *variant,
                base_offset: *base_offset,
                duration_seconds: duration.map(crate::types::duration_to_seconds),
                gapless_leading,
                gapless_trailing,
                has_gapless,
            })
        }
        DecoderEvent::DecodeError {
            class,
            kind,
            codec,
            detail,
        } => Some(FfiItemEvent::DecodeError {
            class: (*class).into(),
            kind: (*kind).into(),
            codec: codec.map(Into::into),
            detail: (*detail).to_string(),
        }),
        DecoderEvent::GaplessResolved {
            leading_frames,
            trailing_frames,
            domain,
            codec,
            sample_rate,
        } => Some(FfiItemEvent::GaplessResolved {
            leading_frames: *leading_frames,
            trailing_frames: *trailing_frames,
            domain: (*domain).into(),
            codec: codec.map(Into::into),
            sample_rate: *sample_rate,
        }),
        DecoderEvent::ResamplerConfigured {
            backend,
            input_rate,
            output_rate,
            channels,
            bypassed,
        } => Some(FfiItemEvent::ResamplerConfigured {
            backend: (*backend).into(),
            input_rate: *input_rate,
            output_rate: *output_rate,
            channels: *channels,
            bypassed: *bypassed,
        }),
        _ => None,
    }
}

fn audio_event_to_ffi(event: &AudioEvent) -> Option<FfiItemEvent> {
    match event {
        AudioEvent::FormatDetected { spec } => Some(FfiItemEvent::AudioFormatDetected {
            channels: spec.channels,
            sample_rate: spec.sample_rate,
        }),
        AudioEvent::FormatChanged { old, new } => Some(FfiItemEvent::AudioFormatChanged {
            old_channels: old.channels,
            old_sample_rate: old.sample_rate,
            new_channels: new.channels,
            new_sample_rate: new.sample_rate,
        }),
        AudioEvent::SeekComplete {
            position,
            seek_epoch,
        } => Some(FfiItemEvent::SeekComplete {
            position_seconds: crate::types::duration_to_seconds(*position),
            epoch: *seek_epoch,
        }),
        AudioEvent::SeekRejected { epoch, target } => Some(FfiItemEvent::SeekRejected {
            epoch: *epoch,
            target_seconds: crate::types::duration_to_seconds(*target),
        }),
        AudioEvent::DecoderReady {
            base_offset,
            variant,
        } => Some(FfiItemEvent::DecoderReady {
            base_offset: *base_offset,
            variant: *variant,
        }),
        AudioEvent::TrackFailed {
            failure,
            seek_epoch,
        } => Some(FfiItemEvent::TrackFailed {
            reason: failure.clone().into(),
            epoch: *seek_epoch,
        }),
        AudioEvent::UnderrunStarted {
            position_ms,
            seek_epoch,
        } => Some(FfiItemEvent::UnderrunStarted {
            position_ms: *position_ms,
            epoch: *seek_epoch,
        }),
        AudioEvent::UnderrunEnded {
            position_ms,
            seek_epoch,
        } => Some(FfiItemEvent::UnderrunEnded {
            position_ms: *position_ms,
            epoch: *seek_epoch,
        }),
        AudioEvent::BufferHealth {
            buffered_ms,
            decoded_frontier_ms,
            seek_epoch,
        } => Some(FfiItemEvent::BufferHealth {
            buffered_ms: *buffered_ms,
            decoded_frontier_ms: *decoded_frontier_ms,
            epoch: *seek_epoch,
        }),
        AudioEvent::EngineLoad {
            load,
            ms_per_chunk,
            realtime_factor,
        } => Some(FfiItemEvent::EngineLoad {
            load: *load,
            ms_per_chunk: *ms_per_chunk,
            realtime_factor: *realtime_factor,
        }),
        AudioEvent::PlaybackResamplerConfigured {
            backend,
            host_sample_rate,
            source_sample_rate,
            active,
        } => Some(FfiItemEvent::PlaybackResamplerConfigured {
            backend: (*backend).into(),
            host_sample_rate: *host_sample_rate,
            source_sample_rate: *source_sample_rate,
            active: *active,
        }),
        AudioEvent::EndOfStream => Some(FfiItemEvent::DidReachEnd),
        _ => None,
    }
}

fn hls_event_to_ffi(event: &HlsEvent) -> Option<FfiItemEvent> {
    match event {
        HlsEvent::VariantSwitchFenced {
            from_variant,
            to_variant,
            cross_codec,
        } => Some(FfiItemEvent::HlsVariantSwitchFenced {
            from_variant: u32::try_from(*from_variant).unwrap_or(u32::MAX),
            to_variant: u32::try_from(*to_variant).unwrap_or(u32::MAX),
            cross_codec: *cross_codec,
        }),
        HlsEvent::VariantSwitchAcked {
            variant,
            generation,
        } => Some(FfiItemEvent::HlsVariantSwitchAcked {
            variant: u32::try_from(*variant).unwrap_or(u32::MAX),
            generation: *generation,
        }),
        HlsEvent::CacheComplete { total_bytes } => Some(FfiItemEvent::HlsCacheComplete {
            total_bytes: *total_bytes,
        }),
        _ => None,
    }
}

fn downloader_event_to_ffi(event: &DownloaderEvent) -> Option<FfiItemEvent> {
    match event {
        DownloaderEvent::RequestStarted {
            request_id,
            wait_in_queue,
        } => Some(FfiItemEvent::DownloadStarted {
            request_id: request_id.get(),
            wait_in_queue_seconds: crate::types::duration_to_seconds(*wait_in_queue),
        }),
        DownloaderEvent::LoadSlow {
            request_id,
            elapsed,
        } => Some(FfiItemEvent::DownloadSlow {
            request_id: request_id.get(),
            elapsed_seconds: crate::types::duration_to_seconds(*elapsed),
        }),
        DownloaderEvent::RequestCompleted {
            request_id,
            bytes_transferred,
            duration,
            bandwidth_bps,
        } => Some(FfiItemEvent::DownloadCompleted {
            request_id: request_id.get(),
            bytes_transferred: *bytes_transferred,
            duration_seconds: crate::types::duration_to_seconds(*duration),
            bandwidth_bps: *bandwidth_bps,
        }),
        DownloaderEvent::RequestRetrying {
            request_id,
            attempt,
            max_retries,
            error,
            backoff,
        } => Some(FfiItemEvent::DownloadRetrying {
            request_id: request_id.get(),
            attempt: *attempt,
            max_retries: *max_retries,
            error: error.to_string(),
            backoff_seconds: crate::types::duration_to_seconds(*backoff),
        }),
        DownloaderEvent::BodyStalled {
            request_id,
            consumed,
            expected,
            stall,
        } => Some(FfiItemEvent::DownloadBodyStalled {
            request_id: request_id.get(),
            consumed: *consumed,
            expected: *expected,
            stall_seconds: crate::types::duration_to_seconds(*stall),
        }),
        DownloaderEvent::BodyResumed {
            request_id,
            resume_number,
            from_offset,
            honoured_range,
        } => Some(FfiItemEvent::DownloadBodyResumed {
            request_id: request_id.get(),
            resume_number: *resume_number,
            from_offset: *from_offset,
            honoured_range: *honoured_range,
        }),
        DownloaderEvent::RetryExhausted {
            request_id,
            max_retries,
            consumed,
            error,
        } => Some(FfiItemEvent::DownloadRetryExhausted {
            request_id: request_id.get(),
            max_retries: *max_retries,
            consumed: *consumed,
            error: error.to_string(),
        }),
        DownloaderEvent::FirstByte {
            request_id,
            ttfb,
            status,
            partial,
        } => Some(FfiItemEvent::DownloadFirstByte {
            request_id: request_id.get(),
            ttfb_seconds: crate::types::duration_to_seconds(*ttfb),
            status: *status,
            partial: *partial,
        }),
        DownloaderEvent::RequestCancelled {
            request_id,
            reason,
            bytes_transferred,
        } => Some(FfiItemEvent::DownloadCancelled {
            request_id: request_id.get(),
            reason: (*reason).into(),
            bytes_transferred: *bytes_transferred,
        }),
        _ => None,
    }
}

fn file_event_to_ffi(event: &FileEvent) -> Option<FfiItemEvent> {
    match event {
        FileEvent::Opened {
            codec,
            container,
            total_bytes,
            cached,
        } => Some(FfiItemEvent::FileOpened {
            codec: codec.map(Into::into),
            container: container.map(Into::into),
            total_bytes: *total_bytes,
            cached: *cached,
        }),
        FileEvent::TotalBytesResolved {
            total_bytes,
            source,
        } => Some(FfiItemEvent::FileTotalBytesResolved {
            total_bytes: *total_bytes,
            source: (*source).into(),
        }),
        FileEvent::CacheComplete { total_bytes } => Some(FfiItemEvent::FileCacheComplete {
            total_bytes: *total_bytes,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use kithara_events::{
        CancelReason, DownloaderEvent, Event, FileError, FileEvent, RequestId, TotalBytesSource,
    };

    use super::{ItemEventBridge, downloader_event_to_ffi, file_event_to_ffi};
    use crate::types::{FfiCancelReason, FfiError, FfiItemEvent, FfiTotalBytesSource};

    fn request_id(value: u64) -> RequestId {
        match NonZeroU64::new(value) {
            Some(id) => RequestId::new(id),
            None => panic!("request id must be non-zero"),
        }
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

    #[kithara::test]
    fn downloader_request_cancelled_maps_to_item_event() {
        let request_id = request_id(7);
        let event = DownloaderEvent::RequestCancelled {
            request_id,
            reason: CancelReason::PeerCancel,
            bytes_transferred: 123,
        };

        assert!(matches!(
            downloader_event_to_ffi(&event),
            Some(FfiItemEvent::DownloadCancelled {
                request_id: 7,
                reason: FfiCancelReason::PeerCancel,
                bytes_transferred: 123,
            })
        ));
    }

    #[kithara::test]
    fn downloader_request_failed_is_not_duplicated() {
        let request_id = request_id(9);
        let event = DownloaderEvent::RequestFailed {
            request_id,
            error: kithara_net::NetError::Network("boom".into()),
            retryable: false,
        };

        assert!(downloader_event_to_ffi(&event).is_none());
    }

    #[kithara::test]
    fn file_total_bytes_resolved_maps_to_item_event() {
        let event = FileEvent::TotalBytesResolved {
            total_bytes: 456,
            source: TotalBytesSource::CommittedLen,
        };

        assert!(matches!(
            file_event_to_ffi(&event),
            Some(FfiItemEvent::FileTotalBytesResolved {
                total_bytes: 456,
                source: FfiTotalBytesSource::CommittedLen,
            })
        ));
    }

    #[kithara::test]
    fn file_end_of_stream_is_not_duplicated() {
        assert!(file_event_to_ffi(&FileEvent::EndOfStream).is_none());
    }
}
