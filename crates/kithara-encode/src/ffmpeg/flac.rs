use ffmpeg::{
    ChannelLayout, Dictionary, Error as FfmpegError, Packet, Rational,
    codec::{
        Id, context::Context as CodecContext, encoder::Audio as AudioEncoder,
        flag::Flags as CodecFlags,
    },
    encoder::find as find_encoder,
};
use ffmpeg_next as ffmpeg;
use kithara_stream::{AudioCodec, ContainerFormat};

use super::{
    build_direct_filter,
    bytes::encode_bytes_audio,
    ensure_ffmpeg_initialized,
    pcm::{
        drain_filtered_frames, flush_filter, pump_pcm_frames, send_eof_to_encoder,
        send_frame_to_filter,
    },
};
use crate::{
    BytesEncodeRequest, BytesEncodeTarget, EncodeError, EncodeResult,
    types::{EncodedAccessUnit, EncodedTrack, PackagedEncodeRequest},
};

/// FLAC encoder using `FFmpeg`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FlacFFmpegEncoder;

impl FlacFFmpegEncoder {
    pub(crate) const FLAC_FRAME_SAMPLES: usize = 4608;
    pub(crate) const FLAC_STREAMINFO_LEN: usize = 34;
    pub(crate) const FLAC_METADATA_HEADER_LEN: usize = 4;
    pub(crate) const FLAC_MIN_METADATA_BLOCK_LEN: usize =
        Self::FLAC_METADATA_HEADER_LEN + Self::FLAC_STREAMINFO_LEN;
    pub(crate) const FLAC_BLOCK_TYPE_MASK: u8 = 0x7F;
    pub(crate) const FLAC_LAST_BLOCK_FLAG: u8 = 0x80;

    pub(crate) const fn frame_samples() -> usize {
        Self::FLAC_FRAME_SAMPLES
    }

    pub(crate) fn encode(request: &PackagedEncodeRequest<'_>) -> EncodeResult<EncodedTrack> {
        if request.timescale == 0 {
            return Err(EncodeError::InvalidInput(
                "timescale must be > 0".to_owned(),
            ));
        }
        if request.packets_per_segment == 0 {
            return Err(EncodeError::InvalidInput(
                "packets_per_segment must be > 0".to_owned(),
            ));
        }
        if request.pcm.total_byte_len().is_none() {
            return Err(EncodeError::InvalidInput(
                "PCM source must have a finite length".to_owned(),
            ));
        }

        ensure_ffmpeg_initialized()?;
        let codec_config = extract_flac_codec_config(request.pcm)?;
        let mut encoder = PacketCollectingEncoder::new(
            request.pcm.sample_rate(),
            request.pcm.channels(),
            request.timescale,
            codec_config,
        )?;

        pump_pcm_frames(request.pcm, Self::frame_samples(), |audio_frame| {
            send_frame_to_filter(&mut encoder.filter, audio_frame)?;
            encoder.receive_and_collect_filtered_frames()
        })?;

        flush_filter(&mut encoder.filter)?;
        encoder.receive_and_collect_filtered_frames()?;
        send_eof_to_encoder(&mut encoder.encoder)?;
        encoder.receive_and_collect_packets();

        let media_info = request
            .media_info
            .clone()
            .with_codec(AudioCodec::Flac)
            .with_container(ContainerFormat::Fmp4)
            .with_sample_rate(request.pcm.sample_rate())
            .with_channels(request.pcm.channels());

        let (codec_config, access_units) = encoder.into_track_parts();

        Ok(EncodedTrack {
            media_info,
            timescale: request.timescale,
            bit_rate: request.bit_rate,
            codec_config,
            packets_per_segment: request.packets_per_segment,
            access_units,
        })
    }
}

struct PacketCollectingEncoder {
    filter: ffmpeg::filter::Graph,
    encoder: AudioEncoder,
    codec_config: Vec<u8>,
    encoder_time_base: Rational,
    target_time_base: Rational,
    timestamp_origin: Option<i64>,
    units: Vec<EncodedAccessUnit>,
}

impl PacketCollectingEncoder {
    fn new(
        sample_rate: u32,
        channels: u16,
        timescale: u32,
        codec_config: Vec<u8>,
    ) -> EncodeResult<Self> {
        let output_codec = find_encoder(Id::FLAC)
            .ok_or(EncodeError::UnsupportedCodec(AudioCodec::Flac))?
            .audio()
            .map_err(|_| EncodeError::UnsupportedCodec(AudioCodec::Flac))?;
        let context = CodecContext::new();
        let mut encoder = context.encoder().audio()?;

        let input_channel_layout = ChannelLayout::default(i32::from(channels));
        let channel_layout = output_codec
            .channel_layouts()
            .map_or(ChannelLayout::STEREO, |layouts| {
                layouts.best(input_channel_layout.channels())
            });

        encoder.set_flags(CodecFlags::GLOBAL_HEADER);
        encoder.set_rate(sample_rate as i32);
        encoder.set_channel_layout(channel_layout);
        encoder.set_format(
            output_codec
                .formats()
                .ok_or(FfmpegError::InvalidData)?
                .next()
                .ok_or(FfmpegError::InvalidData)?,
        );
        encoder.set_time_base((1, sample_rate as i32));

        let mut options = Dictionary::new();
        options.set("compression_level", "5");
        let encoder = encoder.open_as_with(output_codec, options)?;
        let filter = build_direct_filter(&encoder, sample_rate, channels)?;

        Ok(Self {
            filter,
            encoder,
            codec_config,
            encoder_time_base: Rational(1, sample_rate as i32),
            target_time_base: Rational(1, timescale as i32),
            timestamp_origin: None,
            units: Vec::new(),
        })
    }

    fn receive_and_collect_filtered_frames(&mut self) -> Result<(), FfmpegError> {
        let encoder_time_base = self.encoder_time_base;
        let target_time_base = self.target_time_base;
        let timestamp_origin = &mut self.timestamp_origin;
        let units = &mut self.units;
        drain_filtered_frames(&mut self.filter, &mut self.encoder, |encoder| {
            collect_encoded_packets(
                encoder,
                encoder_time_base,
                target_time_base,
                timestamp_origin,
                units,
            );
            Ok(())
        })
    }

    fn receive_and_collect_packets(&mut self) {
        collect_encoded_packets(
            &mut self.encoder,
            self.encoder_time_base,
            self.target_time_base,
            &mut self.timestamp_origin,
            &mut self.units,
        );
    }

    fn into_track_parts(self) -> (Vec<u8>, Vec<EncodedAccessUnit>) {
        (self.codec_config, self.units)
    }
}

fn extract_flac_codec_config(pcm: &dyn crate::PcmSource) -> EncodeResult<Vec<u8>> {
    let encoded = encode_bytes_audio(&BytesEncodeRequest {
        pcm,
        target: BytesEncodeTarget::Flac,
        bit_rate: None,
    })?;
    extract_stream_info_from_flac_bytes(&encoded.bytes)
}

fn normalize_flac_codec_config(raw: &[u8]) -> EncodeResult<Vec<u8>> {
    if raw.len() == FlacFFmpegEncoder::FLAC_STREAMINFO_LEN {
        return Ok(raw.to_vec());
    }
    if let Some(stream_info) = parse_flac_metadata_block(raw) {
        return Ok(stream_info.to_vec());
    }
    if let Some(stream_info) = raw
        .strip_prefix(b"fLaC")
        .and_then(parse_flac_metadata_block)
    {
        return Ok(stream_info.to_vec());
    }

    Err(EncodeError::InvalidInput(format!(
        "unsupported FLAC codec_config layout ({} bytes)",
        raw.len()
    )))
}

fn extract_stream_info_from_flac_bytes(raw: &[u8]) -> EncodeResult<Vec<u8>> {
    let mut remaining = raw.strip_prefix(b"fLaC").ok_or_else(|| {
        EncodeError::InvalidInput("FLAC bytes are missing the `fLaC` marker".to_owned())
    })?;

    while remaining.len() >= FlacFFmpegEncoder::FLAC_METADATA_HEADER_LEN {
        let block_len = parse_block_body_len(remaining);
        if remaining.len() < FlacFFmpegEncoder::FLAC_METADATA_HEADER_LEN + block_len {
            break;
        }
        if remaining[0] & FlacFFmpegEncoder::FLAC_BLOCK_TYPE_MASK == 0 {
            return normalize_flac_codec_config(
                &remaining[..FlacFFmpegEncoder::FLAC_METADATA_HEADER_LEN + block_len],
            );
        }
        let is_last = remaining[0] & FlacFFmpegEncoder::FLAC_LAST_BLOCK_FLAG != 0;
        remaining = &remaining[FlacFFmpegEncoder::FLAC_METADATA_HEADER_LEN + block_len..];
        if is_last {
            break;
        }
    }

    Err(EncodeError::InvalidInput(
        "FLAC bytes do not contain a valid STREAMINFO block".to_owned(),
    ))
}

fn parse_block_body_len(header: &[u8]) -> usize {
    const HI_SHIFT: usize = 16;
    const MID_SHIFT: usize = 8;
    const HI: usize = 1;
    const MID: usize = 2;
    const LO: usize = 3;
    (usize::from(header[HI]) << HI_SHIFT)
        | (usize::from(header[MID]) << MID_SHIFT)
        | usize::from(header[LO])
}

fn parse_flac_metadata_block(raw: &[u8]) -> Option<&[u8]> {
    if raw.len() < FlacFFmpegEncoder::FLAC_MIN_METADATA_BLOCK_LEN {
        return None;
    }
    if raw[0] & FlacFFmpegEncoder::FLAC_BLOCK_TYPE_MASK != 0 {
        return None;
    }
    let block_len = parse_block_body_len(raw);
    if block_len != FlacFFmpegEncoder::FLAC_STREAMINFO_LEN
        || raw.len() < FlacFFmpegEncoder::FLAC_METADATA_HEADER_LEN + block_len
    {
        return None;
    }
    Some(
        &raw[FlacFFmpegEncoder::FLAC_METADATA_HEADER_LEN
            ..FlacFFmpegEncoder::FLAC_METADATA_HEADER_LEN + block_len],
    )
}

fn collect_encoded_packets(
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

#[cfg(test)]
mod tests {
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
    use kithara_test_utils::kithara;

    use super::{FlacFFmpegEncoder, normalize_flac_codec_config};
    use crate::{EncoderFactory, PackagedEncodeRequest, test_pcm::SawtoothPcmFixture};

    struct Consts;
    impl Consts {
        const SAMPLE_RATE: u32 = 48_000;
        const CHANNELS: u16 = 2;
    }

    #[kithara::test]
    fn normalize_flac_codec_config_accepts_mp4_metadata_block() {
        let data = [
            0x80, 0x00, 0x00, 0x22, 0x12, 0x00, 0x12, 0x00, 0x00, 0x04, 0x2F, 0x00, 0x09, 0x41,
            0x0A, 0xC4, 0x42, 0xF0, 0x00, 0x00, 0xAC, 0x44, 0x09, 0x1A, 0x92, 0x07, 0x6E, 0xC3,
            0xBC, 0x84, 0x8E, 0x7F, 0x60, 0x75, 0x8D, 0x3A, 0x77, 0x61,
        ];
        let normalized = normalize_flac_codec_config(&data).expect("normalize dfLa payload");
        assert_eq!(normalized.len(), 34);
        assert_eq!(&normalized[..4], &[0x12, 0x00, 0x12, 0x00]);
    }

    #[kithara::test]
    fn encode_packaged_flac_happy_path_emits_monotonic_access_units() {
        let total_frames = 4 * FlacFFmpegEncoder::frame_samples();
        let pcm = SawtoothPcmFixture::new(total_frames, Consts::SAMPLE_RATE, Consts::CHANNELS);
        let media_info = MediaInfo::default()
            .with_codec(AudioCodec::Flac)
            .with_container(ContainerFormat::Fmp4);

        let encoded = EncoderFactory::encode_packaged(PackagedEncodeRequest {
            pcm: &pcm,
            media_info,
            timescale: Consts::SAMPLE_RATE,
            bit_rate: 512_000,
            packets_per_segment: 2,
        })
        .unwrap_or_else(|error| panic!("encode_packaged(Flac) failed: {error}"));

        assert_eq!(encoded.media_info.codec, Some(AudioCodec::Flac));
        assert_eq!(encoded.media_info.container, Some(ContainerFormat::Fmp4));
        assert_eq!(encoded.media_info.sample_rate, Some(Consts::SAMPLE_RATE));
        assert_eq!(encoded.media_info.channels, Some(Consts::CHANNELS));
        assert_eq!(encoded.timescale, Consts::SAMPLE_RATE);
        assert_eq!(encoded.packets_per_segment, 2);
        assert_eq!(encoded.codec_config.len(), 34);
        assert!(
            encoded.access_units.len() >= 2,
            "expected multiple FLAC access units, got {}",
            encoded.access_units.len()
        );

        let mut expected_pts = None;
        for unit in &encoded.access_units {
            assert!(!unit.bytes.is_empty(), "access unit payload is empty");
            assert_eq!(unit.pts, unit.dts, "FLAC should not reorder audio packets");
            assert!(unit.is_sync, "FLAC packets should be sync samples");
            assert!(unit.duration > 0, "FLAC packet duration must be positive");

            if let Some(expected_pts) = expected_pts {
                assert_eq!(
                    unit.pts, expected_pts,
                    "FLAC packet timestamps should be contiguous"
                );
            } else {
                assert_eq!(unit.pts, 0, "FLAC timeline should start at zero");
            }
            expected_pts = Some(unit.pts + u64::from(unit.duration));
        }
    }
}
