use std::sync::OnceLock;

use ffmpeg::{
    ChannelLayout, Error as FfmpegError, Packet, Rational,
    codec::{capabilities::Capabilities, encoder::Audio as AudioEncoder},
    filter::{self, Graph as FilterGraph},
    format as av_format,
};
use ffmpeg_next as ffmpeg;
use kithara_stream::AudioCodec;

use super::{aac::AacFFmpegEncoder, flac::FlacFFmpegEncoder};
use crate::{
    BytesEncodeRequest, EncodeError, EncodeResult, EncodedBytes, EncodedTrack, InnerEncoder,
    PackagedEncodeRequest, types::EncodedAccessUnit,
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct FfmpegEncoder;

impl InnerEncoder for FfmpegEncoder {
    fn encode_bytes(&self, request: BytesEncodeRequest<'_>) -> EncodeResult<EncodedBytes> {
        super::bytes::encode_bytes_audio(&request)
    }

    fn encode_packaged(&self, request: PackagedEncodeRequest<'_>) -> EncodeResult<EncodedTrack> {
        let codec = request
            .media_info
            .codec
            .ok_or(EncodeError::InvalidMediaInfo("codec"))?;
        match codec {
            AudioCodec::AacLc => AacFFmpegEncoder::encode(&request),
            AudioCodec::Flac => FlacFFmpegEncoder::encode(&request),
            codec => Err(EncodeError::UnsupportedCodec(codec)),
        }
    }

    fn packaged_frame_samples(&self, codec: AudioCodec) -> EncodeResult<usize> {
        match codec {
            AudioCodec::AacLc => Ok(AacFFmpegEncoder::frame_samples()),
            AudioCodec::Flac => Ok(FlacFFmpegEncoder::frame_samples()),
            codec => Err(EncodeError::UnsupportedCodec(codec)),
        }
    }
}

pub(crate) fn ensure_ffmpeg_initialized() -> Result<(), EncodeError> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();

    match INIT.get_or_init(|| ffmpeg::init().map_err(|error| error.to_string())) {
        Ok(()) => Ok(()),
        Err(message) => Err(EncodeError::backend_message(message.clone())),
    }
}

pub(crate) fn build_direct_filter(
    encoder: &AudioEncoder,
    sample_rate: u32,
    channels: u16,
) -> Result<FilterGraph, FfmpegError> {
    let mut graph = FilterGraph::new();
    let input_channel_layout = ChannelLayout::default(i32::from(channels));
    let args = format!(
        "time_base=1/{}:sample_rate={}:sample_fmt={}:channel_layout=0x{:x}",
        sample_rate,
        sample_rate,
        av_format::Sample::I16(av_format::sample::Type::Packed).name(),
        input_channel_layout.bits()
    );

    graph.add(
        &filter::find("abuffer").ok_or(FfmpegError::Bug)?,
        "in",
        &args,
    )?;
    graph.add(
        &filter::find("abuffersink").ok_or(FfmpegError::Bug)?,
        "out",
        "",
    )?;

    let aformat_args = format!(
        "aformat=sample_fmts={}:sample_rates={}:channel_layouts=0x{:x}",
        encoder.format().name(),
        encoder.rate(),
        encoder.channel_layout().bits()
    );
    graph
        .output("in", 0)?
        .input("out", 0)?
        .parse(&aformat_args)?;
    graph.validate()?;

    if let Some(codec) = encoder.codec()
        && !codec
            .capabilities()
            .contains(Capabilities::VARIABLE_FRAME_SIZE)
    {
        graph
            .get("out")
            .ok_or(FfmpegError::Bug)?
            .sink()
            .set_frame_size(encoder.frame_size());
    }

    Ok(graph)
}

pub(crate) fn collect_encoded_packets(
    encoder: &mut AudioEncoder,
    encoder_time_base: Rational,
    target_time_base: Rational,
    timestamp_origin: &mut Option<i64>,
    units: &mut Vec<EncodedAccessUnit>,
) {
    let mut encoded = Packet::empty();
    while encoder.receive_packet(&mut encoded).is_ok() {
        if encoded.size() == 0 {
            continue;
        }
        let mut packet = Packet::copy(encoded.data().unwrap_or(&[]));
        packet.set_pts(encoded.pts());
        packet.set_dts(encoded.dts());
        packet.set_duration(encoded.duration());
        packet.rescale_ts(encoder_time_base, target_time_base);
        let raw_pts = packet.pts().unwrap_or_default();
        let raw_dts = packet.dts().unwrap_or_default();
        let origin = *timestamp_origin.get_or_insert(raw_pts.min(raw_dts));
        units.push(EncodedAccessUnit {
            bytes: packet.data().unwrap_or(&[]).to_vec(),
            pts: normalize_timestamp(raw_pts, origin),
            dts: normalize_timestamp(raw_dts, origin),
            duration: u32::try_from(packet.duration().max(0)).unwrap_or(u32::MAX),
            is_sync: encoded.is_key(),
        });
    }
}

fn normalize_timestamp(value: i64, origin: i64) -> u64 {
    let normalized = i128::from(value) - i128::from(origin);
    u64::try_from(normalized.max(0)).unwrap_or(u64::MAX)
}
