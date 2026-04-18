use std::{fs, path::Path};

use ffmpeg::{
    ChannelLayout, Dictionary, Error as FfmpegError, Packet,
    codec::{
        context::Context as CodecContext, encoder::Audio as AudioEncoder, flag::Flags as CodecFlags,
    },
    encoder::find as find_encoder,
    format::{self as av_format, flag::Flags as FormatFlags},
    media::Type as MediaType,
};
use ffmpeg_next as ffmpeg;

use super::{
    build_direct_filter, ensure_ffmpeg_initialized,
    pcm::{
        drain_filtered_frames, flush_filter, pump_pcm_frames, send_eof_to_encoder,
        send_frame_to_filter,
    },
};
use crate::{
    BytesEncodeRequest, BytesEncodeTarget, EncodeError, EncodeResult, EncodedBytes, PcmSource,
};

struct EncodeTarget {
    ext: &'static str,
    mime: &'static str,
    bit_rate: Option<usize>,
    option_pairs: &'static [(&'static str, &'static str)],
}

impl EncodeTarget {
    fn from_request(request: &BytesEncodeRequest<'_>) -> EncodeResult<Self> {
        let bit_rate = request
            .bit_rate
            .or_else(|| request.target.default_bit_rate());
        let bit_rate = bit_rate
            .map(|value| {
                usize::try_from(value).map_err(|_| {
                    EncodeError::InvalidInput("bit_rate does not fit into usize".to_owned())
                })
            })
            .transpose()?;

        let target = match request.target {
            BytesEncodeTarget::Mp3 => Self {
                ext: "mp3",
                mime: "audio/mpeg",
                bit_rate,
                option_pairs: &[("b", "128k")],
            },
            BytesEncodeTarget::Flac => Self {
                ext: "flac",
                mime: "audio/flac",
                bit_rate: None,
                option_pairs: &[("compression_level", "5")],
            },
            BytesEncodeTarget::Aac => Self {
                ext: "aac",
                mime: "audio/aac",
                bit_rate,
                option_pairs: &[("b", "128k")],
            },
            BytesEncodeTarget::M4a => Self {
                ext: "m4a",
                mime: "audio/mp4",
                bit_rate,
                option_pairs: &[("b", "128k")],
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
        output_path,
        target,
        pcm.sample_rate(),
        pcm.channels(),
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
    filter: ffmpeg::filter::Graph,
    encoder: AudioEncoder,
}

impl DirectEncoder {
    fn new(
        octx: &mut av_format::context::Output,
        output_path: &Path,
        target: &EncodeTarget,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, EncodeError> {
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

        Ok(Self { filter, encoder })
    }

    fn receive_and_process_filtered_frames(
        &mut self,
        octx: &mut av_format::context::Output,
    ) -> Result<(), FfmpegError> {
        drain_filtered_frames(&mut self.filter, &mut self.encoder, |encoder| {
            write_encoded_packets(encoder, octx)
        })
    }

    fn receive_and_process_encoded_packets(
        &mut self,
        octx: &mut av_format::context::Output,
    ) -> Result<(), FfmpegError> {
        write_encoded_packets(&mut self.encoder, octx)
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use crate::{
        BytesEncodeRequest, BytesEncodeTarget, EncoderFactory, test_pcm::SawtoothPcmFixture,
    };

    #[kithara::test]
    fn encode_bytes_happy_paths_return_expected_metadata_and_container_markers() {
        const SAMPLE_RATE: u32 = 48_000;
        const CHANNELS: u16 = 2;
        const AAC_FRAME_SAMPLES: usize = 1024;

        let pcm = SawtoothPcmFixture::new(4 * AAC_FRAME_SAMPLES, SAMPLE_RATE, CHANNELS);
        let cases = [
            BytesEncodeTarget::Mp3,
            BytesEncodeTarget::Flac,
            BytesEncodeTarget::Aac,
            BytesEncodeTarget::M4a,
        ];

        for target in cases {
            let encoded = EncoderFactory::encode_bytes(BytesEncodeRequest {
                pcm: &pcm,
                target,
                bit_rate: None,
            })
            .unwrap_or_else(|error| panic!("encode_bytes({target:?}) failed: {error}"));

            assert!(!encoded.bytes.is_empty(), "{target:?} payload is empty");
            assert_eq!(encoded.content_type, target.content_type());
            assert_eq!(encoded.media_info.codec, Some(target.codec()));
            assert_eq!(encoded.media_info.container, Some(target.container()));
            assert_eq!(encoded.media_info.sample_rate, Some(SAMPLE_RATE));
            assert_eq!(encoded.media_info.channels, Some(CHANNELS));

            assert_container_marker(target, &encoded.bytes);
        }
    }

    fn assert_container_marker(target: BytesEncodeTarget, bytes: &[u8]) {
        match target {
            BytesEncodeTarget::Mp3 => assert!(
                bytes.starts_with(b"ID3")
                    || (bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0),
                "MP3 output is missing an ID3 tag or MPEG frame sync"
            ),
            BytesEncodeTarget::Flac => {
                assert!(
                    bytes.starts_with(b"fLaC"),
                    "FLAC output is missing the `fLaC` marker"
                );
            }
            BytesEncodeTarget::Aac => assert!(
                bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xF0) == 0xF0,
                "AAC output is missing an ADTS sync word"
            ),
            BytesEncodeTarget::M4a => assert!(
                bytes.windows(4).any(|window| window == b"ftyp")
                    && bytes.windows(4).any(|window| window == b"mdat"),
                "M4A output is missing MP4 container boxes"
            ),
        }
    }
}
