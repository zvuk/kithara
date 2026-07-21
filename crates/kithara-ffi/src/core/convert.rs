use kithara_events::{
    AssetEvent, AudioEvent, DecoderEvent, DjEvent, DownloaderEvent, DrmEvent, EngineEvent, Event,
    FileEvent, HlsEvent, QueueEvent, SessionEvent, TrackId,
};
use kithara_play::PlayerEvent;

use crate::types::{
    FfiAdvanceReason, FfiError, FfiEvictReason, FfiItemEvent, FfiPlayerEvent, FfiRepeatMode,
    FfiRouteChangeReason, FfiStretchBackendKind, FfiTrackStatus, duration_to_seconds,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotForwarded;

impl TryFrom<&Event> for FfiItemEvent {
    type Error = NotForwarded;

    fn try_from(event: &Event) -> Result<Self, NotForwarded> {
        match event {
            Event::Decoder(e) => Self::try_from(e),
            Event::Audio(e) => Self::try_from(e),
            Event::Hls(e) => Self::try_from(e),
            Event::Downloader(e) => Self::try_from(e),
            Event::File(e) => Self::try_from(e),
            Event::Drm(e) => Self::try_from(e),
            _ => Err(NotForwarded),
        }
    }
}

impl TryFrom<&Event> for FfiPlayerEvent {
    type Error = NotForwarded;

    fn try_from(event: &Event) -> Result<Self, NotForwarded> {
        match event {
            Event::Engine(e) => Self::try_from(e),
            Event::Session(e) => Self::try_from(e),
            Event::Transport(e) => Self::try_from(e),
            Event::Sync(e) => Self::try_from(e),
            Event::Dj(e) => Self::try_from(e),
            Event::Asset(e) => Self::try_from(e),
            _ => Err(NotForwarded),
        }
    }
}

impl TryFrom<&Event> for FfiError {
    type Error = NotForwarded;

    fn try_from(event: &Event) -> Result<Self, NotForwarded> {
        match event {
            Event::File(FileEvent::Error { error }) => Ok(Self::ItemFailed {
                reason: error.to_string(),
            }),
            Event::Hls(HlsEvent::Error { error }) => Ok(Self::ItemFailed {
                reason: error.to_string(),
            }),
            Event::Downloader(DownloaderEvent::RequestFailed { error, .. }) => {
                Ok(Self::ItemFailed {
                    reason: error.to_string(),
                })
            }
            _ => Err(NotForwarded),
        }
    }
}

impl TryFrom<&DecoderEvent> for FfiItemEvent {
    type Error = NotForwarded;

    fn try_from(event: &DecoderEvent) -> Result<Self, NotForwarded> {
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
                    .map_or((0, 0, false), |span| {
                        (span.leading_frames, span.trailing_frames, true)
                    });
                Ok(Self::DecoderChanged {
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
            } => Ok(Self::DecodeError {
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
            } => Ok(Self::GaplessResolved {
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
            } => Ok(Self::ResamplerConfigured {
                backend: (*backend).into(),
                input_rate: *input_rate,
                output_rate: *output_rate,
                channels: *channels,
                bypassed: *bypassed,
            }),
            _ => Err(NotForwarded),
        }
    }
}

impl TryFrom<&AudioEvent> for FfiItemEvent {
    type Error = NotForwarded;

    fn try_from(event: &AudioEvent) -> Result<Self, NotForwarded> {
        match event {
            AudioEvent::FormatDetected { spec } => Ok(Self::AudioFormatDetected {
                channels: spec.channels,
                sample_rate: spec.sample_rate,
            }),
            AudioEvent::FormatChanged { old, new } => Ok(Self::AudioFormatChanged {
                old_channels: old.channels,
                old_sample_rate: old.sample_rate,
                new_channels: new.channels,
                new_sample_rate: new.sample_rate,
            }),
            AudioEvent::SeekComplete {
                position,
                seek_epoch,
            } => Ok(Self::SeekComplete {
                position_seconds: duration_to_seconds(*position),
                epoch: *seek_epoch,
            }),
            AudioEvent::SeekRejected { epoch, target } => Ok(Self::SeekRejected {
                epoch: *epoch,
                target_seconds: duration_to_seconds(*target),
            }),
            AudioEvent::DecoderReady {
                base_offset,
                variant,
            } => Ok(Self::DecoderReady {
                base_offset: *base_offset,
                variant: *variant,
            }),
            AudioEvent::TrackFailed {
                failure,
                seek_epoch,
            } => Ok(Self::TrackFailed {
                reason: failure.clone().into(),
                epoch: *seek_epoch,
            }),
            AudioEvent::UnderrunStarted {
                position_ms,
                seek_epoch,
            } => Ok(Self::UnderrunStarted {
                position_ms: *position_ms,
                epoch: *seek_epoch,
            }),
            AudioEvent::UnderrunEnded {
                position_ms,
                seek_epoch,
            } => Ok(Self::UnderrunEnded {
                position_ms: *position_ms,
                epoch: *seek_epoch,
            }),
            AudioEvent::BufferHealth {
                buffered_ms,
                decoded_frontier_ms,
                seek_epoch,
            } => Ok(Self::BufferHealth {
                buffered_ms: *buffered_ms,
                decoded_frontier_ms: *decoded_frontier_ms,
                epoch: *seek_epoch,
            }),
            AudioEvent::EngineLoad {
                load,
                ms_per_chunk,
                realtime_factor,
            } => Ok(Self::EngineLoad {
                load: *load,
                ms_per_chunk: *ms_per_chunk,
                realtime_factor: *realtime_factor,
            }),
            AudioEvent::PlaybackResamplerConfigured {
                backend,
                host_sample_rate,
                source_sample_rate,
                active,
            } => Ok(Self::PlaybackResamplerConfigured {
                backend: (*backend).into(),
                host_sample_rate: *host_sample_rate,
                source_sample_rate: *source_sample_rate,
                active: *active,
            }),
            AudioEvent::EndOfStream => Ok(Self::DidReachEnd),
            _ => Err(NotForwarded),
        }
    }
}

impl TryFrom<&HlsEvent> for FfiItemEvent {
    type Error = NotForwarded;

    fn try_from(event: &HlsEvent) -> Result<Self, NotForwarded> {
        match event {
            HlsEvent::VariantSwitchFenced {
                from_variant,
                to_variant,
                cross_codec,
            } => Ok(Self::HlsVariantSwitchFenced {
                from_variant: u32::try_from(*from_variant).unwrap_or(u32::MAX),
                to_variant: u32::try_from(*to_variant).unwrap_or(u32::MAX),
                cross_codec: *cross_codec,
            }),
            HlsEvent::VariantSwitchAcked {
                variant,
                generation,
            } => Ok(Self::HlsVariantSwitchAcked {
                variant: u32::try_from(*variant).unwrap_or(u32::MAX),
                generation: *generation,
            }),
            HlsEvent::CacheComplete { total_bytes } => Ok(Self::HlsCacheComplete {
                total_bytes: *total_bytes,
            }),
            _ => Err(NotForwarded),
        }
    }
}

impl TryFrom<&DownloaderEvent> for FfiItemEvent {
    type Error = NotForwarded;

    fn try_from(event: &DownloaderEvent) -> Result<Self, NotForwarded> {
        match event {
            DownloaderEvent::RequestStarted {
                request_id,
                wait_in_queue,
            } => Ok(Self::DownloadStarted {
                request_id: request_id.get(),
                wait_in_queue_seconds: duration_to_seconds(*wait_in_queue),
            }),
            DownloaderEvent::LoadSlow {
                request_id,
                elapsed,
            } => Ok(Self::DownloadSlow {
                request_id: request_id.get(),
                elapsed_seconds: duration_to_seconds(*elapsed),
            }),
            DownloaderEvent::RequestCompleted {
                request_id,
                bytes_transferred,
                duration,
                bandwidth_bps,
            } => Ok(Self::DownloadCompleted {
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
            } => Ok(Self::DownloadRetrying {
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
            } => Ok(Self::DownloadBodyStalled {
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
            } => Ok(Self::DownloadBodyResumed {
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
            } => Ok(Self::DownloadRetryExhausted {
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
            } => Ok(Self::DownloadFirstByte {
                request_id: request_id.get(),
                ttfb_seconds: duration_to_seconds(*ttfb),
                status: *status,
                partial: *partial,
            }),
            DownloaderEvent::RequestCancelled {
                request_id,
                reason,
                bytes_transferred,
            } => Ok(Self::DownloadCancelled {
                request_id: request_id.get(),
                reason: (*reason).into(),
                bytes_transferred: *bytes_transferred,
            }),
            _ => Err(NotForwarded),
        }
    }
}

impl TryFrom<&FileEvent> for FfiItemEvent {
    type Error = NotForwarded;

    fn try_from(event: &FileEvent) -> Result<Self, NotForwarded> {
        match event {
            FileEvent::Opened {
                codec,
                container,
                total_bytes,
                cached,
            } => Ok(Self::FileOpened {
                codec: codec.map(Into::into),
                container: container.map(Into::into),
                total_bytes: *total_bytes,
                cached: *cached,
            }),
            FileEvent::TotalBytesResolved {
                total_bytes,
                source,
            } => Ok(Self::FileTotalBytesResolved {
                total_bytes: *total_bytes,
                source: (*source).into(),
            }),
            FileEvent::CacheComplete { total_bytes } => Ok(Self::FileCacheComplete {
                total_bytes: *total_bytes,
            }),
            _ => Err(NotForwarded),
        }
    }
}

impl TryFrom<&DrmEvent> for FfiItemEvent {
    type Error = NotForwarded;

    fn try_from(event: &DrmEvent) -> Result<Self, NotForwarded> {
        match event {
            DrmEvent::KeyFetchFailed {
                key_host,
                stage,
                detail,
            } => Ok(Self::DrmKeyFetchFailed {
                key_host: key_host.clone(),
                stage: (*stage).into(),
                detail: detail.clone(),
            }),
            DrmEvent::KeyAcquired {
                key_host,
                source,
                bytes,
                latency_ms,
            } => Ok(Self::DrmKeyAcquired {
                key_host: key_host.clone(),
                source: (*source).into(),
                bytes: *bytes as u64,
                latency_ms: *latency_ms,
            }),
            DrmEvent::SegmentDecryptFailed {
                variant,
                segment_index,
                detail,
            } => Ok(Self::DrmSegmentDecryptFailed {
                variant: *variant,
                segment_index: *segment_index,
                detail: detail.clone(),
            }),
            _ => Err(NotForwarded),
        }
    }
}

impl TryFrom<&EngineEvent> for FfiPlayerEvent {
    type Error = NotForwarded;

    fn try_from(event: &EngineEvent) -> Result<Self, NotForwarded> {
        Ok(match event {
            EngineEvent::Started => Self::EngineStarted,
            EngineEvent::Stopped => Self::EngineStopped,
            EngineEvent::CrossfadeCompleted { .. } => Self::CrossfadeCompleted,
            EngineEvent::CrossfadeCancelled => Self::CrossfadeCancelled,
            EngineEvent::MasterVolumeChanged { volume } => {
                Self::MasterVolumeChanged { volume: *volume }
            }
            _ => return Err(NotForwarded),
        })
    }
}

impl TryFrom<&SessionEvent> for FfiPlayerEvent {
    type Error = NotForwarded;

    fn try_from(event: &SessionEvent) -> Result<Self, NotForwarded> {
        Ok(match event {
            SessionEvent::RouteChanged { reason, .. } => Self::AudioRouteChanged {
                reason: FfiRouteChangeReason::from(*reason),
            },
            _ => return Err(NotForwarded),
        })
    }
}

impl TryFrom<&DjEvent> for FfiPlayerEvent {
    type Error = NotForwarded;

    fn try_from(event: &DjEvent) -> Result<Self, NotForwarded> {
        Ok(match event {
            DjEvent::BpmDetected { slot, info } => Self::DjBpmDetected {
                slot: slot.value(),
                bpm: info.bpm,
                confidence: info.confidence,
                first_beat_offset_seconds: duration_to_seconds(info.first_beat_offset),
            },
            DjEvent::KeylockChanged { on } => Self::DjKeylockChanged { on: *on },
            DjEvent::StretchBackendChanged { kind } => Self::DjStretchBackendChanged {
                kind: FfiStretchBackendKind::from(*kind),
            },
            _ => return Err(NotForwarded),
        })
    }
}

impl TryFrom<&AssetEvent> for FfiPlayerEvent {
    type Error = NotForwarded;

    fn try_from(event: &AssetEvent) -> Result<Self, NotForwarded> {
        Ok(match event {
            AssetEvent::Committed {
                asset_root,
                rel_path,
                final_len,
            } => Self::AssetCommitted {
                asset_root: asset_root.clone(),
                rel_path: rel_path.clone(),
                final_len: *final_len,
            },
            AssetEvent::Failed {
                asset_root,
                rel_path,
                reason,
            } => Self::AssetFailed {
                asset_root: asset_root.clone(),
                rel_path: rel_path.clone(),
                reason: reason.clone(),
            },
            AssetEvent::Evicted { asset_root, reason } => Self::AssetEvicted {
                asset_root: asset_root.clone(),
                reason: FfiEvictReason::from(*reason),
            },
            _ => return Err(NotForwarded),
        })
    }
}

impl TryFrom<&PlayerEvent> for FfiPlayerEvent {
    type Error = NotForwarded;

    fn try_from(event: &PlayerEvent) -> Result<Self, NotForwarded> {
        Ok(match event {
            PlayerEvent::RateChanged { rate } => Self::RateChanged { rate: *rate },
            PlayerEvent::StatusChanged { status } => Self::StatusChanged {
                status: (*status).into(),
            },
            PlayerEvent::TimeControlStatusChanged { status, .. } => {
                Self::TimeControlStatusChanged {
                    status: (*status).into(),
                }
            }
            PlayerEvent::VolumeChanged { volume } => Self::VolumeChanged { volume: *volume },
            PlayerEvent::MuteChanged { muted } => Self::MuteChanged { muted: *muted },
            PlayerEvent::ItemDidPlayToEnd { .. } => Self::ItemDidPlayToEnd,
            PlayerEvent::ItemDidFail { item_id, .. } => Self::ItemDidFail {
                item_id: item_id
                    .as_ref()
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(TrackId::from),
            },
            _ => return Err(NotForwarded),
        })
    }
}

impl TryFrom<&QueueEvent> for FfiPlayerEvent {
    type Error = NotForwarded;

    fn try_from(event: &QueueEvent) -> Result<Self, NotForwarded> {
        Ok(match event {
            QueueEvent::TrackAdded { id, index } => Self::TrackAdded {
                item_id: *id,
                index: *index as u64,
            },
            QueueEvent::TrackRemoved { id } => Self::TrackRemoved { item_id: *id },
            QueueEvent::TrackStatusChanged { id, status } => Self::TrackStatusChanged {
                item_id: *id,
                status: FfiTrackStatus::from(status.clone()),
            },
            QueueEvent::CurrentTrackChanged { id } => Self::CurrentItemChanged { item_id: *id },
            QueueEvent::CurrentTrackAdvance { id, reason } => Self::CurrentItemAdvanced {
                item_id: *id,
                reason: FfiAdvanceReason::from(*reason),
            },
            QueueEvent::QueueEnded => Self::QueueEnded,
            QueueEvent::TrackLoadFailed {
                id,
                reason,
                auto_skipped,
            } => Self::TrackLoadFailed {
                item_id: *id,
                reason: reason.clone(),
                auto_skipped: *auto_skipped,
            },
            QueueEvent::CrossfadeStarted { duration_seconds } => Self::CrossfadeStarted {
                duration_seconds: *duration_seconds,
            },
            QueueEvent::CrossfadeDurationChanged { seconds } => {
                Self::CrossfadeDurationChanged { seconds: *seconds }
            }
            QueueEvent::RepeatModeChanged { mode } => Self::RepeatModeChanged {
                mode: FfiRepeatMode::from(*mode),
            },
            QueueEvent::NextTrackReady { id, index } => Self::NextTrackReady {
                item_id: *id,
                index: *index as u64,
            },
            _ => return Err(NotForwarded),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use kithara_events::{
        AssetEvent, CancelReason, DownloaderEvent, DrmEvent, EngineEvent, Event, EvictReason,
        FileEvent, KeyFailureStage, KeySource, MediaTime, PlaybackDirection, QueueEvent, RequestId,
        RouteChangeReason, RouteDescription, SessionEvent, StretchBackendKind, SyncEvent,
        TotalBytesSource, TrackId, TransportEvent,
    };
    use kithara_platform::time::Duration;
    use kithara_play::PlayerEvent;

    use super::{FfiError, FfiItemEvent, FfiPlayerEvent, NotForwarded};
    use crate::types::{
        FfiCancelReason, FfiEvictReason, FfiKeyFailureStage, FfiKeySource, FfiPlaybackDirection,
        FfiRouteChangeReason, FfiStretchBackendKind, FfiTotalBytesSource,
    };

    fn request_id(value: u64) -> RequestId {
        NonZeroU64::new(value).map_or_else(|| panic!("request id must be non-zero"), RequestId::new)
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
            FfiItemEvent::try_from(&event),
            Ok(FfiItemEvent::DownloadCancelled {
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

        assert!(matches!(FfiItemEvent::try_from(&event), Err(NotForwarded)));
    }

    #[kithara::test]
    fn file_total_bytes_resolved_maps_to_item_event() {
        let event = FileEvent::TotalBytesResolved {
            total_bytes: 456,
            source: TotalBytesSource::CommittedLen,
        };

        assert!(matches!(
            FfiItemEvent::try_from(&event),
            Ok(FfiItemEvent::FileTotalBytesResolved {
                total_bytes: 456,
                source: FfiTotalBytesSource::CommittedLen,
            })
        ));
    }

    #[kithara::test]
    fn file_end_of_stream_is_not_duplicated() {
        assert!(matches!(
            FfiItemEvent::try_from(&FileEvent::EndOfStream),
            Err(NotForwarded)
        ));
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
            FfiItemEvent::try_from(&event),
            Ok(FfiItemEvent::DrmKeyAcquired {
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
            FfiItemEvent::try_from(&event),
            Ok(FfiItemEvent::DrmKeyFetchFailed {
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
            FfiItemEvent::try_from(&event),
            Ok(FfiItemEvent::DrmSegmentDecryptFailed {
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
            FfiPlayerEvent::try_from(&EngineEvent::MasterVolumeChanged { volume: 0.5 }),
            Ok(FfiPlayerEvent::MasterVolumeChanged { volume }) if volume == 0.5
        ));
    }

    #[kithara::test]
    fn engine_event_to_ffi_skips_internal_and_duplicate_crossfade_events() {
        assert!(matches!(
            FfiPlayerEvent::try_from(&EngineEvent::CrossfadeStarted {
                from: kithara_events::SlotId::new(1),
                to: kithara_events::SlotId::new(2),
                duration: Duration::from_secs(1),
            }),
            Err(NotForwarded)
        ));
        assert!(matches!(
            FfiPlayerEvent::try_from(&EngineEvent::SlotAllocated {
                slot: kithara_events::SlotId::new(3),
            }),
            Err(NotForwarded)
        ));
    }

    #[kithara::test]
    fn session_event_to_ffi_maps_route_changed_reason() {
        assert!(matches!(
            FfiPlayerEvent::try_from(&SessionEvent::RouteChanged {
                reason: RouteChangeReason::CategoryChange,
                previous_route: RouteDescription::default(),
            }),
            Ok(FfiPlayerEvent::AudioRouteChanged {
                reason: FfiRouteChangeReason::CategoryChange,
            })
        ));
    }

    #[kithara::test]
    fn transport_event_to_ffi_uses_transport_vocabulary() {
        let event = Event::Transport(TransportEvent::TempoCommitted {
            beats_per_minute: 128.0,
            revision: 7,
        });

        assert!(matches!(
            FfiPlayerEvent::try_from(&event),
            Ok(FfiPlayerEvent::TransportTempoCommitted {
                beats_per_minute: 128.0,
                revision: 7,
            })
        ));
    }

    #[kithara::test]
    fn transport_seek_event_to_ffi_preserves_target_and_revision() {
        let event = Event::Transport(TransportEvent::SeekCommitted {
            position_beats: 12.5,
            revision: 8,
        });

        assert!(matches!(
            FfiPlayerEvent::try_from(&event),
            Ok(FfiPlayerEvent::TransportSeekCommitted {
                position_beats: 12.5,
                revision: 8,
            })
        ));
    }

    #[kithara::test]
    fn sync_event_to_ffi_uses_sync_vocabulary() {
        let event = Event::Sync(SyncEvent::BindingCommitted {
            slot: kithara_events::SlotId::new(4),
            session_anchor_beats: 8.0,
            track_anchor_beats: 16.0,
            direction: PlaybackDirection::Reverse,
        });

        assert!(matches!(
            FfiPlayerEvent::try_from(&event),
            Ok(FfiPlayerEvent::SyncBindingCommitted {
                slot: 4,
                session_anchor_beats: 8.0,
                track_anchor_beats: 16.0,
                direction: FfiPlaybackDirection::Reverse,
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
            FfiPlayerEvent::try_from(&kithara_events::DjEvent::BeatTick {
                slot: kithara_events::SlotId::new(9),
                beat_number: 4,
                timestamp: MediaTime::default(),
            }),
            Err(NotForwarded)
        ));
    }

    #[kithara::test]
    fn dj_event_to_ffi_maps_bpm_detected_fields() {
        assert!(matches!(
            FfiPlayerEvent::try_from(&kithara_events::DjEvent::BpmDetected {
                slot: kithara_events::SlotId::new(7),
                info: kithara_events::BpmInfo::new(128.5, Some(0.8), Duration::from_millis(250)),
            }),
            Ok(FfiPlayerEvent::DjBpmDetected {
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
            FfiPlayerEvent::try_from(&kithara_events::DjEvent::StretchBackendChanged {
                kind: StretchBackendKind::Bungee,
            }),
            Ok(FfiPlayerEvent::DjStretchBackendChanged {
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
            FfiPlayerEvent::try_from(&AssetEvent::Evicted {
                asset_root: "/tmp/cache".to_string(),
                reason: EvictReason::QuotaBytes,
            }),
            Ok(FfiPlayerEvent::AssetEvicted {
                asset_root,
                reason: FfiEvictReason::QuotaBytes,
            }) if asset_root == "/tmp/cache"
        ));
    }

    #[kithara::test]
    fn player_event_to_ffi_maps_item_did_fail_track_id() {
        let event = PlayerEvent::ItemDidFail {
            src: "src".into(),
            item_id: Some("7".into()),
        };

        assert!(matches!(
            FfiPlayerEvent::try_from(&event),
            Ok(FfiPlayerEvent::ItemDidFail { item_id: Some(id) }) if id == TrackId::from(7_u64)
        ));
    }

    #[kithara::test]
    fn queue_event_to_ffi_maps_repeat_mode() {
        let event = QueueEvent::RepeatModeChanged {
            mode: kithara_events::QueueRepeatMode::All,
        };

        assert!(matches!(
            FfiPlayerEvent::try_from(&event),
            Ok(FfiPlayerEvent::RepeatModeChanged {
                mode: crate::types::FfiRepeatMode::All
            })
        ));
    }

    #[kithara::test]
    fn event_to_ffi_error_maps_request_failed() {
        let event = Event::Downloader(DownloaderEvent::RequestFailed {
            request_id: request_id(13),
            error: kithara_net::NetError::Network("boom".into()),
            retryable: false,
        });

        assert!(matches!(
            FfiError::try_from(&event),
            Ok(FfiError::ItemFailed { reason }) if reason == "Network error: boom"
        ));
    }
}
