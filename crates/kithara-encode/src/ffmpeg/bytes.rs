use std::{fs, path::Path};

use ffmpeg::{
    ChannelLayout, Dictionary, Error as FfmpegError, Packet,
    codec::{
        context::Context as CodecContext, encoder::Audio as AudioEncoder, flag::Flags as CodecFlags,
    },
    format::{self as av_format, flag::Flags as FormatFlags},
    media::Type as MediaType,
};
use ffmpeg_next as ffmpeg;

use super::{
    build_direct_filter, ensure_ffmpeg_initialized, find_encoder,
    pcm::{
        drain_filtered_frames, flush_filter, pump_pcm_frames, send_eof_to_encoder,
        send_frame_to_filter,
    },
};
use crate::{
    BytesEncodeRequest, BytesEncodeTarget, EncodeError, EncodeResult, EncodedBytes, PcmSource,
};

struct EncodeTarget {
    option_pairs: &'static [(&'static str, &'static str)],
    ext: &'static str,
    mime: &'static str,
    bit_rate: Option<usize>,
}

impl EncodeTarget {
    fn from_request(request: &BytesEncodeRequest<'_>) -> EncodeResult<Self> {
        let explicit = request.bit_rate;
        let codec_default = request.target.default_bit_rate();
        let bit_rate = explicit.or(codec_default);
        let bit_rate = bit_rate
            .map(|value| {
                usize::try_from(value).map_err(|_| {
                    EncodeError::InvalidInput("bit_rate does not fit into usize".to_owned())
                })
            })
            .transpose()?;

        let target = match request.target {
            BytesEncodeTarget::Mp3 => Self {
                bit_rate,
                ext: "mp3",
                mime: "audio/mpeg",
                option_pairs: &[],
            },
            BytesEncodeTarget::Flac => Self {
                ext: "flac",
                mime: "audio/flac",
                bit_rate: None,
                option_pairs: &[("compression_level", "5")],
            },
            BytesEncodeTarget::Aac => Self {
                bit_rate,
                ext: "aac",
                mime: "audio/aac",
                option_pairs: &[],
            },
            BytesEncodeTarget::M4a => Self {
                bit_rate,
                ext: "m4a",
                mime: "audio/mp4",
                option_pairs: &[],
            },
        };

        Ok(target)
    }
}

pub(crate) fn encode_bytes_audio(request: &BytesEncodeRequest<'_>) -> EncodeResult<EncodedBytes> {
    if request.pcm.total_byte_len().is_none() {
        return Err(EncodeError::InvalidInput(
            "PCM source must have a finite length".to_owned(),
        ));
    }

    ensure_ffmpeg_initialized()?;

    let target = EncodeTarget::from_request(request)?;
    let temp_dir = tempfile::tempdir()?;
    let output_path = temp_dir.path().join(format!("signal.{}", target.ext));

    encode_direct_pcm(request.pcm, &output_path, &target)?;

    Ok(EncodedBytes {
        bytes: fs::read(&output_path)?,
        content_type: target.mime,
        media_info: request.media_info(),
    })
}

fn encode_direct_pcm(
    pcm: &dyn PcmSource,
    output_path: &Path,
    target: &EncodeTarget,
) -> Result<(), EncodeError> {
    const DIRECT_PCM_CHUNK_FRAMES: usize = 1024;
    let mut octx = av_format::output(output_path)?;
    let mut encoder = DirectEncoder::new(
        &mut octx,
        DirectEncodeConfig {
            output_path,
            target,
            sample_rate: pcm.sample_rate(),
            channels: pcm.channels(),
        },
    )?;

    octx.write_header()?;

    pump_pcm_frames(pcm, DIRECT_PCM_CHUNK_FRAMES, |audio_frame| {
        send_frame_to_filter(&mut encoder.filter, audio_frame)?;
        encoder.receive_and_process_filtered_frames(&mut octx)
    })?;

    flush_filter(&mut encoder.filter)?;
    encoder.receive_and_process_filtered_frames(&mut octx)?;

    send_eof_to_encoder(&mut encoder.encoder)?;
    encoder.receive_and_process_encoded_packets(&mut octx)?;

    octx.write_trailer()?;
    Ok(())
}

struct DirectEncoder {
    encoder: AudioEncoder,
    filter: ffmpeg::filter::Graph,
}

/// Codec/format config for [`DirectEncoder::new`], separate from the
/// mutable `octx`: the `output_path` (drives codec lookup), the
/// `target` format, and the source `sample_rate` / `channels`.
#[derive(Clone, Copy)]
struct DirectEncodeConfig<'a> {
    target: &'a EncodeTarget,
    output_path: &'a Path,
    channels: u16,
    sample_rate: u32,
}

impl DirectEncoder {
    fn new(
        octx: &mut av_format::context::Output,
        config: DirectEncodeConfig<'_>,
    ) -> Result<Self, EncodeError> {
        let DirectEncodeConfig {
            output_path,
            target,
            sample_rate,
            channels,
        } = config;
        let codec_id = octx.format().codec(output_path, MediaType::Audio);
        let output_codec = find_encoder(codec_id)
            .ok_or_else(|| {
                EncodeError::backend_message(format!(
                    "no output codec is registered for `{}`",
                    target.ext
                ))
            })?
            .audio()
            .map_err(|_| {
                EncodeError::backend_message(format!(
                    "no encoder is available for `{}`",
                    target.ext
                ))
            })?;
        let global_header = octx.format().flags().contains(FormatFlags::GLOBAL_HEADER);

        let mut output = octx.add_stream(output_codec)?;
        let context = CodecContext::from_parameters(output.parameters())?;
        let mut encoder = context.encoder().audio()?;

        let input_channel_layout = ChannelLayout::default(i32::from(channels));
        let channel_layout = output_codec
            .channel_layouts()
            .map_or(ChannelLayout::STEREO, |layouts| {
                layouts.best(input_channel_layout.channels())
            });

        if global_header {
            encoder.set_flags(CodecFlags::GLOBAL_HEADER);
        }

        encoder.set_rate(sample_rate as i32);
        encoder.set_channel_layout(channel_layout);
        encoder.set_format(
            output_codec
                .formats()
                .ok_or(FfmpegError::InvalidData)?
                .next()
                .ok_or(FfmpegError::InvalidData)?,
        );
        if let Some(bit_rate) = target.bit_rate {
            encoder.set_bit_rate(bit_rate);
            encoder.set_max_bit_rate(bit_rate);
        }
        encoder.set_time_base((1, sample_rate as i32));
        output.set_time_base((1, sample_rate as i32));

        let mut options = Dictionary::new();
        for (key, value) in target.option_pairs {
            options.set(key, value);
        }
        let encoder = encoder.open_as_with(output_codec, options)?;
        output.set_parameters(&encoder);

        let filter = build_direct_filter(&encoder, sample_rate, channels)?;

        Ok(Self { encoder, filter })
    }

    fn receive_and_process_encoded_packets(
        &mut self,
        octx: &mut av_format::context::Output,
    ) -> Result<(), FfmpegError> {
        write_encoded_packets(&mut self.encoder, octx)
    }

    fn receive_and_process_filtered_frames(
        &mut self,
        octx: &mut av_format::context::Output,
    ) -> Result<(), FfmpegError> {
        drain_filtered_frames(&mut self.filter, &mut self.encoder, |encoder| {
            write_encoded_packets(encoder, octx)
        })
    }
}

fn write_encoded_packets(
    encoder: &mut AudioEncoder,
    octx: &mut av_format::context::Output,
) -> Result<(), FfmpegError> {
    let mut encoded = Packet::empty();
    let stream_time_base = octx.stream(0).ok_or(FfmpegError::Bug)?.time_base();
    while encoder.receive_packet(&mut encoded).is_ok() {
        if encoded.size() > 0 {
            encoded.set_stream(0);
            encoded.rescale_ts(encoder.time_base(), stream_time_base);
            encoded.write_interleaved(octx)?;
        }
    }

    Ok(())
}
