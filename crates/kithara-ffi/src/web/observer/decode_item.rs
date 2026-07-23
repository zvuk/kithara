use js_sys::{Array, Reflect};
use wasm_bindgen::{JsCast, JsValue};

use super::marshal::{discriminant, get_bool, get_f64, get_str, narrow_f32};
use crate::types::{
    FfiAudioCodecKind, FfiCancelReason, FfiContainerKind, FfiDecodeErrorClass, FfiDecodeErrorKind,
    FfiDecoderBackend, FfiDecoderChangeCause, FfiFrameDomain, FfiItemEvent, FfiItemStatus,
    FfiKeyFailureStage, FfiKeySource, FfiPlaybackResamplerKind, FfiResamplerKind, FfiTimeRange,
    FfiTotalBytesSource, FfiTrackFailureKind, FfiVariant,
};

pub(crate) fn decode_item_event(data: &JsValue) -> Option<FfiItemEvent> {
    let kind = get_str(data, "kind")?;
    Some(match kind.as_str() {
        "DurationChanged" => FfiItemEvent::DurationChanged {
            seconds: get_f64(data, "seconds")?,
        },
        "LoadedRangesChanged" => FfiItemEvent::LoadedRangesChanged {
            ranges: decode_time_ranges(data, "ranges")?,
        },
        "StatusChanged" => FfiItemEvent::StatusChanged {
            status: ItemDecode::decode_item_status(get_f64(data, "status")?),
        },
        "VariantsDiscovered" => FfiItemEvent::VariantsDiscovered {
            variants: decode_variants(data, "variants")?,
        },
        "VariantSelected" => FfiItemEvent::VariantSelected {
            variant: decode_variant(&Reflect::get(data, &JsValue::from_str("variant")).ok()?)?,
        },
        "VariantApplied" => FfiItemEvent::VariantApplied {
            variant: decode_variant(&Reflect::get(data, &JsValue::from_str("variant")).ok()?)?,
        },
        "DidReachEnd" => FfiItemEvent::DidReachEnd,
        "DidFail" => FfiItemEvent::DidFail,
        "DidStall" => FfiItemEvent::DidStall,
        "Error" => FfiItemEvent::Error {
            error: get_str(data, "error").unwrap_or_default(),
        },
        "DecoderChanged" => FfiItemEvent::DecoderChanged {
            backend: ItemDecode::decode_decoder_backend(get_str(data, "backend")),
            codec: get_str(data, "codec")
                .map(|value| ItemDecode::decode_audio_codec_kind(Some(value))),
            container: get_str(data, "container")
                .map(|value| ItemDecode::decode_container_kind(Some(value))),
            sample_rate: ItemDecode::narrow_u32(get_f64(data, "sample_rate")?),
            channels: ItemDecode::narrow_u16(get_f64(data, "channels")?),
            bit_depth: get_f64(data, "bit_depth").map(ItemDecode::narrow_u16),
            bitrate: get_f64(data, "bitrate").map(ItemDecode::narrow_u32),
            epoch: ItemDecode::narrow_u64(get_f64(data, "epoch")?),
            cause: ItemDecode::decode_decoder_change_cause(get_str(data, "cause")),
            variant: get_f64(data, "variant").map(ItemDecode::narrow_u32),
            base_offset: ItemDecode::narrow_u64(get_f64(data, "base_offset")?),
            duration_seconds: get_f64(data, "duration_seconds"),
            gapless_leading: ItemDecode::narrow_u64(get_f64(data, "gapless_leading")?),
            gapless_trailing: ItemDecode::narrow_u64(get_f64(data, "gapless_trailing")?),
            has_gapless: get_bool(data, "has_gapless")?,
        },
        "DecodeError" => FfiItemEvent::DecodeError {
            class: ItemDecode::decode_decode_error_class(get_str(data, "class")),
            kind: ItemDecode::decode_decode_error_kind(get_str(data, "decode_kind")),
            codec: get_str(data, "codec")
                .map(|value| ItemDecode::decode_audio_codec_kind(Some(value))),
            detail: get_str(data, "detail").unwrap_or_default(),
        },
        "GaplessResolved" => FfiItemEvent::GaplessResolved {
            leading_frames: ItemDecode::narrow_u64(get_f64(data, "leading_frames")?),
            trailing_frames: ItemDecode::narrow_u64(get_f64(data, "trailing_frames")?),
            domain: ItemDecode::decode_frame_domain(get_str(data, "domain")),
            codec: get_str(data, "codec")
                .map(|value| ItemDecode::decode_audio_codec_kind(Some(value))),
            sample_rate: ItemDecode::narrow_u32(get_f64(data, "sample_rate")?),
        },
        "ResamplerConfigured" => FfiItemEvent::ResamplerConfigured {
            backend: ItemDecode::decode_resampler_kind(get_str(data, "backend")),
            input_rate: ItemDecode::narrow_u32(get_f64(data, "input_rate")?),
            output_rate: ItemDecode::narrow_u32(get_f64(data, "output_rate")?),
            channels: ItemDecode::narrow_u16(get_f64(data, "channels")?),
            bypassed: get_bool(data, "bypassed")?,
        },
        "AudioFormatDetected" => FfiItemEvent::AudioFormatDetected {
            channels: ItemDecode::narrow_u16(get_f64(data, "channels")?),
            sample_rate: ItemDecode::narrow_u32(get_f64(data, "sample_rate")?),
        },
        "AudioFormatChanged" => FfiItemEvent::AudioFormatChanged {
            old_channels: ItemDecode::narrow_u16(get_f64(data, "old_channels")?),
            old_sample_rate: ItemDecode::narrow_u32(get_f64(data, "old_sample_rate")?),
            new_channels: ItemDecode::narrow_u16(get_f64(data, "new_channels")?),
            new_sample_rate: ItemDecode::narrow_u32(get_f64(data, "new_sample_rate")?),
        },
        "SeekComplete" => FfiItemEvent::SeekComplete {
            position_seconds: get_f64(data, "position_seconds")?,
            epoch: ItemDecode::narrow_u64(get_f64(data, "epoch")?),
        },
        "SeekRejected" => FfiItemEvent::SeekRejected {
            epoch: ItemDecode::narrow_u64(get_f64(data, "epoch")?),
            target_seconds: get_f64(data, "target_seconds")?,
        },
        "DecoderReady" => FfiItemEvent::DecoderReady {
            base_offset: ItemDecode::narrow_u64(get_f64(data, "base_offset")?),
            variant: get_f64(data, "variant").map(ItemDecode::narrow_u32),
        },
        "TrackFailed" => FfiItemEvent::TrackFailed {
            reason: ItemDecode::decode_track_failure_kind(data)?,
            epoch: ItemDecode::narrow_u64(get_f64(data, "epoch")?),
        },
        "UnderrunStarted" => FfiItemEvent::UnderrunStarted {
            position_ms: ItemDecode::narrow_u64(get_f64(data, "position_ms")?),
            epoch: ItemDecode::narrow_u64(get_f64(data, "epoch")?),
        },
        "UnderrunEnded" => FfiItemEvent::UnderrunEnded {
            position_ms: ItemDecode::narrow_u64(get_f64(data, "position_ms")?),
            epoch: ItemDecode::narrow_u64(get_f64(data, "epoch")?),
        },
        "BufferHealth" => FfiItemEvent::BufferHealth {
            buffered_ms: ItemDecode::narrow_u64(get_f64(data, "buffered_ms")?),
            decoded_frontier_ms: ItemDecode::narrow_u64(get_f64(data, "decoded_frontier_ms")?),
            epoch: ItemDecode::narrow_u64(get_f64(data, "epoch")?),
        },
        "EngineLoad" => FfiItemEvent::EngineLoad {
            load: narrow_f32(get_f64(data, "load")?),
            ms_per_chunk: narrow_f32(get_f64(data, "ms_per_chunk")?),
            realtime_factor: narrow_f32(get_f64(data, "realtime_factor")?),
        },
        "PlaybackResamplerConfigured" => FfiItemEvent::PlaybackResamplerConfigured {
            backend: ItemDecode::decode_playback_resampler_kind(get_str(data, "backend")),
            host_sample_rate: ItemDecode::narrow_u32(get_f64(data, "host_sample_rate")?),
            source_sample_rate: ItemDecode::narrow_u32(get_f64(data, "source_sample_rate")?),
            active: get_bool(data, "active")?,
        },
        "HlsVariantSwitchFenced" => FfiItemEvent::HlsVariantSwitchFenced {
            from_variant: ItemDecode::narrow_u32(get_f64(data, "from_variant")?),
            to_variant: ItemDecode::narrow_u32(get_f64(data, "to_variant")?),
            cross_codec: get_bool(data, "cross_codec")?,
        },
        "HlsVariantSwitchAcked" => FfiItemEvent::HlsVariantSwitchAcked {
            variant: ItemDecode::narrow_u32(get_f64(data, "variant")?),
            generation: ItemDecode::narrow_u64(get_f64(data, "generation")?),
        },
        "HlsCacheComplete" => FfiItemEvent::HlsCacheComplete {
            total_bytes: get_f64(data, "total_bytes").map(ItemDecode::narrow_u64),
        },
        "DownloadStarted" => FfiItemEvent::DownloadStarted {
            request_id: ItemDecode::narrow_u64(get_f64(data, "request_id")?),
            wait_in_queue_seconds: get_f64(data, "wait_in_queue_seconds")?,
        },
        "DownloadSlow" => FfiItemEvent::DownloadSlow {
            request_id: ItemDecode::narrow_u64(get_f64(data, "request_id")?),
            elapsed_seconds: get_f64(data, "elapsed_seconds")?,
        },
        "DownloadCompleted" => FfiItemEvent::DownloadCompleted {
            request_id: ItemDecode::narrow_u64(get_f64(data, "request_id")?),
            bytes_transferred: ItemDecode::narrow_u64(get_f64(data, "bytes_transferred")?),
            duration_seconds: get_f64(data, "duration_seconds")?,
            bandwidth_bps: ItemDecode::narrow_u64(get_f64(data, "bandwidth_bps")?),
        },
        "DownloadRetrying" => FfiItemEvent::DownloadRetrying {
            request_id: ItemDecode::narrow_u64(get_f64(data, "request_id")?),
            attempt: ItemDecode::narrow_u32(get_f64(data, "attempt")?),
            max_retries: ItemDecode::narrow_u32(get_f64(data, "max_retries")?),
            error: get_str(data, "error").unwrap_or_default(),
            backoff_seconds: get_f64(data, "backoff_seconds")?,
        },
        "DownloadBodyStalled" => FfiItemEvent::DownloadBodyStalled {
            request_id: ItemDecode::narrow_u64(get_f64(data, "request_id")?),
            consumed: ItemDecode::narrow_u64(get_f64(data, "consumed")?),
            expected: get_f64(data, "expected").map(ItemDecode::narrow_u64),
            stall_seconds: get_f64(data, "stall_seconds")?,
        },
        "DownloadBodyResumed" => FfiItemEvent::DownloadBodyResumed {
            request_id: ItemDecode::narrow_u64(get_f64(data, "request_id")?),
            resume_number: ItemDecode::narrow_u32(get_f64(data, "resume_number")?),
            from_offset: ItemDecode::narrow_u64(get_f64(data, "from_offset")?),
            honoured_range: get_bool(data, "honoured_range")?,
        },
        "DownloadRetryExhausted" => FfiItemEvent::DownloadRetryExhausted {
            request_id: ItemDecode::narrow_u64(get_f64(data, "request_id")?),
            max_retries: ItemDecode::narrow_u32(get_f64(data, "max_retries")?),
            consumed: ItemDecode::narrow_u64(get_f64(data, "consumed")?),
            error: get_str(data, "error").unwrap_or_default(),
        },
        "DownloadFirstByte" => FfiItemEvent::DownloadFirstByte {
            request_id: ItemDecode::narrow_u64(get_f64(data, "request_id")?),
            ttfb_seconds: get_f64(data, "ttfb_seconds")?,
            status: ItemDecode::narrow_u16(get_f64(data, "status")?),
            partial: get_bool(data, "partial")?,
        },
        "DownloadCancelled" => FfiItemEvent::DownloadCancelled {
            request_id: ItemDecode::narrow_u64(get_f64(data, "request_id")?),
            reason: ItemDecode::decode_cancel_reason(get_str(data, "reason"))?,
            bytes_transferred: ItemDecode::narrow_u64(get_f64(data, "bytes_transferred")?),
        },
        "FileOpened" => FfiItemEvent::FileOpened {
            codec: get_str(data, "codec")
                .map(|value| ItemDecode::decode_audio_codec_kind(Some(value))),
            container: get_str(data, "container")
                .map(|value| ItemDecode::decode_container_kind(Some(value))),
            total_bytes: get_f64(data, "total_bytes").map(ItemDecode::narrow_u64),
            cached: get_bool(data, "cached")?,
        },
        "FileTotalBytesResolved" => FfiItemEvent::FileTotalBytesResolved {
            total_bytes: ItemDecode::narrow_u64(get_f64(data, "total_bytes")?),
            source: ItemDecode::decode_total_bytes_source(get_str(data, "source")),
        },
        "FileCacheComplete" => FfiItemEvent::FileCacheComplete {
            total_bytes: ItemDecode::narrow_u64(get_f64(data, "total_bytes")?),
        },
        "DrmKeyFetchFailed" => FfiItemEvent::DrmKeyFetchFailed {
            key_host: get_str(data, "key_host"),
            stage: ItemDecode::decode_key_failure_stage(get_str(data, "stage")),
            detail: get_str(data, "detail").unwrap_or_default(),
        },
        "DrmKeyAcquired" => FfiItemEvent::DrmKeyAcquired {
            key_host: get_str(data, "key_host"),
            source: ItemDecode::decode_key_source(get_str(data, "source")),
            bytes: ItemDecode::narrow_u64(get_f64(data, "bytes")?),
            latency_ms: get_f64(data, "latency_ms").map(ItemDecode::narrow_u64),
        },
        "DrmSegmentDecryptFailed" => FfiItemEvent::DrmSegmentDecryptFailed {
            variant: ItemDecode::narrow_u32(get_f64(data, "variant")?),
            segment_index: ItemDecode::narrow_u32(get_f64(data, "segment_index")?),
            detail: get_str(data, "detail").unwrap_or_default(),
        },
        _ => return None,
    })
}

fn decode_variants(data: &JsValue, key: &str) -> Option<Vec<FfiVariant>> {
    let arr = Reflect::get(data, &JsValue::from_str(key))
        .ok()?
        .dyn_into::<Array>()
        .ok()?;
    let mut variants = Vec::with_capacity(arr.length() as usize);
    for value in arr.iter() {
        variants.push(decode_variant(&value)?);
    }
    Some(variants)
}

fn decode_variant(value: &JsValue) -> Option<FfiVariant> {
    Some(FfiVariant {
        name: get_str(value, "name"),
        index: ItemDecode::narrow_u32(get_f64(value, "index")?),
        bandwidth_bps: ItemDecode::narrow_u64(get_f64(value, "bandwidth_bps")?),
    })
}

fn decode_time_ranges(data: &JsValue, key: &str) -> Option<Vec<FfiTimeRange>> {
    let arr = Reflect::get(data, &JsValue::from_str(key))
        .ok()?
        .dyn_into::<Array>()
        .ok()?;
    let mut ranges = Vec::with_capacity(arr.length() as usize);
    for value in arr.iter() {
        ranges.push(decode_time_range(&value)?);
    }
    Some(ranges)
}

fn decode_time_range(value: &JsValue) -> Option<FfiTimeRange> {
    Some(FfiTimeRange {
        start_seconds: get_f64(value, "start_seconds")?,
        duration_seconds: get_f64(value, "duration_seconds")?,
    })
}

struct ItemDecode;

impl ItemDecode {
    fn narrow_u16(value: f64) -> u16 {
        num_traits::cast(value).unwrap_or(0)
    }

    fn narrow_u32(value: f64) -> u32 {
        num_traits::cast(value).unwrap_or(0)
    }

    fn narrow_u64(value: f64) -> u64 {
        num_traits::cast(value.max(0.0)).unwrap_or(0)
    }

    fn decode_item_status(code: f64) -> FfiItemStatus {
        match discriminant(code) {
            1 => FfiItemStatus::ReadyToPlay,
            2 => FfiItemStatus::Failed,
            _ => FfiItemStatus::Unknown,
        }
    }

    fn decode_audio_codec_kind(value: Option<String>) -> FfiAudioCodecKind {
        match value.as_deref() {
            Some("AacLc") => FfiAudioCodecKind::AacLc,
            Some("AacHe") => FfiAudioCodecKind::AacHe,
            Some("AacHeV2") => FfiAudioCodecKind::AacHeV2,
            Some("Mp3") => FfiAudioCodecKind::Mp3,
            Some("Flac") => FfiAudioCodecKind::Flac,
            Some("Vorbis") => FfiAudioCodecKind::Vorbis,
            Some("Opus") => FfiAudioCodecKind::Opus,
            Some("Alac") => FfiAudioCodecKind::Alac,
            Some("Pcm") => FfiAudioCodecKind::Pcm,
            Some("Adpcm") => FfiAudioCodecKind::Adpcm,
            _ => FfiAudioCodecKind::Unknown,
        }
    }

    fn decode_container_kind(value: Option<String>) -> FfiContainerKind {
        match value.as_deref() {
            Some("Mp4") => FfiContainerKind::Mp4,
            Some("Fmp4") => FfiContainerKind::Fmp4,
            Some("MpegTs") => FfiContainerKind::MpegTs,
            Some("MpegAudio") => FfiContainerKind::MpegAudio,
            Some("Adts") => FfiContainerKind::Adts,
            Some("Flac") => FfiContainerKind::Flac,
            Some("Wav") => FfiContainerKind::Wav,
            Some("Ogg") => FfiContainerKind::Ogg,
            Some("Caf") => FfiContainerKind::Caf,
            Some("Mkv") => FfiContainerKind::Mkv,
            _ => FfiContainerKind::Unknown,
        }
    }

    fn decode_decoder_backend(value: Option<String>) -> FfiDecoderBackend {
        match value.as_deref() {
            Some("Symphonia") => FfiDecoderBackend::Symphonia,
            Some("Apple") => FfiDecoderBackend::Apple,
            Some("Android") => FfiDecoderBackend::Android,
            _ => FfiDecoderBackend::Unknown,
        }
    }

    fn decode_decoder_change_cause(value: Option<String>) -> FfiDecoderChangeCause {
        match value.as_deref() {
            Some("Initial") => FfiDecoderChangeCause::Initial,
            Some("VariantSwitch") => FfiDecoderChangeCause::VariantSwitch,
            Some("FormatBoundary") => FfiDecoderChangeCause::FormatBoundary,
            Some("SeekRecreate") => FfiDecoderChangeCause::SeekRecreate,
            Some("Recovery") => FfiDecoderChangeCause::Recovery,
            Some("HostRateChange") => FfiDecoderChangeCause::HostRateChange,
            _ => FfiDecoderChangeCause::Unknown,
        }
    }

    fn decode_decode_error_class(value: Option<String>) -> FfiDecodeErrorClass {
        match value.as_deref() {
            Some("Interrupted") => FfiDecodeErrorClass::Interrupted,
            Some("VariantChange") => FfiDecodeErrorClass::VariantChange,
            Some("Other") => FfiDecodeErrorClass::Other,
            _ => FfiDecodeErrorClass::Unknown,
        }
    }

    fn decode_decode_error_kind(value: Option<String>) -> FfiDecodeErrorKind {
        match value.as_deref() {
            Some("Io") => FfiDecodeErrorKind::Io,
            Some("UnsupportedCodec") => FfiDecodeErrorKind::UnsupportedCodec,
            Some("UnsupportedContainer") => FfiDecodeErrorKind::UnsupportedContainer,
            Some("InvalidData") => FfiDecodeErrorKind::InvalidData,
            Some("SeekFailed") => FfiDecodeErrorKind::SeekFailed,
            Some("SeekOutOfRange") => FfiDecodeErrorKind::SeekOutOfRange,
            Some("Parse") => FfiDecodeErrorKind::Parse,
            Some("ProbeFailed") => FfiDecodeErrorKind::ProbeFailed,
            Some("BackendUnavailable") => FfiDecodeErrorKind::BackendUnavailable,
            Some("InvalidSampleRate") => FfiDecodeErrorKind::InvalidSampleRate,
            Some("BackendStatus") => FfiDecodeErrorKind::BackendStatus,
            Some("Interrupted") => FfiDecodeErrorKind::Interrupted,
            Some("Backend") => FfiDecodeErrorKind::Backend,
            _ => FfiDecodeErrorKind::Unknown,
        }
    }

    fn decode_frame_domain(value: Option<String>) -> FfiFrameDomain {
        match value.as_deref() {
            Some("Source") => FfiFrameDomain::Source,
            Some("Output") => FfiFrameDomain::Output,
            _ => FfiFrameDomain::Unknown,
        }
    }

    fn decode_resampler_kind(value: Option<String>) -> FfiResamplerKind {
        match value.as_deref() {
            Some("Rubato") => FfiResamplerKind::Rubato,
            Some("Apple") => FfiResamplerKind::Apple,
            Some("Glide") => FfiResamplerKind::Glide,
            Some("None") => FfiResamplerKind::None,
            _ => FfiResamplerKind::Unknown,
        }
    }

    fn decode_track_failure_kind(data: &JsValue) -> Option<FfiTrackFailureKind> {
        let value = get_str(data, "reason");
        Some(match value.as_deref() {
            Some("Decode") => FfiTrackFailureKind::Decode,
            Some("RecreateFailed") => FfiTrackFailureKind::RecreateFailed {
                offset: Self::narrow_u64(get_f64(data, "offset")?),
            },
            Some("SourceCancelled") => FfiTrackFailureKind::SourceCancelled,
            Some("Unknown") | None => FfiTrackFailureKind::Unknown,
            Some(_) => FfiTrackFailureKind::Unknown,
        })
    }

    fn decode_playback_resampler_kind(value: Option<String>) -> FfiPlaybackResamplerKind {
        match value.as_deref() {
            Some("Rubato") => FfiPlaybackResamplerKind::Rubato,
            Some("Glide") => FfiPlaybackResamplerKind::Glide,
            Some("None") => FfiPlaybackResamplerKind::None,
            _ => FfiPlaybackResamplerKind::Unknown,
        }
    }

    fn decode_cancel_reason(value: Option<String>) -> Option<FfiCancelReason> {
        match value.as_deref() {
            Some("EpochCancel") => Some(FfiCancelReason::EpochCancel),
            Some("PeerCancel") => Some(FfiCancelReason::PeerCancel),
            Some("DownloaderShutdown") => Some(FfiCancelReason::DownloaderShutdown),
            Some("BeforeStart") => Some(FfiCancelReason::BeforeStart),
            _ => None,
        }
    }

    fn decode_total_bytes_source(value: Option<String>) -> FfiTotalBytesSource {
        match value.as_deref() {
            Some("CommittedLen") => FfiTotalBytesSource::CommittedLen,
            Some("ContentLength") => FfiTotalBytesSource::ContentLength,
            _ => FfiTotalBytesSource::Unknown,
        }
    }

    fn decode_key_failure_stage(value: Option<String>) -> FfiKeyFailureStage {
        match value.as_deref() {
            Some("Network") => FfiKeyFailureStage::Network,
            Some("BodyCollect") => FfiKeyFailureStage::BodyCollect,
            Some("Processor") => FfiKeyFailureStage::Processor,
            Some("Missing") => FfiKeyFailureStage::Missing,
            _ => FfiKeyFailureStage::Unknown,
        }
    }

    fn decode_key_source(value: Option<String>) -> FfiKeySource {
        match value.as_deref() {
            Some("Network") => FfiKeySource::Network,
            Some("DiskCache") => FfiKeySource::DiskCache,
            Some("MemCache") => FfiKeySource::MemCache,
            _ => FfiKeySource::Unknown,
        }
    }
}
