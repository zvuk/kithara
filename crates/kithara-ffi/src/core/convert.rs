use kithara::{
    assets::AssetEvent,
    download::DownloaderEvent,
    play::{DjEvent, EngineEvent, PlayerEvent, SessionEvent},
    queue::QueueEvent,
};
use kithara_audio::{AudioEvent, DecoderEvent};
use kithara_file::FileEvent;
use kithara_hls::{DrmEvent, HlsEvent};

use super::event_set::{ItemBusEvent, QueueBusEvent};
use crate::types::{
    FfiActionAtItemEnd, FfiAdvanceReason, FfiError, FfiEvictReason, FfiItemEvent, FfiPlaybackOrder,
    FfiPlayerEvent, FfiRepeatMode, FfiRouteChangeReason, FfiStretchBackendKind, FfiTrackStatus,
    duration_to_seconds,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotForwarded;

impl TryFrom<&ItemBusEvent> for FfiItemEvent {
    type Error = NotForwarded;

    fn try_from(event: &ItemBusEvent) -> Result<Self, NotForwarded> {
        match event {
            ItemBusEvent::Decoder(e) => Self::try_from(e),
            ItemBusEvent::Audio(e) => Self::try_from(e),
            ItemBusEvent::Hls(e) => Self::try_from(e),
            ItemBusEvent::Downloader(e) => Self::try_from(e),
            ItemBusEvent::File(e) => Self::try_from(e),
            ItemBusEvent::Drm(e) => Self::try_from(e),
            _ => Err(NotForwarded),
        }
    }
}

impl TryFrom<&QueueBusEvent> for FfiPlayerEvent {
    type Error = NotForwarded;

    fn try_from(event: &QueueBusEvent) -> Result<Self, NotForwarded> {
        match event {
            QueueBusEvent::Engine(e) => Self::try_from(e),
            QueueBusEvent::Session(e) => Self::try_from(e),
            QueueBusEvent::Dj(e) => Self::try_from(e),
            QueueBusEvent::Asset(e) => Self::try_from(e),
            _ => Err(NotForwarded),
        }
    }
}

impl TryFrom<&ItemBusEvent> for FfiError {
    type Error = NotForwarded;

    fn try_from(event: &ItemBusEvent) -> Result<Self, NotForwarded> {
        match event {
            ItemBusEvent::File(FileEvent::Error { error }) => Ok(Self::ItemFailed {
                reason: error.to_string(),
            }),
            ItemBusEvent::Hls(HlsEvent::Error { error }) => Ok(Self::ItemFailed {
                reason: error.to_string(),
            }),
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
                    gapless_leading,
                    gapless_trailing,
                    has_gapless,
                    backend: (*backend).into(),
                    codec: codec.map(Into::into),
                    container: container.map(Into::into),
                    sample_rate: *sample_rate,
                    channels: *channels,
                    bit_depth: *bit_depth,
                    bitrate: *bitrate,
                    cause: (*cause).into(),
                    variant: *variant,
                    base_offset: *base_offset,
                    duration_seconds: duration.map(duration_to_seconds),
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
            DecoderEvent::TransitionHold { .. } => Err(NotForwarded),
        }
    }
}

impl TryFrom<&AudioEvent> for FfiItemEvent {
    type Error = NotForwarded;

    fn try_from(event: &AudioEvent) -> Result<Self, NotForwarded> {
        match event {
            AudioEvent::FormatDetected { spec } => Ok(Self::AudioFormatDetected {
                channels: spec.channels,
                sample_rate: spec.sample_rate.get(),
            }),
            AudioEvent::FormatChanged { old, new } => Ok(Self::AudioFormatChanged {
                old_channels: old.channels,
                old_sample_rate: old.sample_rate.get(),
                new_channels: new.channels,
                new_sample_rate: new.sample_rate.get(),
            }),
            AudioEvent::SeekComplete { position } => Ok(Self::SeekComplete {
                position_seconds: duration_to_seconds(*position),
            }),
            AudioEvent::SeekRejected { target } => Ok(Self::SeekRejected {
                target_seconds: duration_to_seconds(*target),
            }),
            AudioEvent::DecoderReady {
                base_offset,
                variant,
            } => Ok(Self::DecoderReady {
                base_offset: *base_offset,
                variant: *variant,
            }),
            AudioEvent::TrackFailed { failure } => Ok(Self::TrackFailed {
                reason: (*failure).into(),
            }),
            AudioEvent::UnderrunStarted { position_ms } => Ok(Self::UnderrunStarted {
                position_ms: *position_ms,
            }),
            AudioEvent::UnderrunEnded { position_ms } => Ok(Self::UnderrunEnded {
                position_ms: *position_ms,
            }),
            AudioEvent::BufferHealth {
                buffered_ms,
                decoded_frontier_ms,
            } => Ok(Self::BufferHealth {
                buffered_ms: *buffered_ms,
                decoded_frontier_ms: *decoded_frontier_ms,
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
            AudioEvent::PlaybackProgress { .. }
            | AudioEvent::OutputAvailable
            | AudioEvent::SeekLifecycle { .. }
            | AudioEvent::EndOfStream => Err(NotForwarded),
        }
    }
}

impl TryFrom<&HlsEvent> for FfiItemEvent {
    type Error = NotForwarded;

    fn try_from(event: &HlsEvent) -> Result<Self, NotForwarded> {
        match event {
            HlsEvent::CacheComplete { total_bytes } => Ok(Self::HlsCacheComplete {
                total_bytes: *total_bytes,
            }),
            HlsEvent::SegmentReadStart { .. }
            | HlsEvent::SegmentReadComplete { .. }
            | HlsEvent::ReadProgress { .. }
            | HlsEvent::ReaderSeek { .. }
            | HlsEvent::Seek { .. }
            | HlsEvent::Error { .. }
            | HlsEvent::EndOfStream => Err(NotForwarded),
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
            DownloaderEvent::RequestEnqueued { .. }
            | DownloaderEvent::RequestFailed { .. }
            | DownloaderEvent::PriorityChanged { .. } => Err(NotForwarded),
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
            FileEvent::ReadProgress { .. }
            | FileEvent::ReaderSeek { .. }
            | FileEvent::Error { .. }
            | FileEvent::EndOfStream => Err(NotForwarded),
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
            EngineEvent::CrossfadeStarted { .. } | EngineEvent::CrossfadeProgress { .. } => {
                return Err(NotForwarded);
            }
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
            SessionEvent::Interruption { .. }
            | SessionEvent::MediaServicesLost
            | SessionEvent::MediaServicesReset
            | SessionEvent::SilenceSecondaryAudioHint { .. } => return Err(NotForwarded),
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
            DjEvent::BeatTick { .. }
            | DjEvent::BpmSyncEngaged { .. }
            | DjEvent::BpmSyncDisengaged { .. }
            | DjEvent::PhaseAligned { .. } => return Err(NotForwarded),
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
            PlayerEvent::ItemDidFail { item, .. } => Self::ItemDidFail {
                item_id: Some(item.id()),
            },
            PlayerEvent::PlaybackStarted { .. }
            | PlayerEvent::CurrentItemChanged { .. }
            | PlayerEvent::PrerollCompleted { .. } => return Err(NotForwarded),
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
            QueueEvent::TrackMetadataChanged { .. } => return Err(NotForwarded),
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
            QueueEvent::CrossfadeStarted { settings } => Self::CrossfadeStarted {
                settings: (*settings).into(),
            },
            QueueEvent::CrossfadeSettingsChanged { settings } => Self::CrossfadeSettingsChanged {
                settings: (*settings).into(),
            },
            QueueEvent::PlaybackOrderChanged { order } => Self::PlaybackOrderChanged {
                order: FfiPlaybackOrder::from(*order),
            },
            QueueEvent::ActionAtItemEndChanged { action } => Self::ActionAtItemEndChanged {
                action: FfiActionAtItemEnd::from(*action),
            },
            QueueEvent::RepeatModeChanged { mode } => Self::RepeatModeChanged {
                mode: FfiRepeatMode::from(*mode),
            },
            QueueEvent::NextTrackReady { id, index } => Self::NextTrackReady {
                item_id: *id,
                index: *index as u64,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};

    use kithara::{
        assets::{AssetEvent, EvictReason},
        audio::{
            AudioEvent, DecodeErrorClass, DecodeErrorKind, DecoderBackend, DecoderChangeCause,
            DecoderEvent, FrameDomain, PlaybackResamplerKind, ResamplerKind, TrackFailureKind,
        },
        decode::GaplessInfo,
        download::{CancelReason, DownloaderEvent, RequestId},
        events::{SlotId, TrackId},
        platform::{sync::Arc, time::Duration},
        play::{
            DjEvent, EngineEvent, ItemRole, MediaTime, PlaybackFault, PlayerEvent, PlayerStatus,
            RouteChangeReason, RouteDescription, SessionEvent, StretchBackendKind,
            TimeControlStatus, TrackRef,
        },
        queue::{QueueEvent, QueueRepeatMode, TrackStatus},
        signal::AudioSpec,
        stream::{AudioCodec, ContainerFormat},
    };
    use kithara_file::{FileEvent, TotalBytesSource};
    use kithara_hls::{DrmEvent, HlsEvent, KeyFailureStage, KeySource};

    use super::{
        FfiError, FfiItemEvent, FfiPlayerEvent, ItemBusEvent, NotForwarded, QueueBusEvent,
    };
    use crate::types::{
        FfiAdvanceReason, FfiAudioCodecKind, FfiCancelReason, FfiContainerKind,
        FfiCrossfadeSettings, FfiDecodeErrorClass, FfiDecodeErrorKind, FfiDecoderBackend,
        FfiDecoderChangeCause, FfiEvictReason, FfiFrameDomain, FfiKeyFailureStage, FfiKeySource,
        FfiPlaybackResamplerKind, FfiPlayerStatus, FfiRepeatMode, FfiResamplerKind,
        FfiRouteChangeReason, FfiStretchBackendKind, FfiTimeControlStatus, FfiTotalBytesSource,
        FfiTrackFailureKind, FfiTrackStatus,
    };

    type ItemEventCase<T> = (T, fn(&FfiItemEvent) -> bool);
    type PlayerEventCase<T> = (T, fn(&FfiPlayerEvent) -> bool);

    fn request_id(value: u64) -> RequestId {
        let id = NonZeroU64::new(value).expect("request id must be non-zero");
        RequestId::new(id)
    }

    fn audio_spec(channels: u16, sample_rate: u32) -> AudioSpec {
        AudioSpec::new(
            channels,
            NonZeroU32::new(sample_rate).expect("sample rate must be non-zero"),
        )
    }

    fn item_role(id: u64) -> ItemRole {
        ItemRole::Leading(TrackRef::new(
            TrackId::from(id),
            SlotId::new(0),
            Arc::from("test://track"),
        ))
    }

    #[kithara::test]
    fn event_routes_every_forwarded_domain() {
        let item_events = [
            ItemBusEvent::Decoder(DecoderEvent::ResamplerConfigured {
                backend: ResamplerKind::Rubato,
                input_rate: 44_100,
                output_rate: 48_000,
                channels: 2,
                bypassed: false,
            }),
            ItemBusEvent::Audio(AudioEvent::DecoderReady {
                base_offset: 17,
                variant: Some(2),
            }),
            ItemBusEvent::Hls(HlsEvent::CacheComplete {
                total_bytes: Some(1024),
            }),
            ItemBusEvent::Downloader(DownloaderEvent::RequestStarted {
                request_id: request_id(1),
                wait_in_queue: Duration::from_millis(250),
            }),
            ItemBusEvent::File(FileEvent::CacheComplete { total_bytes: 2048 }),
            ItemBusEvent::Drm(DrmEvent::SegmentDecryptFailed {
                variant: 3,
                segment_index: 4,
                detail: "bad key".into(),
            }),
        ];
        for event in &item_events {
            assert!(FfiItemEvent::try_from(event).is_ok(), "{event:?}");
        }

        let player_events = [
            QueueBusEvent::Engine(EngineEvent::Started),
            QueueBusEvent::Session(SessionEvent::RouteChanged {
                reason: RouteChangeReason::Override,
                previous_route: RouteDescription::default(),
            }),
            QueueBusEvent::Dj(DjEvent::KeylockChanged { on: true }),
            QueueBusEvent::Asset(AssetEvent::Committed {
                asset_root: "cache".into(),
                rel_path: "track/file".into(),
                final_len: Some(99),
            }),
            QueueBusEvent::Asset(AssetEvent::Failed {
                asset_root: "cache".into(),
                rel_path: "track/file".into(),
                reason: "disk full".into(),
            }),
        ];
        for event in &player_events {
            assert!(FfiPlayerEvent::try_from(event).is_ok(), "{event:?}");
        }

        assert!(matches!(
            FfiItemEvent::try_from(&ItemBusEvent::Audio(AudioEvent::OutputAvailable)),
            Err(NotForwarded)
        ));
        assert!(matches!(
            FfiPlayerEvent::try_from(&QueueBusEvent::Queue(QueueEvent::QueueEnded)),
            Err(NotForwarded)
        ));
    }

    #[kithara::test]
    fn decoder_events_preserve_every_forwarded_contract() {
        let cases: [ItemEventCase<DecoderEvent>; 4] = [
            (
                DecoderEvent::DecoderChanged {
                    backend: DecoderBackend::Apple,
                    codec: Some(AudioCodec::AacLc),
                    container: Some(ContainerFormat::Fmp4),
                    sample_rate: 48_000,
                    channels: 2,
                    bit_depth: Some(24),
                    bitrate: Some(320_000),
                    cause: DecoderChangeCause::VariantSwitch,
                    variant: Some(3),
                    base_offset: 4096,
                    duration: Some(Duration::from_millis(2500)),
                    gapless: Some(GaplessInfo::new(2112, 512)),
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::DecoderChanged {
                            backend: FfiDecoderBackend::Apple,
                            codec: Some(FfiAudioCodecKind::AacLc),
                            container: Some(FfiContainerKind::Fmp4),
                            sample_rate: 48_000,
                            channels: 2,
                            bit_depth: Some(24),
                            bitrate: Some(320_000),
                            cause: FfiDecoderChangeCause::VariantSwitch,
                            variant: Some(3),
                            base_offset: 4096,
                            duration_seconds: Some(2.5),
                            gapless_leading: 2112,
                            gapless_trailing: 512,
                            has_gapless: true,
                        }
                    )
                },
            ),
            (
                DecoderEvent::DecodeError {
                    class: DecodeErrorClass::Interrupted,
                    kind: DecodeErrorKind::InvalidData,
                    codec: Some(AudioCodec::Flac),
                    detail: "truncated frame",
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::DecodeError {
                            class: FfiDecodeErrorClass::Interrupted,
                            kind: FfiDecodeErrorKind::InvalidData,
                            codec: Some(FfiAudioCodecKind::Flac),
                            detail,
                        } if detail == "truncated frame"
                    )
                },
            ),
            (
                DecoderEvent::GaplessResolved {
                    leading_frames: 1024,
                    trailing_frames: 256,
                    domain: FrameDomain::Output,
                    codec: Some(AudioCodec::Alac),
                    sample_rate: 44_100,
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::GaplessResolved {
                            leading_frames: 1024,
                            trailing_frames: 256,
                            domain: FfiFrameDomain::Output,
                            codec: Some(FfiAudioCodecKind::Alac),
                            sample_rate: 44_100,
                        }
                    )
                },
            ),
            (
                DecoderEvent::ResamplerConfigured {
                    backend: ResamplerKind::Glide,
                    input_rate: 44_100,
                    output_rate: 48_000,
                    channels: 2,
                    bypassed: false,
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::ResamplerConfigured {
                            backend: FfiResamplerKind::Glide,
                            input_rate: 44_100,
                            output_rate: 48_000,
                            channels: 2,
                            bypassed: false,
                        }
                    )
                },
            ),
        ];

        for (source, preserves_contract) in cases {
            let event = FfiItemEvent::try_from(&source).expect("event must be forwarded");
            assert!(preserves_contract(&event), "unexpected event: {event:?}");
        }
        assert!(matches!(
            FfiItemEvent::try_from(&DecoderEvent::TransitionHold {
                source_exhausted: true,
            }),
            Err(NotForwarded)
        ));
    }

    #[kithara::test]
    fn audio_events_preserve_every_forwarded_contract() {
        let cases: [ItemEventCase<AudioEvent>; 11] = [
            (
                AudioEvent::FormatDetected {
                    spec: audio_spec(2, 48_000),
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::AudioFormatDetected {
                            channels: 2,
                            sample_rate: 48_000,
                        }
                    )
                },
            ),
            (
                AudioEvent::FormatChanged {
                    old: audio_spec(1, 44_100),
                    new: audio_spec(2, 48_000),
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::AudioFormatChanged {
                            old_channels: 1,
                            old_sample_rate: 44_100,
                            new_channels: 2,
                            new_sample_rate: 48_000,
                        }
                    )
                },
            ),
            (
                AudioEvent::SeekComplete {
                    position: Duration::from_millis(1250),
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::SeekComplete {
                            position_seconds: 1.25,
                        }
                    )
                },
            ),
            (
                AudioEvent::SeekRejected {
                    target: Duration::from_millis(1500),
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::SeekRejected {
                            target_seconds: 1.5,
                        }
                    )
                },
            ),
            (
                AudioEvent::DecoderReady {
                    base_offset: 2048,
                    variant: Some(7),
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::DecoderReady {
                            base_offset: 2048,
                            variant: Some(7),
                        }
                    )
                },
            ),
            (
                AudioEvent::TrackFailed {
                    failure: TrackFailureKind::RecreateFailed { offset: 99 },
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::TrackFailed {
                            reason: FfiTrackFailureKind::RecreateFailed { offset: 99 },
                        }
                    )
                },
            ),
            (AudioEvent::UnderrunStarted { position_ms: 600 }, |event| {
                matches!(event, FfiItemEvent::UnderrunStarted { position_ms: 600 })
            }),
            (AudioEvent::UnderrunEnded { position_ms: 700 }, |event| {
                matches!(event, FfiItemEvent::UnderrunEnded { position_ms: 700 })
            }),
            (
                AudioEvent::BufferHealth {
                    buffered_ms: 800,
                    decoded_frontier_ms: 900,
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::BufferHealth {
                            buffered_ms: 800,
                            decoded_frontier_ms: 900,
                        }
                    )
                },
            ),
            (
                AudioEvent::EngineLoad {
                    load: 0.25,
                    ms_per_chunk: 3.5,
                    realtime_factor: 0.5,
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::EngineLoad {
                            load: 0.25,
                            ms_per_chunk: 3.5,
                            realtime_factor: 0.5,
                        }
                    )
                },
            ),
            (
                AudioEvent::PlaybackResamplerConfigured {
                    backend: PlaybackResamplerKind::Rubato,
                    host_sample_rate: 48_000,
                    source_sample_rate: 44_100,
                    active: true,
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::PlaybackResamplerConfigured {
                            backend: FfiPlaybackResamplerKind::Rubato,
                            host_sample_rate: 48_000,
                            source_sample_rate: 44_100,
                            active: true,
                        }
                    )
                },
            ),
        ];

        for (source, preserves_contract) in cases {
            let event = FfiItemEvent::try_from(&source).expect("event must be forwarded");
            assert!(preserves_contract(&event), "unexpected event: {event:?}");
        }
    }

    #[kithara::test]
    fn audio_track_failure_preserves_kind_and_epoch() {
        let cases = [
            (
                TrackFailureKind::Decode {
                    kind: DecodeErrorKind::Io,
                },
                FfiTrackFailureKind::Decode {
                    kind: FfiDecodeErrorKind::Io,
                },
            ),
            (
                TrackFailureKind::Decode {
                    kind: DecodeErrorKind::InvalidData,
                },
                FfiTrackFailureKind::Decode {
                    kind: FfiDecodeErrorKind::InvalidData,
                },
            ),
            (
                TrackFailureKind::RecreateFailed { offset: 4096 },
                FfiTrackFailureKind::RecreateFailed { offset: 4096 },
            ),
            (
                TrackFailureKind::SourceCancelled,
                FfiTrackFailureKind::SourceCancelled,
            ),
            (
                TrackFailureKind::ChannelClosed,
                FfiTrackFailureKind::ChannelClosed,
            ),
            (TrackFailureKind::Render, FfiTrackFailureKind::Render),
        ];

        for (failure, expected) in cases {
            let source = AudioEvent::TrackFailed { failure };
            let event = FfiItemEvent::try_from(&source).expect("event must be forwarded");
            assert!(matches!(
                event,
                FfiItemEvent::TrackFailed { reason } if reason == expected
            ));
        }
    }

    #[kithara::test]
    fn downloader_events_preserve_every_forwarded_contract() {
        let network_error = || kithara::net::NetError::Network("offline".into());
        let cases: [ItemEventCase<DownloaderEvent>; 9] = [
            (
                DownloaderEvent::RequestStarted {
                    request_id: request_id(1),
                    wait_in_queue: Duration::from_millis(250),
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::DownloadStarted {
                            request_id: 1,
                            wait_in_queue_seconds: 0.25
                        }
                    )
                },
            ),
            (
                DownloaderEvent::LoadSlow {
                    request_id: request_id(2),
                    elapsed: Duration::from_millis(500),
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::DownloadSlow {
                            request_id: 2,
                            elapsed_seconds: 0.5
                        }
                    )
                },
            ),
            (
                DownloaderEvent::RequestCompleted {
                    request_id: request_id(3),
                    bytes_transferred: 4096,
                    duration: Duration::from_secs(2),
                    bandwidth_bps: 16_384,
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::DownloadCompleted {
                            request_id: 3,
                            bytes_transferred: 4096,
                            duration_seconds: 2.0,
                            bandwidth_bps: 16_384
                        }
                    )
                },
            ),
            (
                DownloaderEvent::RequestRetrying {
                    request_id: request_id(4),
                    attempt: 2,
                    max_retries: 4,
                    error: network_error(),
                    backoff: Duration::from_millis(750),
                },
                |event| matches!(event, FfiItemEvent::DownloadRetrying { request_id: 4, attempt: 2, max_retries: 4, error, backoff_seconds: 0.75 } if error.contains("offline")),
            ),
            (
                DownloaderEvent::BodyStalled {
                    request_id: request_id(5),
                    consumed: 1024,
                    expected: Some(4096),
                    stall: Duration::from_millis(125),
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::DownloadBodyStalled {
                            request_id: 5,
                            consumed: 1024,
                            expected: Some(4096),
                            stall_seconds: 0.125
                        }
                    )
                },
            ),
            (
                DownloaderEvent::BodyResumed {
                    request_id: request_id(6),
                    resume_number: 3,
                    from_offset: 1024,
                    honoured_range: true,
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::DownloadBodyResumed {
                            request_id: 6,
                            resume_number: 3,
                            from_offset: 1024,
                            honoured_range: true
                        }
                    )
                },
            ),
            (
                DownloaderEvent::RetryExhausted {
                    request_id: request_id(7),
                    max_retries: 5,
                    consumed: 2048,
                    error: network_error(),
                },
                |event| matches!(event, FfiItemEvent::DownloadRetryExhausted { request_id: 7, max_retries: 5, consumed: 2048, error } if error.contains("offline")),
            ),
            (
                DownloaderEvent::FirstByte {
                    request_id: request_id(8),
                    ttfb: Duration::from_millis(80),
                    status: 206,
                    partial: true,
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::DownloadFirstByte {
                            request_id: 8,
                            ttfb_seconds: 0.08,
                            status: 206,
                            partial: true
                        }
                    )
                },
            ),
            (
                DownloaderEvent::RequestCancelled {
                    request_id: request_id(9),
                    reason: CancelReason::EpochCancel,
                    bytes_transferred: 512,
                },
                |event| {
                    matches!(
                        event,
                        FfiItemEvent::DownloadCancelled {
                            request_id: 9,
                            reason: FfiCancelReason::EpochCancel,
                            bytes_transferred: 512
                        }
                    )
                },
            ),
        ];

        for (source, preserves_contract) in cases {
            let event = FfiItemEvent::try_from(&source).expect("event must be forwarded");
            assert!(preserves_contract(&event), "unexpected event: {event:?}");
        }
    }

    #[kithara::test]
    fn player_events_preserve_every_forwarded_contract() {
        let cases: [PlayerEventCase<PlayerEvent>; 7] = [
            (PlayerEvent::RateChanged { rate: 1.25 }, |event| {
                matches!(event, FfiPlayerEvent::RateChanged { rate: 1.25 })
            }),
            (
                PlayerEvent::StatusChanged {
                    status: PlayerStatus::ReadyToPlay,
                },
                |event| {
                    matches!(
                        event,
                        FfiPlayerEvent::StatusChanged {
                            status: FfiPlayerStatus::ReadyToPlay
                        }
                    )
                },
            ),
            (
                PlayerEvent::TimeControlStatusChanged {
                    status: TimeControlStatus::Playing,
                    reason: None,
                },
                |event| {
                    matches!(
                        event,
                        FfiPlayerEvent::TimeControlStatusChanged {
                            status: FfiTimeControlStatus::Playing
                        }
                    )
                },
            ),
            (PlayerEvent::VolumeChanged { volume: 0.75 }, |event| {
                matches!(event, FfiPlayerEvent::VolumeChanged { volume: 0.75 })
            }),
            (PlayerEvent::MuteChanged { muted: true }, |event| {
                matches!(event, FfiPlayerEvent::MuteChanged { muted: true })
            }),
            (
                PlayerEvent::ItemDidPlayToEnd {
                    item: item_role(10),
                },
                |event| matches!(event, FfiPlayerEvent::ItemDidPlayToEnd),
            ),
            (
                PlayerEvent::ItemDidFail {
                    item: item_role(11),
                    fault: PlaybackFault::Source(TrackFailureKind::Decode {
                        kind: DecodeErrorKind::InvalidData,
                    }),
                },
                |event| matches!(event, FfiPlayerEvent::ItemDidFail { item_id: Some(id) } if *id == TrackId::from(11_u64)),
            ),
        ];

        for (source, preserves_contract) in cases {
            let event = FfiPlayerEvent::try_from(&source).expect("event must be forwarded");
            assert!(preserves_contract(&event), "unexpected event: {event:?}");
        }
        assert!(matches!(
            FfiPlayerEvent::try_from(&PlayerEvent::CurrentItemChanged { item: None }),
            Err(NotForwarded)
        ));
    }

    #[kithara::test]
    fn queue_events_preserve_every_forwarded_contract() {
        let id = TrackId::from(21_u64);
        let cases: [PlayerEventCase<QueueEvent>; 11] = [
            (
                QueueEvent::TrackAdded { id, index: 2 },
                |event| matches!(event, FfiPlayerEvent::TrackAdded { item_id, index: 2 } if *item_id == TrackId::from(21_u64)),
            ),
            (
                QueueEvent::TrackRemoved { id },
                |event| matches!(event, FfiPlayerEvent::TrackRemoved { item_id } if *item_id == TrackId::from(21_u64)),
            ),
            (
                QueueEvent::TrackStatusChanged {
                    id,
                    status: TrackStatus::Loaded,
                },
                |event| matches!(event, FfiPlayerEvent::TrackStatusChanged { item_id, status: FfiTrackStatus::Loaded } if *item_id == TrackId::from(21_u64)),
            ),
            (
                QueueEvent::CurrentTrackChanged { id: Some(id) },
                |event| matches!(event, FfiPlayerEvent::CurrentItemChanged { item_id: Some(item_id) } if *item_id == TrackId::from(21_u64)),
            ),
            (
                QueueEvent::CurrentTrackAdvance {
                    id: Some(id),
                    reason: kithara::queue::AdvanceReason::NaturalEof,
                },
                |event| matches!(event, FfiPlayerEvent::CurrentItemAdvanced { item_id: Some(item_id), reason: FfiAdvanceReason::NaturalEof } if *item_id == TrackId::from(21_u64)),
            ),
            (QueueEvent::QueueEnded, |event| {
                matches!(event, FfiPlayerEvent::QueueEnded)
            }),
            (
                QueueEvent::TrackLoadFailed {
                    id,
                    reason: "decode failed".into(),
                    auto_skipped: true,
                },
                |event| matches!(event, FfiPlayerEvent::TrackLoadFailed { item_id, reason, auto_skipped: true } if *item_id == TrackId::from(21_u64) && reason == "decode failed"),
            ),
            (
                QueueEvent::CrossfadeStarted {
                    settings: kithara::play::CrossfadeSettings {
                        duration: 3.5,
                        ..Default::default()
                    },
                },
                |event| {
                    matches!(
                        event,
                        FfiPlayerEvent::CrossfadeStarted {
                            settings: FfiCrossfadeSettings { duration: 3.5, .. }
                        }
                    )
                },
            ),
            (
                QueueEvent::CrossfadeSettingsChanged {
                    settings: kithara::play::CrossfadeSettings {
                        duration: 4.0,
                        ..Default::default()
                    },
                },
                |event| {
                    matches!(
                        event,
                        FfiPlayerEvent::CrossfadeSettingsChanged {
                            settings: FfiCrossfadeSettings { duration: 4.0, .. }
                        }
                    )
                },
            ),
            (
                QueueEvent::RepeatModeChanged {
                    mode: QueueRepeatMode::All,
                },
                |event| {
                    matches!(
                        event,
                        FfiPlayerEvent::RepeatModeChanged {
                            mode: FfiRepeatMode::All
                        }
                    )
                },
            ),
            (
                QueueEvent::NextTrackReady { id, index: 3 },
                |event| matches!(event, FfiPlayerEvent::NextTrackReady { item_id, index: 3 } if *item_id == TrackId::from(21_u64)),
            ),
        ];

        for (source, preserves_contract) in cases {
            let event = FfiPlayerEvent::try_from(&source).expect("event must be forwarded");
            assert!(preserves_contract(&event), "unexpected event: {event:?}");
        }
        assert!(matches!(
            FfiPlayerEvent::try_from(&QueueEvent::TrackMetadataChanged { id }),
            Err(NotForwarded)
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
            error: kithara::net::NetError::Network("boom".into()),
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
    fn decoder_end_of_stream_is_not_duplicated() {
        let event = AudioEvent::EndOfStream;

        assert!(matches!(FfiItemEvent::try_from(&event), Err(NotForwarded)));
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
                from: SlotId::new(1),
                to: SlotId::new(2),
                duration: Duration::from_secs(1),
            }),
            Err(NotForwarded)
        ));
        assert!(matches!(
            FfiPlayerEvent::try_from(&EngineEvent::CrossfadeProgress {
                from: SlotId::new(1),
                to: SlotId::new(2),
                progress: 0.5,
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
    fn stretch_backend_kind_from_maps_bungee() {
        assert_eq!(
            FfiStretchBackendKind::from(StretchBackendKind::Bungee),
            FfiStretchBackendKind::Bungee
        );
    }

    #[kithara::test]
    fn dj_event_to_ffi_skips_beat_tick() {
        assert!(matches!(
            FfiPlayerEvent::try_from(&DjEvent::BeatTick {
                slot: SlotId::new(9),
                beat_number: 4,
                timestamp: MediaTime::default(),
            }),
            Err(NotForwarded)
        ));
    }

    #[kithara::test]
    fn dj_event_to_ffi_maps_bpm_detected_fields() {
        assert!(matches!(
            FfiPlayerEvent::try_from(&DjEvent::BpmDetected {
                slot: SlotId::new(7),
                info: kithara::play::BpmInfo::new(128.5, Some(0.8), Duration::from_millis(250)),
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
            FfiPlayerEvent::try_from(&DjEvent::StretchBackendChanged {
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
            item: ItemRole::Leading(TrackRef::new(
                TrackId::from(7_u64),
                SlotId::new(0),
                "src".into(),
            )),
            fault: PlaybackFault::Source(TrackFailureKind::Decode {
                kind: DecodeErrorKind::InvalidData,
            }),
        };

        assert!(matches!(
            FfiPlayerEvent::try_from(&event),
            Ok(FfiPlayerEvent::ItemDidFail { item_id: Some(id) }) if id == TrackId::from(7_u64)
        ));
    }

    #[kithara::test]
    fn queue_event_to_ffi_maps_repeat_mode() {
        let event = QueueEvent::RepeatModeChanged {
            mode: QueueRepeatMode::All,
        };

        assert!(matches!(
            FfiPlayerEvent::try_from(&event),
            Ok(FfiPlayerEvent::RepeatModeChanged {
                mode: FfiRepeatMode::All
            })
        ));
    }

    #[kithara::test]
    fn event_to_ffi_error_skips_request_failed() {
        let event = ItemBusEvent::Downloader(DownloaderEvent::RequestFailed {
            request_id: request_id(13),
            error: kithara::net::NetError::Network("boom".into()),
            retryable: false,
        });

        assert!(matches!(FfiError::try_from(&event), Err(NotForwarded)));
    }
}
