use js_sys::{Array, Object, Reflect};
use wasm_bindgen::JsValue;

use super::marshal::{KIND, set_bool, set_f64, set_opt_f64, set_opt_str, set_str};
use crate::types::{
    FfiAudioCodecKind, FfiCancelReason, FfiContainerKind, FfiDecodeErrorClass, FfiDecodeErrorKind,
    FfiDecoderBackend, FfiDecoderChangeCause, FfiFrameDomain, FfiItemEvent, FfiItemStatus,
    FfiKeyFailureStage, FfiKeySource, FfiPlaybackResamplerKind, FfiResamplerKind, FfiTimeRange,
    FfiTotalBytesSource, FfiTrackFailureKind, FfiVariant,
};

pub(crate) fn encode_item_event(event: &FfiItemEvent) -> JsValue {
    let obj = Object::new();
    match event {
        FfiItemEvent::DurationChanged { seconds } => {
            set_str(&obj, KIND, "DurationChanged");
            set_f64(&obj, "seconds", *seconds);
        }
        FfiItemEvent::LoadedRangesChanged { ranges } => {
            set_str(&obj, KIND, "LoadedRangesChanged");
            let arr = Array::new();
            for range in ranges {
                arr.push(&range_to_js(range));
            }
            let _ = Reflect::set(&obj, &JsValue::from_str("ranges"), &arr);
        }
        FfiItemEvent::StatusChanged { status } => {
            set_str(&obj, KIND, "StatusChanged");
            set_f64(&obj, "status", item_status_code(*status));
        }
        FfiItemEvent::VariantsDiscovered { variants } => {
            set_str(&obj, KIND, "VariantsDiscovered");
            let arr = Array::new();
            for variant in variants {
                arr.push(&variant_to_js(variant));
            }
            let _ = Reflect::set(&obj, &JsValue::from_str("variants"), &arr);
        }
        FfiItemEvent::VariantSelected { variant } => {
            set_str(&obj, KIND, "VariantSelected");
            let _ = Reflect::set(&obj, &JsValue::from_str("variant"), &variant_to_js(variant));
        }
        FfiItemEvent::VariantApplied { variant } => {
            set_str(&obj, KIND, "VariantApplied");
            let _ = Reflect::set(&obj, &JsValue::from_str("variant"), &variant_to_js(variant));
        }
        FfiItemEvent::DidReachEnd => set_str(&obj, KIND, "DidReachEnd"),
        FfiItemEvent::DidFail => set_str(&obj, KIND, "DidFail"),
        FfiItemEvent::DidStall => set_str(&obj, KIND, "DidStall"),
        FfiItemEvent::Error { error } => {
            set_str(&obj, KIND, "Error");
            set_str(&obj, "error", error);
        }
        FfiItemEvent::DecoderChanged {
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
            duration_seconds,
            gapless_leading,
            gapless_trailing,
            has_gapless,
        } => {
            set_str(&obj, KIND, "DecoderChanged");
            set_str(&obj, "backend", decoder_backend_str(*backend));
            set_opt_str(&obj, "codec", codec.map(audio_codec_kind_str));
            set_opt_str(&obj, "container", container.map(container_kind_str));
            set_f64(&obj, "sample_rate", f64::from(*sample_rate));
            set_f64(&obj, "channels", f64::from(*channels));
            set_opt_f64(&obj, "bit_depth", bit_depth.map(f64::from));
            set_opt_f64(&obj, "bitrate", bitrate.map(f64::from));
            set_f64(&obj, "epoch", num_traits::cast(*epoch).unwrap_or(0.0));
            set_str(&obj, "cause", decoder_change_cause_str(*cause));
            set_opt_f64(&obj, "variant", variant.map(f64::from));
            set_f64(
                &obj,
                "base_offset",
                num_traits::cast(*base_offset).unwrap_or(0.0),
            );
            set_opt_f64(&obj, "duration_seconds", *duration_seconds);
            set_f64(
                &obj,
                "gapless_leading",
                num_traits::cast(*gapless_leading).unwrap_or(0.0),
            );
            set_f64(
                &obj,
                "gapless_trailing",
                num_traits::cast(*gapless_trailing).unwrap_or(0.0),
            );
            set_bool(&obj, "has_gapless", *has_gapless);
        }
        FfiItemEvent::DecodeError {
            class,
            kind,
            codec,
            detail,
        } => {
            set_str(&obj, KIND, "DecodeError");
            set_str(&obj, "class", decode_error_class_str(*class));
            set_str(&obj, "decode_kind", decode_error_kind_str(*kind));
            set_opt_str(&obj, "codec", codec.map(audio_codec_kind_str));
            set_str(&obj, "detail", detail);
        }
        FfiItemEvent::GaplessResolved {
            leading_frames,
            trailing_frames,
            domain,
            codec,
            sample_rate,
        } => {
            set_str(&obj, KIND, "GaplessResolved");
            set_f64(
                &obj,
                "leading_frames",
                num_traits::cast(*leading_frames).unwrap_or(0.0),
            );
            set_f64(
                &obj,
                "trailing_frames",
                num_traits::cast(*trailing_frames).unwrap_or(0.0),
            );
            set_str(&obj, "domain", frame_domain_str(*domain));
            set_opt_str(&obj, "codec", codec.map(audio_codec_kind_str));
            set_f64(&obj, "sample_rate", f64::from(*sample_rate));
        }
        FfiItemEvent::ResamplerConfigured {
            backend,
            input_rate,
            output_rate,
            channels,
            bypassed,
        } => {
            set_str(&obj, KIND, "ResamplerConfigured");
            set_str(&obj, "backend", resampler_kind_str(*backend));
            set_f64(&obj, "input_rate", f64::from(*input_rate));
            set_f64(&obj, "output_rate", f64::from(*output_rate));
            set_f64(&obj, "channels", f64::from(*channels));
            set_bool(&obj, "bypassed", *bypassed);
        }
        FfiItemEvent::AudioFormatDetected {
            channels,
            sample_rate,
        } => {
            set_str(&obj, KIND, "AudioFormatDetected");
            set_f64(&obj, "channels", f64::from(*channels));
            set_f64(&obj, "sample_rate", f64::from(*sample_rate));
        }
        FfiItemEvent::AudioFormatChanged {
            old_channels,
            old_sample_rate,
            new_channels,
            new_sample_rate,
        } => {
            set_str(&obj, KIND, "AudioFormatChanged");
            set_f64(&obj, "old_channels", f64::from(*old_channels));
            set_f64(&obj, "old_sample_rate", f64::from(*old_sample_rate));
            set_f64(&obj, "new_channels", f64::from(*new_channels));
            set_f64(&obj, "new_sample_rate", f64::from(*new_sample_rate));
        }
        FfiItemEvent::SeekComplete {
            position_seconds,
            epoch,
        } => {
            set_str(&obj, KIND, "SeekComplete");
            set_f64(&obj, "position_seconds", *position_seconds);
            set_f64(&obj, "epoch", num_traits::cast(*epoch).unwrap_or(0.0));
        }
        FfiItemEvent::SeekRejected {
            epoch,
            target_seconds,
        } => {
            set_str(&obj, KIND, "SeekRejected");
            set_f64(&obj, "epoch", num_traits::cast(*epoch).unwrap_or(0.0));
            set_f64(&obj, "target_seconds", *target_seconds);
        }
        FfiItemEvent::DecoderReady {
            base_offset,
            variant,
        } => {
            set_str(&obj, KIND, "DecoderReady");
            set_f64(
                &obj,
                "base_offset",
                num_traits::cast(*base_offset).unwrap_or(0.0),
            );
            set_opt_f64(&obj, "variant", variant.map(f64::from));
        }
        FfiItemEvent::TrackFailed { reason, epoch } => {
            set_str(&obj, KIND, "TrackFailed");
            set_str(&obj, "reason", track_failure_kind_str(reason));
            if let FfiTrackFailureKind::RecreateFailed { offset } = reason {
                set_f64(&obj, "offset", num_traits::cast(*offset).unwrap_or(0.0));
            }
            set_f64(&obj, "epoch", num_traits::cast(*epoch).unwrap_or(0.0));
        }
        FfiItemEvent::UnderrunStarted { position_ms, epoch } => {
            set_str(&obj, KIND, "UnderrunStarted");
            set_f64(
                &obj,
                "position_ms",
                num_traits::cast(*position_ms).unwrap_or(0.0),
            );
            set_f64(&obj, "epoch", num_traits::cast(*epoch).unwrap_or(0.0));
        }
        FfiItemEvent::UnderrunEnded { position_ms, epoch } => {
            set_str(&obj, KIND, "UnderrunEnded");
            set_f64(
                &obj,
                "position_ms",
                num_traits::cast(*position_ms).unwrap_or(0.0),
            );
            set_f64(&obj, "epoch", num_traits::cast(*epoch).unwrap_or(0.0));
        }
        FfiItemEvent::BufferHealth {
            buffered_ms,
            decoded_frontier_ms,
            epoch,
        } => {
            set_str(&obj, KIND, "BufferHealth");
            set_f64(
                &obj,
                "buffered_ms",
                num_traits::cast(*buffered_ms).unwrap_or(0.0),
            );
            set_f64(
                &obj,
                "decoded_frontier_ms",
                num_traits::cast(*decoded_frontier_ms).unwrap_or(0.0),
            );
            set_f64(&obj, "epoch", num_traits::cast(*epoch).unwrap_or(0.0));
        }
        FfiItemEvent::EngineLoad {
            load,
            ms_per_chunk,
            realtime_factor,
        } => {
            set_str(&obj, KIND, "EngineLoad");
            set_f64(&obj, "load", f64::from(*load));
            set_f64(&obj, "ms_per_chunk", f64::from(*ms_per_chunk));
            set_f64(&obj, "realtime_factor", f64::from(*realtime_factor));
        }
        FfiItemEvent::PlaybackResamplerConfigured {
            backend,
            host_sample_rate,
            source_sample_rate,
            active,
        } => {
            set_str(&obj, KIND, "PlaybackResamplerConfigured");
            set_str(&obj, "backend", playback_resampler_kind_str(*backend));
            set_f64(&obj, "host_sample_rate", f64::from(*host_sample_rate));
            set_f64(&obj, "source_sample_rate", f64::from(*source_sample_rate));
            set_bool(&obj, "active", *active);
        }
        FfiItemEvent::HlsVariantSwitchFenced {
            from_variant,
            to_variant,
            cross_codec,
        } => {
            set_str(&obj, KIND, "HlsVariantSwitchFenced");
            set_f64(&obj, "from_variant", f64::from(*from_variant));
            set_f64(&obj, "to_variant", f64::from(*to_variant));
            set_bool(&obj, "cross_codec", *cross_codec);
        }
        FfiItemEvent::HlsVariantSwitchAcked {
            variant,
            generation,
        } => {
            set_str(&obj, KIND, "HlsVariantSwitchAcked");
            set_f64(&obj, "variant", f64::from(*variant));
            set_f64(
                &obj,
                "generation",
                num_traits::cast(*generation).unwrap_or(0.0),
            );
        }
        FfiItemEvent::HlsCacheComplete { total_bytes } => {
            set_str(&obj, KIND, "HlsCacheComplete");
            set_opt_f64(
                &obj,
                "total_bytes",
                total_bytes.and_then(|value| num_traits::cast(value)),
            );
        }
        FfiItemEvent::DownloadStarted {
            request_id,
            wait_in_queue_seconds,
        } => {
            set_str(&obj, KIND, "DownloadStarted");
            set_f64(
                &obj,
                "request_id",
                num_traits::cast(*request_id).unwrap_or(0.0),
            );
            set_f64(&obj, "wait_in_queue_seconds", *wait_in_queue_seconds);
        }
        FfiItemEvent::DownloadSlow {
            request_id,
            elapsed_seconds,
        } => {
            set_str(&obj, KIND, "DownloadSlow");
            set_f64(
                &obj,
                "request_id",
                num_traits::cast(*request_id).unwrap_or(0.0),
            );
            set_f64(&obj, "elapsed_seconds", *elapsed_seconds);
        }
        FfiItemEvent::DownloadCompleted {
            request_id,
            bytes_transferred,
            duration_seconds,
            bandwidth_bps,
        } => {
            set_str(&obj, KIND, "DownloadCompleted");
            set_f64(
                &obj,
                "request_id",
                num_traits::cast(*request_id).unwrap_or(0.0),
            );
            set_f64(
                &obj,
                "bytes_transferred",
                num_traits::cast(*bytes_transferred).unwrap_or(0.0),
            );
            set_f64(&obj, "duration_seconds", *duration_seconds);
            set_f64(
                &obj,
                "bandwidth_bps",
                num_traits::cast(*bandwidth_bps).unwrap_or(0.0),
            );
        }
        FfiItemEvent::DownloadRetrying {
            request_id,
            attempt,
            max_retries,
            error,
            backoff_seconds,
        } => {
            set_str(&obj, KIND, "DownloadRetrying");
            set_f64(
                &obj,
                "request_id",
                num_traits::cast(*request_id).unwrap_or(0.0),
            );
            set_f64(&obj, "attempt", f64::from(*attempt));
            set_f64(&obj, "max_retries", f64::from(*max_retries));
            set_str(&obj, "error", error);
            set_f64(&obj, "backoff_seconds", *backoff_seconds);
        }
        FfiItemEvent::DownloadBodyStalled {
            request_id,
            consumed,
            expected,
            stall_seconds,
        } => {
            set_str(&obj, KIND, "DownloadBodyStalled");
            set_f64(
                &obj,
                "request_id",
                num_traits::cast(*request_id).unwrap_or(0.0),
            );
            set_f64(&obj, "consumed", num_traits::cast(*consumed).unwrap_or(0.0));
            set_opt_f64(
                &obj,
                "expected",
                expected.and_then(|value| num_traits::cast(value)),
            );
            set_f64(&obj, "stall_seconds", *stall_seconds);
        }
        FfiItemEvent::DownloadBodyResumed {
            request_id,
            resume_number,
            from_offset,
            honoured_range,
        } => {
            set_str(&obj, KIND, "DownloadBodyResumed");
            set_f64(
                &obj,
                "request_id",
                num_traits::cast(*request_id).unwrap_or(0.0),
            );
            set_f64(&obj, "resume_number", f64::from(*resume_number));
            set_f64(
                &obj,
                "from_offset",
                num_traits::cast(*from_offset).unwrap_or(0.0),
            );
            set_bool(&obj, "honoured_range", *honoured_range);
        }
        FfiItemEvent::DownloadRetryExhausted {
            request_id,
            max_retries,
            consumed,
            error,
        } => {
            set_str(&obj, KIND, "DownloadRetryExhausted");
            set_f64(
                &obj,
                "request_id",
                num_traits::cast(*request_id).unwrap_or(0.0),
            );
            set_f64(&obj, "max_retries", f64::from(*max_retries));
            set_f64(&obj, "consumed", num_traits::cast(*consumed).unwrap_or(0.0));
            set_str(&obj, "error", error);
        }
        FfiItemEvent::DownloadFirstByte {
            request_id,
            ttfb_seconds,
            status,
            partial,
        } => {
            set_str(&obj, KIND, "DownloadFirstByte");
            set_f64(
                &obj,
                "request_id",
                num_traits::cast(*request_id).unwrap_or(0.0),
            );
            set_f64(&obj, "ttfb_seconds", *ttfb_seconds);
            set_f64(&obj, "status", f64::from(*status));
            set_bool(&obj, "partial", *partial);
        }
        FfiItemEvent::DownloadCancelled {
            request_id,
            reason,
            bytes_transferred,
        } => {
            set_str(&obj, KIND, "DownloadCancelled");
            set_f64(
                &obj,
                "request_id",
                num_traits::cast(*request_id).unwrap_or(0.0),
            );
            set_str(&obj, "reason", cancel_reason_str(*reason));
            set_f64(
                &obj,
                "bytes_transferred",
                num_traits::cast(*bytes_transferred).unwrap_or(0.0),
            );
        }
        FfiItemEvent::FileOpened {
            codec,
            container,
            total_bytes,
            cached,
        } => {
            set_str(&obj, KIND, "FileOpened");
            set_opt_str(&obj, "codec", codec.map(audio_codec_kind_str));
            set_opt_str(&obj, "container", container.map(container_kind_str));
            set_opt_f64(
                &obj,
                "total_bytes",
                total_bytes.and_then(|value| num_traits::cast(value)),
            );
            set_bool(&obj, "cached", *cached);
        }
        FfiItemEvent::FileTotalBytesResolved {
            total_bytes,
            source,
        } => {
            set_str(&obj, KIND, "FileTotalBytesResolved");
            set_f64(
                &obj,
                "total_bytes",
                num_traits::cast(*total_bytes).unwrap_or(0.0),
            );
            set_str(&obj, "source", total_bytes_source_str(*source));
        }
        FfiItemEvent::FileCacheComplete { total_bytes } => {
            set_str(&obj, KIND, "FileCacheComplete");
            set_f64(
                &obj,
                "total_bytes",
                num_traits::cast(*total_bytes).unwrap_or(0.0),
            );
        }
        FfiItemEvent::DrmKeyFetchFailed {
            key_host,
            stage,
            detail,
        } => {
            set_str(&obj, KIND, "DrmKeyFetchFailed");
            set_opt_str(&obj, "key_host", key_host.as_deref());
            set_str(&obj, "stage", key_failure_stage_str(*stage));
            set_str(&obj, "detail", detail);
        }
        FfiItemEvent::DrmKeyAcquired {
            key_host,
            source,
            bytes,
            latency_ms,
        } => {
            set_str(&obj, KIND, "DrmKeyAcquired");
            set_opt_str(&obj, "key_host", key_host.as_deref());
            set_str(&obj, "source", key_source_str(*source));
            set_f64(&obj, "bytes", num_traits::cast(*bytes).unwrap_or(0.0));
            set_opt_f64(
                &obj,
                "latency_ms",
                latency_ms.and_then(|value| num_traits::cast(value)),
            );
        }
        FfiItemEvent::DrmSegmentDecryptFailed {
            variant,
            segment_index,
            detail,
        } => {
            set_str(&obj, KIND, "DrmSegmentDecryptFailed");
            set_f64(&obj, "variant", f64::from(*variant));
            set_f64(&obj, "segment_index", f64::from(*segment_index));
            set_str(&obj, "detail", detail);
        }
    }
    obj.into()
}

fn variant_to_js(variant: &FfiVariant) -> JsValue {
    let obj = Object::new();
    set_f64(&obj, "index", f64::from(variant.index));
    set_f64(
        &obj,
        "bandwidth_bps",
        num_traits::cast(variant.bandwidth_bps).unwrap_or(0.0),
    );
    set_opt_str(&obj, "name", variant.name.as_deref());
    obj.into()
}

fn range_to_js(range: &FfiTimeRange) -> JsValue {
    let obj = Object::new();
    set_f64(&obj, "start_seconds", range.start_seconds);
    set_f64(&obj, "duration_seconds", range.duration_seconds);
    obj.into()
}

fn item_status_code(status: FfiItemStatus) -> f64 {
    match status {
        FfiItemStatus::Unknown => 0.0,
        FfiItemStatus::ReadyToPlay => 1.0,
        FfiItemStatus::Failed => 2.0,
    }
}

fn audio_codec_kind_str(kind: FfiAudioCodecKind) -> &'static str {
    match kind {
        FfiAudioCodecKind::AacLc => "AacLc",
        FfiAudioCodecKind::AacHe => "AacHe",
        FfiAudioCodecKind::AacHeV2 => "AacHeV2",
        FfiAudioCodecKind::Mp3 => "Mp3",
        FfiAudioCodecKind::Flac => "Flac",
        FfiAudioCodecKind::Vorbis => "Vorbis",
        FfiAudioCodecKind::Opus => "Opus",
        FfiAudioCodecKind::Alac => "Alac",
        FfiAudioCodecKind::Pcm => "Pcm",
        FfiAudioCodecKind::Adpcm => "Adpcm",
        FfiAudioCodecKind::Unknown => "Unknown",
    }
}

fn container_kind_str(kind: FfiContainerKind) -> &'static str {
    match kind {
        FfiContainerKind::Mp4 => "Mp4",
        FfiContainerKind::Fmp4 => "Fmp4",
        FfiContainerKind::MpegTs => "MpegTs",
        FfiContainerKind::MpegAudio => "MpegAudio",
        FfiContainerKind::Adts => "Adts",
        FfiContainerKind::Flac => "Flac",
        FfiContainerKind::Wav => "Wav",
        FfiContainerKind::Ogg => "Ogg",
        FfiContainerKind::Caf => "Caf",
        FfiContainerKind::Mkv => "Mkv",
        FfiContainerKind::Unknown => "Unknown",
    }
}

fn decoder_backend_str(kind: FfiDecoderBackend) -> &'static str {
    match kind {
        FfiDecoderBackend::Symphonia => "Symphonia",
        FfiDecoderBackend::Apple => "Apple",
        FfiDecoderBackend::Android => "Android",
        FfiDecoderBackend::Unknown => "Unknown",
    }
}

fn decoder_change_cause_str(cause: FfiDecoderChangeCause) -> &'static str {
    match cause {
        FfiDecoderChangeCause::Initial => "Initial",
        FfiDecoderChangeCause::VariantSwitch => "VariantSwitch",
        FfiDecoderChangeCause::FormatBoundary => "FormatBoundary",
        FfiDecoderChangeCause::SeekRecreate => "SeekRecreate",
        FfiDecoderChangeCause::Recovery => "Recovery",
        FfiDecoderChangeCause::HostRateChange => "HostRateChange",
        FfiDecoderChangeCause::Unknown => "Unknown",
    }
}

fn decode_error_class_str(class: FfiDecodeErrorClass) -> &'static str {
    match class {
        FfiDecodeErrorClass::Interrupted => "Interrupted",
        FfiDecodeErrorClass::VariantChange => "VariantChange",
        FfiDecodeErrorClass::Other => "Other",
        FfiDecodeErrorClass::Unknown => "Unknown",
    }
}

fn decode_error_kind_str(kind: FfiDecodeErrorKind) -> &'static str {
    match kind {
        FfiDecodeErrorKind::Io => "Io",
        FfiDecodeErrorKind::UnsupportedCodec => "UnsupportedCodec",
        FfiDecodeErrorKind::UnsupportedContainer => "UnsupportedContainer",
        FfiDecodeErrorKind::InvalidData => "InvalidData",
        FfiDecodeErrorKind::SeekFailed => "SeekFailed",
        FfiDecodeErrorKind::SeekOutOfRange => "SeekOutOfRange",
        FfiDecodeErrorKind::Parse => "Parse",
        FfiDecodeErrorKind::ProbeFailed => "ProbeFailed",
        FfiDecodeErrorKind::BackendUnavailable => "BackendUnavailable",
        FfiDecodeErrorKind::InvalidSampleRate => "InvalidSampleRate",
        FfiDecodeErrorKind::BackendStatus => "BackendStatus",
        FfiDecodeErrorKind::Interrupted => "Interrupted",
        FfiDecodeErrorKind::Backend => "Backend",
        FfiDecodeErrorKind::Unknown => "Unknown",
    }
}

fn frame_domain_str(domain: FfiFrameDomain) -> &'static str {
    match domain {
        FfiFrameDomain::Source => "Source",
        FfiFrameDomain::Output => "Output",
        FfiFrameDomain::Unknown => "Unknown",
    }
}

fn resampler_kind_str(kind: FfiResamplerKind) -> &'static str {
    match kind {
        FfiResamplerKind::Rubato => "Rubato",
        FfiResamplerKind::Apple => "Apple",
        FfiResamplerKind::Glide => "Glide",
        FfiResamplerKind::None => "None",
        FfiResamplerKind::Unknown => "Unknown",
    }
}

fn track_failure_kind_str(kind: &FfiTrackFailureKind) -> &'static str {
    match kind {
        FfiTrackFailureKind::Decode => "Decode",
        FfiTrackFailureKind::RecreateFailed { .. } => "RecreateFailed",
        FfiTrackFailureKind::SourceCancelled => "SourceCancelled",
        FfiTrackFailureKind::Unknown => "Unknown",
    }
}

fn playback_resampler_kind_str(kind: FfiPlaybackResamplerKind) -> &'static str {
    match kind {
        FfiPlaybackResamplerKind::Rubato => "Rubato",
        FfiPlaybackResamplerKind::Glide => "Glide",
        FfiPlaybackResamplerKind::None => "None",
        FfiPlaybackResamplerKind::Unknown => "Unknown",
    }
}

fn cancel_reason_str(reason: FfiCancelReason) -> &'static str {
    match reason {
        FfiCancelReason::EpochCancel => "EpochCancel",
        FfiCancelReason::PeerCancel => "PeerCancel",
        FfiCancelReason::DownloaderShutdown => "DownloaderShutdown",
        FfiCancelReason::BeforeStart => "BeforeStart",
    }
}

fn total_bytes_source_str(source: FfiTotalBytesSource) -> &'static str {
    match source {
        FfiTotalBytesSource::CommittedLen => "CommittedLen",
        FfiTotalBytesSource::ContentLength => "ContentLength",
        FfiTotalBytesSource::Unknown => "Unknown",
    }
}

fn key_failure_stage_str(stage: FfiKeyFailureStage) -> &'static str {
    match stage {
        FfiKeyFailureStage::Network => "Network",
        FfiKeyFailureStage::BodyCollect => "BodyCollect",
        FfiKeyFailureStage::Processor => "Processor",
        FfiKeyFailureStage::Missing => "Missing",
        FfiKeyFailureStage::Unknown => "Unknown",
    }
}

fn key_source_str(source: FfiKeySource) -> &'static str {
    match source {
        FfiKeySource::Network => "Network",
        FfiKeySource::DiskCache => "DiskCache",
        FfiKeySource::MemCache => "MemCache",
        FfiKeySource::Unknown => "Unknown",
    }
}
