use ffmpeg::{
    ChannelLayout, Dictionary, Error as FfmpegError, Packet, Rational,
    codec::{
        Id, context::Context as CodecContext, encoder::Audio as AudioEncoder,
        flag::Flags as CodecFlags,
    },
    encoder::find as find_encoder,
};
use ffmpeg_next as ffmpeg;
use kithara_stream::AudioCodec;

use super::{
    build_direct_filter, ensure_ffmpeg_initialized,
    pcm::{
        drain_filtered_frames, flush_filter, pump_pcm_frames, send_eof_to_encoder,
        send_frame_to_filter,
    },
};
use crate::{
    EncodeError, EncodeResult,
    types::{EncodedAccessUnit, EncodedTrack, PackagedEncodeRequest},
};

/// AAC-LC encoder using `FFmpeg` (`libfdk-aac` or built-in AAC).
#[derive(Debug, Clone, Copy)]
pub(crate) struct AacFFmpegEncoder;

impl AacFFmpegEncoder {
    pub(crate) const AAC_FRAME_SAMPLES: usize = 1024;

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
        let mut encoder = PacketCollectingEncoder::new(
            request.pcm.sample_rate(),
            request.pcm.channels(),
            request.timescale,
            request.bit_rate,
        )?;

        pump_pcm_frames(request.pcm, Self::frame_samples(), |audio_frame| {
            send_frame_to_filter(&mut encoder.filter, audio_frame)?;
            encoder.receive_and_collect_filtered_frames()
        })?;

        flush_filter(&mut encoder.filter)?;
        encoder.receive_and_collect_filtered_frames()?;
        send_eof_to_encoder(&mut encoder.encoder)?;
        encoder.receive_and_collect_packets();

        let mut media_info = request.media_info.clone();
        media_info.codec = Some(AudioCodec::AacLc);
        media_info.sample_rate = Some(request.pcm.sample_rate());
        media_info.channels = Some(request.pcm.channels());

        Ok(EncodedTrack {
            media_info,
            timescale: request.timescale,
            bit_rate: request.bit_rate,
            codec_config: Vec::new(),
            packets_per_segment: request.packets_per_segment,
            encoder_delay: request.encoder_delay,
            trailing_delay: request.trailing_delay,
            access_units: encoder.into(),
        })
    }

    pub(crate) const fn frame_samples() -> usize {
        Self::AAC_FRAME_SAMPLES
    }
}

struct PacketCollectingEncoder {
    encoder: AudioEncoder,
    filter: ffmpeg::filter::Graph,
    timestamp_origin: Option<i64>,
    encoder_time_base: Rational,
    target_time_base: Rational,
    units: Vec<EncodedAccessUnit>,
}

impl From<PacketCollectingEncoder> for Vec<EncodedAccessUnit> {
    fn from(encoder: PacketCollectingEncoder) -> Self {
        encoder.units
    }
}

impl PacketCollectingEncoder {
    fn new(sample_rate: u32, channels: u16, timescale: u32, bit_rate: u64) -> EncodeResult<Self> {
        let output_codec = find_encoder(Id::AAC)
            .ok_or(EncodeError::UnsupportedCodec(AudioCodec::AacLc))?
            .audio()
            .map_err(|_| EncodeError::UnsupportedCodec(AudioCodec::AacLc))?;
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
        let bit_rate = usize::try_from(bit_rate).map_err(|_| {
            EncodeError::InvalidInput("bit_rate does not fit into usize".to_owned())
        })?;
        encoder.set_bit_rate(bit_rate);
        encoder.set_max_bit_rate(bit_rate);
        encoder.set_time_base((1, sample_rate as i32));

        let encoder = encoder.open_as_with(output_codec, Dictionary::new())?;
        let filter = build_direct_filter(&encoder, sample_rate, channels)?;

        Ok(Self {
            filter,
            encoder,
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
            duration: {
                let d = packet.duration().max(0);
                u32::try_from(d).unwrap_or_else(|_| {
                    tracing::error!(
                        packet_duration = d,
                        "BUG: AAC packet duration exceeds u32::MAX in target_time_base"
                    );
                    0
                })
            },
            is_sync: encoded.is_key(),
        });
    }
}

fn normalize_timestamp(value: i64, origin: i64) -> u64 {
    let normalized = i128::from(value) - i128::from(origin);
    let clamped = normalized.max(0);
    u64::try_from(clamped).unwrap_or_else(|_| {
        tracing::error!(normalized = ?clamped, "BUG: normalized timestamp exceeds u64::MAX");
        0
    })
}
