use kithara_events::{
    AssetEvent, AudioEvent, DecoderEvent, DjEvent, DownloaderEvent, DrmEvent, EngineEvent, Event,
    FileEvent, HlsEvent, SessionEvent,
};

use crate::types::{
    FfiEvictReason, FfiItemEvent, FfiPlayerEvent, FfiRouteChangeReason, FfiStretchBackendKind,
    duration_to_seconds,
};

pub(crate) fn item_event_to_ffi(event: &Event) -> Option<FfiItemEvent> {
    match event {
        Event::Decoder(e) => decoder_event_to_ffi(e),
        Event::Audio(e) => audio_event_to_ffi(e),
        Event::Hls(e) => hls_event_to_ffi(e),
        Event::Downloader(e) => downloader_event_to_ffi(e),
        Event::File(e) => file_event_to_ffi(e),
        Event::Drm(e) => drm_event_to_ffi(e),
        _ => None,
    }
}

pub(crate) fn player_event_to_ffi(event: &Event) -> Option<FfiPlayerEvent> {
    match event {
        Event::Engine(e) => engine_event_to_ffi(e),
        Event::Session(e) => session_event_to_ffi(e),
        Event::Dj(e) => dj_event_to_ffi(e),
        Event::Asset(e) => asset_event_to_ffi(e),
        _ => None,
    }
}

pub(crate) fn decoder_event_to_ffi(event: &DecoderEvent) -> Option<FfiItemEvent> {
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
                duration_seconds: duration.map(duration_to_seconds),
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

pub(crate) fn audio_event_to_ffi(event: &AudioEvent) -> Option<FfiItemEvent> {
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
            position_seconds: duration_to_seconds(*position),
            epoch: *seek_epoch,
        }),
        AudioEvent::SeekRejected { epoch, target } => Some(FfiItemEvent::SeekRejected {
            epoch: *epoch,
            target_seconds: duration_to_seconds(*target),
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

pub(crate) fn hls_event_to_ffi(event: &HlsEvent) -> Option<FfiItemEvent> {
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

pub(crate) fn downloader_event_to_ffi(event: &DownloaderEvent) -> Option<FfiItemEvent> {
    match event {
        DownloaderEvent::RequestStarted {
            request_id,
            wait_in_queue,
        } => Some(FfiItemEvent::DownloadStarted {
            request_id: request_id.get(),
            wait_in_queue_seconds: duration_to_seconds(*wait_in_queue),
        }),
        DownloaderEvent::LoadSlow {
            request_id,
            elapsed,
        } => Some(FfiItemEvent::DownloadSlow {
            request_id: request_id.get(),
            elapsed_seconds: duration_to_seconds(*elapsed),
        }),
        DownloaderEvent::RequestCompleted {
            request_id,
            bytes_transferred,
            duration,
            bandwidth_bps,
        } => Some(FfiItemEvent::DownloadCompleted {
            request_id: request_id.get(),
            bytes_transferred: *bytes_transferred,
            duration_seconds: duration_to_seconds(*duration),
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
            backoff_seconds: duration_to_seconds(*backoff),
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
            stall_seconds: duration_to_seconds(*stall),
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
            ttfb_seconds: duration_to_seconds(*ttfb),
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

pub(crate) fn file_event_to_ffi(event: &FileEvent) -> Option<FfiItemEvent> {
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

pub(crate) fn drm_event_to_ffi(event: &DrmEvent) -> Option<FfiItemEvent> {
    match event {
        DrmEvent::KeyFetchFailed {
            key_host,
            stage,
            detail,
        } => Some(FfiItemEvent::DrmKeyFetchFailed {
            key_host: key_host.clone(),
            stage: (*stage).into(),
            detail: detail.clone(),
        }),
        DrmEvent::KeyAcquired {
            key_host,
            source,
            bytes,
            latency_ms,
        } => Some(FfiItemEvent::DrmKeyAcquired {
            key_host: key_host.clone(),
            source: (*source).into(),
            bytes: *bytes as u64,
            latency_ms: *latency_ms,
        }),
        DrmEvent::SegmentDecryptFailed {
            variant,
            segment_index,
            detail,
        } => Some(FfiItemEvent::DrmSegmentDecryptFailed {
            variant: *variant,
            segment_index: *segment_index,
            detail: detail.clone(),
        }),
        _ => None,
    }
}

pub(crate) fn engine_event_to_ffi(event: &EngineEvent) -> Option<FfiPlayerEvent> {
    Some(match event {
        EngineEvent::Started => FfiPlayerEvent::EngineStarted,
        EngineEvent::Stopped => FfiPlayerEvent::EngineStopped,
        EngineEvent::CrossfadeCompleted { .. } => FfiPlayerEvent::CrossfadeCompleted,
        EngineEvent::CrossfadeCancelled => FfiPlayerEvent::CrossfadeCancelled,
        EngineEvent::MasterVolumeChanged { volume } => {
            FfiPlayerEvent::MasterVolumeChanged { volume: *volume }
        }
        _ => return None,
    })
}

pub(crate) fn session_event_to_ffi(event: &SessionEvent) -> Option<FfiPlayerEvent> {
    Some(match event {
        SessionEvent::RouteChanged { reason, .. } => FfiPlayerEvent::AudioRouteChanged {
            reason: FfiRouteChangeReason::from(*reason),
        },
        _ => return None,
    })
}

pub(crate) fn dj_event_to_ffi(event: &DjEvent) -> Option<FfiPlayerEvent> {
    Some(match event {
        DjEvent::BpmDetected { slot, info } => FfiPlayerEvent::DjBpmDetected {
            slot: slot.value(),
            bpm: info.bpm,
            confidence: info.confidence,
            first_beat_offset_seconds: duration_to_seconds(info.first_beat_offset),
        },
        DjEvent::KeylockChanged { on } => FfiPlayerEvent::DjKeylockChanged { on: *on },
        DjEvent::StretchBackendChanged { kind } => FfiPlayerEvent::DjStretchBackendChanged {
            kind: FfiStretchBackendKind::from(*kind),
        },
        _ => return None,
    })
}

pub(crate) fn asset_event_to_ffi(event: &AssetEvent) -> Option<FfiPlayerEvent> {
    Some(match event {
        AssetEvent::Committed {
            asset_root,
            rel_path,
            final_len,
        } => FfiPlayerEvent::AssetCommitted {
            asset_root: asset_root.clone(),
            rel_path: rel_path.clone(),
            final_len: *final_len,
        },
        AssetEvent::Failed {
            asset_root,
            rel_path,
            reason,
        } => FfiPlayerEvent::AssetFailed {
            asset_root: asset_root.clone(),
            rel_path: rel_path.clone(),
            reason: reason.clone(),
        },
        AssetEvent::Evicted { asset_root, reason } => FfiPlayerEvent::AssetEvicted {
            asset_root: asset_root.clone(),
            reason: FfiEvictReason::from(*reason),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use kithara_events::{
        AssetEvent, CancelReason, DownloaderEvent, DrmEvent, EngineEvent, EvictReason, FileEvent,
        KeyFailureStage, KeySource, RequestId, RouteChangeReason, SessionEvent, StretchBackendKind,
        TotalBytesSource,
    };
    use kithara_platform::time::Duration;

    use super::{
        asset_event_to_ffi, dj_event_to_ffi, downloader_event_to_ffi, drm_event_to_ffi,
        engine_event_to_ffi, file_event_to_ffi, session_event_to_ffi,
    };
    use crate::types::{
        FfiCancelReason, FfiEvictReason, FfiItemEvent, FfiKeyFailureStage, FfiKeySource,
        FfiPlayerEvent, FfiRouteChangeReason, FfiStretchBackendKind, FfiTotalBytesSource,
    };

    fn request_id(value: u64) -> RequestId {
        match NonZeroU64::new(value) {
            Some(id) => RequestId::new(id),
            None => panic!("request id must be non-zero"),
        }
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

    #[kithara::test]
    fn drm_key_acquired_maps_to_item_event() {
        let event = DrmEvent::KeyAcquired {
            key_host: Some("keys.example.com".into()),
            source: KeySource::DiskCache,
            bytes: 64,
            latency_ms: Some(12),
        };

        assert!(matches!(
            drm_event_to_ffi(&event),
            Some(FfiItemEvent::DrmKeyAcquired {
                key_host,
                source: FfiKeySource::DiskCache,
                bytes: 64,
                latency_ms: Some(12),
            }) if key_host.as_deref() == Some("keys.example.com")
        ));
    }

    #[kithara::test]
    fn drm_key_fetch_failed_maps_to_item_event() {
        let event = DrmEvent::KeyFetchFailed {
            key_host: Some("keys.example.com".into()),
            stage: KeyFailureStage::Missing,
            detail: "missing key".into(),
        };

        assert!(matches!(
            drm_event_to_ffi(&event),
            Some(FfiItemEvent::DrmKeyFetchFailed {
                key_host,
                stage: FfiKeyFailureStage::Missing,
                detail,
            }) if key_host.as_deref() == Some("keys.example.com") && detail == "missing key"
        ));
    }

    #[kithara::test]
    fn drm_segment_decrypt_failed_maps_to_item_event() {
        let event = DrmEvent::SegmentDecryptFailed {
            variant: 3,
            segment_index: 17,
            detail: "decrypt failed".into(),
        };

        assert!(matches!(
            drm_event_to_ffi(&event),
            Some(FfiItemEvent::DrmSegmentDecryptFailed {
                variant: 3,
                segment_index: 17,
                detail,
            }) if detail == "decrypt failed"
        ));
    }

    #[kithara::test]
    fn route_change_reason_from_maps_known_value() {
        assert_eq!(
            FfiRouteChangeReason::from(RouteChangeReason::CategoryChange),
            FfiRouteChangeReason::CategoryChange
        );
    }

    #[kithara::test]
    fn engine_event_to_ffi_maps_master_volume_changed() {
        assert!(matches!(
            engine_event_to_ffi(&EngineEvent::MasterVolumeChanged { volume: 0.5 }),
            Some(FfiPlayerEvent::MasterVolumeChanged { volume }) if volume == 0.5
        ));
    }

    #[kithara::test]
    fn engine_event_to_ffi_skips_internal_and_duplicate_crossfade_events() {
        assert!(matches!(
            engine_event_to_ffi(&EngineEvent::CrossfadeStarted {
                from: kithara_events::SlotId::new(1),
                to: kithara_events::SlotId::new(2),
                duration: Duration::from_secs(1),
            }),
            None
        ));
        assert!(matches!(
            engine_event_to_ffi(&EngineEvent::SlotAllocated {
                slot: kithara_events::SlotId::new(3),
            }),
            None
        ));
    }

    #[kithara::test]
    fn session_event_to_ffi_maps_route_changed_reason() {
        assert!(matches!(
            session_event_to_ffi(&SessionEvent::RouteChanged {
                reason: RouteChangeReason::CategoryChange,
                previous_route: Default::default(),
            }),
            Some(FfiPlayerEvent::AudioRouteChanged {
                reason: FfiRouteChangeReason::CategoryChange,
            })
        ));
    }

    #[kithara::test]
    fn stretch_backend_kind_from_maps_bungee() {
        assert_eq!(
            FfiStretchBackendKind::from(StretchBackendKind::Bungee),
            FfiStretchBackendKind::Bungee
        );
    }

    #[kithara::test]
    fn dj_event_to_ffi_skips_beat_tick() {
        assert!(matches!(
            dj_event_to_ffi(&kithara_events::DjEvent::BeatTick {
                slot: kithara_events::SlotId::new(9),
                beat_number: 4,
                timestamp: Default::default(),
            }),
            None
        ));
    }

    #[kithara::test]
    fn dj_event_to_ffi_maps_bpm_detected_fields() {
        assert!(matches!(
            dj_event_to_ffi(&kithara_events::DjEvent::BpmDetected {
                slot: kithara_events::SlotId::new(7),
                info: kithara_events::BpmInfo::new(128.5, Some(0.8), Duration::from_millis(250)),
            }),
            Some(FfiPlayerEvent::DjBpmDetected {
                slot: 7,
                bpm: 128.5,
                confidence: Some(0.8),
                first_beat_offset_seconds: 0.25,
            })
        ));
    }

    #[kithara::test]
    fn dj_event_to_ffi_maps_stretch_backend_changed() {
        assert!(matches!(
            dj_event_to_ffi(&kithara_events::DjEvent::StretchBackendChanged {
                kind: StretchBackendKind::Bungee,
            }),
            Some(FfiPlayerEvent::DjStretchBackendChanged {
                kind: FfiStretchBackendKind::Bungee,
            })
        ));
    }

    #[kithara::test]
    fn evict_reason_from_maps_quota_bytes() {
        assert_eq!(
            FfiEvictReason::from(EvictReason::QuotaBytes),
            FfiEvictReason::QuotaBytes
        );
    }

    #[kithara::test]
    fn asset_event_to_ffi_maps_evicted_reason() {
        assert!(matches!(
            asset_event_to_ffi(&AssetEvent::Evicted {
                asset_root: "/tmp/cache".to_string(),
                reason: EvictReason::QuotaBytes,
            }),
            Some(FfiPlayerEvent::AssetEvicted {
                asset_root,
                reason: FfiEvictReason::QuotaBytes,
            }) if asset_root == "/tmp/cache"
        ));
    }
}
