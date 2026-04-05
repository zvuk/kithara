use std::sync::OnceLock;

use ffmpeg::{
    ChannelLayout, Error as FfmpegError,
    codec::{capabilities::Capabilities, encoder::Audio as AudioEncoder},
    filter::{self, Graph as FilterGraph},
    format as av_format,
};
use ffmpeg_next as ffmpeg;
use kithara_stream::AudioCodec;

use super::{aac::AacFFmpegEncoder, flac::FlacFFmpegEncoder};
use crate::{
    BytesEncodeRequest, EncodeError, EncodeResult, EncodedBytes, EncodedTrack, InnerEncoder,
    PackagedEncodeRequest,
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
