use std::mem::size_of;

use ffmpeg::{
    ChannelLayout, Dictionary, Error as FfmpegError, Packet, Rational,
    codec::{
        Id, context::Context as CodecContext, encoder::Audio as AudioEncoder,
        flag::Flags as CodecFlags,
    },
    error::EAGAIN,
    filter::Graph as FilterGraph,
    format::{Sample, sample::Type as SampleType},
    frame::Audio as AudioFrame,
    rescale::Rescale,
};
use ffmpeg_next as ffmpeg;
use kithara_stream::AudioCodec;

use super::{
    RebaseRates, build_direct_filter, ensure_ffmpeg_initialized, find_encoder,
    pcm::{drain_filtered_frames, flush_filter, send_eof_to_encoder, send_frame_to_filter},
};
use crate::{
    EncodeError, EncodeResult,
    stream::{AacStream, StreamParams},
    types::EncodedAccessUnit,
};

const INPUT_FORMAT: Sample = Sample::F32(SampleType::Packed);

pub(crate) struct FfmpegStream {
    encoder: AudioEncoder,
    filter: FilterGraph,
    channels: u16,
    sample_rate: u32,
    rates: RebaseRates,
    timestamp_origin: Option<i64>,
    next_pts: i64,
}

impl FfmpegStream {
    pub(crate) fn new(params: &StreamParams) -> EncodeResult<Self> {
        let StreamParams {
            sample_rate,
            channels,
            bit_rate,
            timescale,
        } = *params;
        let rate = positive_i32(sample_rate, "sample_rate")?;
        let target_rate = positive_i32(timescale, "timescale")?;

        ensure_ffmpeg_initialized()?;

        let output_codec = find_encoder(Id::AAC)
            .ok_or(EncodeError::UnsupportedCodec(AudioCodec::AacLc))?
            .audio()
            .map_err(|_| EncodeError::UnsupportedCodec(AudioCodec::AacLc))?;
        let context = CodecContext::new();
        let mut encoder = context.encoder().audio()?;

        encoder.set_flags(CodecFlags::GLOBAL_HEADER);
        encoder.set_rate(rate);
        encoder.set_channel_layout(ChannelLayout::default(i32::from(channels)));
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
        encoder.set_time_base((1, rate));

        let encoder = encoder.open_as_with(output_codec, Dictionary::new())?;
        let filter = build_direct_filter(&encoder, sample_rate, channels, INPUT_FORMAT)?;

        Ok(Self {
            encoder,
            filter,
            channels,
            sample_rate,
            rates: RebaseRates {
                encoder: Rational(1, rate),
                target: Rational(1, target_rate),
            },
            timestamp_origin: None,
            next_pts: 0,
        })
    }

    fn encode(&mut self, samples: &[f32]) -> EncodeResult<Vec<EncodedAccessUnit>> {
        let frames = samples.len() / usize::from(self.channels);
        let frame_count = i32::try_from(frames).map_err(|_| {
            EncodeError::InvalidInput(format!("push of {frames} frames does not fit one frame"))
        })?;

        let mut frame = AudioFrame::new(
            INPUT_FORMAT,
            frames,
            ChannelLayout::default(i32::from(self.channels)),
        );
        frame.set_rate(self.sample_rate);
        frame.set_pts(Some(self.next_pts));
        for (target, sample) in frame
            .data_mut(0)
            .chunks_exact_mut(size_of::<f32>())
            .zip(samples)
        {
            target.copy_from_slice(&sample.to_ne_bytes());
        }

        send_frame_to_filter(&mut self.filter, &frame)?;
        self.next_pts += i64::from(frame_count);

        Ok(self.drain_filter()?)
    }

    fn drain(mut self) -> EncodeResult<Vec<EncodedAccessUnit>> {
        flush_filter(&mut self.filter)?;
        let mut units = self.drain_filter()?;
        send_eof_to_encoder(&mut self.encoder)?;
        collect_encoded_packets(
            &mut self.encoder,
            self.rates,
            &mut self.timestamp_origin,
            &mut units,
        )?;
        Ok(units)
    }

    fn drain_filter(&mut self) -> Result<Vec<EncodedAccessUnit>, FfmpegError> {
        let rates = self.rates;
        let timestamp_origin = &mut self.timestamp_origin;
        let mut units = Vec::new();
        drain_filtered_frames(&mut self.filter, &mut self.encoder, |encoder| {
            collect_encoded_packets(encoder, rates, timestamp_origin, &mut units)
        })?;
        Ok(units)
    }
}

impl AacStream for FfmpegStream {
    fn push(&mut self, samples: &[f32]) -> EncodeResult<Vec<EncodedAccessUnit>> {
        self.encode(samples)
    }

    fn finish(self: Box<Self>) -> EncodeResult<Vec<EncodedAccessUnit>> {
        self.drain()
    }
}

fn positive_i32(value: u32, field: &'static str) -> EncodeResult<i32> {
    i32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| EncodeError::InvalidInput(format!("{field} must be > 0, got {value}")))
}

fn collect_encoded_packets(
    encoder: &mut AudioEncoder,
    rates: RebaseRates,
    timestamp_origin: &mut Option<i64>,
    units: &mut Vec<EncodedAccessUnit>,
) -> Result<(), FfmpegError> {
    let mut encoded = Packet::empty();
    loop {
        match encoder.receive_packet(&mut encoded) {
            Ok(()) => {}
            Err(FfmpegError::Eof | FfmpegError::Other { errno: EAGAIN }) => return Ok(()),
            Err(error) => return Err(error),
        }
        if encoded.size() == 0 {
            continue;
        }

        let raw_pts = encoded.pts().unwrap_or_default();
        let raw_dts = encoded.dts().unwrap_or_default();
        let origin = *timestamp_origin.get_or_insert(raw_pts.min(raw_dts));
        let start = raw_pts.saturating_sub(origin).max(0);
        let end = start.saturating_add(encoded.duration().max(0));

        let pts = rescale_timestamp(start, rates);
        units.push(EncodedAccessUnit {
            bytes: encoded.data().unwrap_or(&[]).to_vec(),
            pts,
            dts: rescale_timestamp(raw_dts.saturating_sub(origin).max(0), rates),
            duration: {
                let duration = rescale_timestamp(end, rates).saturating_sub(pts);
                u32::try_from(duration).unwrap_or_else(|_| {
                    tracing::error!(
                        packet_duration = duration,
                        "BUG: AAC packet duration exceeds u32::MAX in the target time base"
                    );
                    0
                })
            },
            is_sync: encoded.is_key(),
        });
    }
}

fn rescale_timestamp(value: i64, rates: RebaseRates) -> u64 {
    let rescaled = value.rescale(rates.encoder, rates.target).max(0);
    u64::try_from(rescaled).unwrap_or_else(|_| {
        tracing::error!(rescaled, "BUG: rescaled timestamp exceeds u64::MAX");
        0
    })
}
