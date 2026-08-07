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
use crate::{EncodeError, EncodeResult, types::EncodedAccessUnit};

/// Sample format the encoder takes from callers.
const INPUT_FORMAT: Sample = Sample::F32(SampleType::Packed);

/// Streaming AAC-LC encoder: interleaved f32 in, encoded access units out.
///
/// One instance encodes one continuous stream; [`push`](Self::push) returns the
/// access units the pushed audio completed, [`finish`](Self::finish) drains the
/// encoder's remaining frames.
pub struct StreamEncoder {
    encoder: AudioEncoder,
    filter: FilterGraph,
    channels: u16,
    sample_rate: u32,
    rates: RebaseRates,
    timestamp_origin: Option<i64>,
    next_pts: i64,
}

impl StreamEncoder {
    /// Samples per channel in one AAC-LC access unit.
    pub const FRAME_SAMPLES: usize = 1024;

    /// Open an AAC-LC encoder for `sample_rate`/`channels` audio, emitting
    /// access-unit timestamps in `timescale` units.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::InvalidInput`] for a zero sample rate, channel
    /// count, or timescale, and [`EncodeError::UnsupportedCodec`] when the
    /// linked `FFmpeg` has no AAC encoder.
    pub fn new(
        sample_rate: u32,
        channels: u16,
        bit_rate: u64,
        timescale: u32,
    ) -> EncodeResult<Self> {
        let rate = positive_i32(sample_rate, "sample_rate")?;
        let target_rate = positive_i32(timescale, "timescale")?;
        if channels == 0 {
            return Err(EncodeError::InvalidInput(
                "channels must be > 0, got 0".to_owned(),
            ));
        }

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

    /// Encode interleaved `samples` and return the access units they completed.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::InvalidInput`] when the slice length is not a
    /// multiple of the channel count, and [`EncodeError::Backend`] when
    /// `FFmpeg` rejects the frame.
    pub fn push(&mut self, samples: &[f32]) -> EncodeResult<Vec<EncodedAccessUnit>> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }

        let channels = usize::from(self.channels);
        if !samples.len().is_multiple_of(channels) {
            return Err(EncodeError::InvalidInput(format!(
                "interleaved sample count {} is not a multiple of {channels} channels",
                samples.len()
            )));
        }
        let frames = samples.len() / channels;
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

    /// Flush the encoder and return its remaining access units.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Backend`] when `FFmpeg` fails to drain.
    pub fn finish(mut self) -> EncodeResult<Vec<EncodedAccessUnit>> {
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

/// Rescale an encoder-domain timestamp into the target time base. Access-unit
/// boundaries are rescaled, so a duration is always the gap between two
/// rescaled positions and the timeline stays consistent for any ratio.
fn rescale_timestamp(value: i64, rates: RebaseRates) -> u64 {
    let rescaled = value.rescale(rates.encoder, rates.target).max(0);
    u64::try_from(rescaled).unwrap_or_else(|_| {
        tracing::error!(rescaled, "BUG: rescaled timestamp exceeds u64::MAX");
        0
    })
}

#[cfg(test)]
mod tests {
    use super::StreamEncoder;
    use crate::{EncodeError, EncodedAccessUnit, ffmpeg::test_pcm::TestPcm};

    struct Consts;

    impl Consts {
        const BIT_RATE: u64 = 128_000;
        const CHANNELS: u16 = 2;
        const FRAMES: usize = 4_096;
        const SAMPLE_RATE: u32 = 48_000;
    }

    fn encode_in_chunks(
        samples: &[f32],
        chunk_frames: usize,
        timescale: u32,
    ) -> Vec<EncodedAccessUnit> {
        let mut encoder = StreamEncoder::new(
            Consts::SAMPLE_RATE,
            Consts::CHANNELS,
            Consts::BIT_RATE,
            timescale,
        )
        .expect("stream encoder");
        let mut units = Vec::new();
        for chunk in samples.chunks(chunk_frames * usize::from(Consts::CHANNELS)) {
            units.extend(encoder.push(chunk).expect("push"));
        }
        units.extend(encoder.finish().expect("finish"));
        units
    }

    #[test]
    fn chunking_does_not_change_the_encoded_stream() {
        let samples =
            TestPcm::sawtooth(Consts::FRAMES, Consts::SAMPLE_RATE, Consts::CHANNELS).samples_f32();

        let whole = encode_in_chunks(&samples, Consts::FRAMES, Consts::SAMPLE_RATE);
        let framed = encode_in_chunks(&samples, StreamEncoder::FRAME_SAMPLES, Consts::SAMPLE_RATE);
        let ragged = encode_in_chunks(&samples, 333, Consts::SAMPLE_RATE);

        assert!(!whole.is_empty(), "encoder produced no access units");
        assert_eq!(whole, framed);
        assert_eq!(whole, ragged);
    }

    #[test]
    fn timestamps_start_at_zero_and_advance_by_one_frame() {
        let frame_samples =
            u64::try_from(StreamEncoder::FRAME_SAMPLES).expect("frame size fits u64");
        let pushed_frames = u64::try_from(Consts::FRAMES).expect("frame count fits u64");
        let samples =
            TestPcm::sawtooth(Consts::FRAMES, Consts::SAMPLE_RATE, Consts::CHANNELS).samples_f32();

        for timescale in [Consts::SAMPLE_RATE, 90_000] {
            let rescale =
                |frames: u64| frames * u64::from(timescale) / u64::from(Consts::SAMPLE_RATE);
            let units = encode_in_chunks(&samples, StreamEncoder::FRAME_SAMPLES, timescale);

            let mut expected_pts = 0;
            for unit in &units {
                assert!(!unit.bytes.is_empty(), "access unit payload is empty");
                assert!(unit.is_sync, "every AAC-LC access unit is a sync point");
                assert_eq!(unit.pts, expected_pts, "timescale {timescale}");
                assert_eq!(unit.dts, unit.pts, "AAC access units are not reordered");
                assert_eq!(
                    u64::from(unit.duration),
                    rescale(frame_samples),
                    "timescale {timescale}"
                );
                expected_pts += u64::from(unit.duration);
            }

            assert_eq!(
                expected_pts,
                rescale(pushed_frames) + rescale(frame_samples),
                "the flush adds exactly one priming frame"
            );
        }
    }

    #[test]
    fn a_fractional_timescale_ratio_keeps_durations_on_the_pts_timeline() {
        const SOURCE_RATE: u32 = 44_100;
        const TIMESCALE: u32 = 90_000;

        let samples =
            TestPcm::sawtooth(Consts::FRAMES, SOURCE_RATE, Consts::CHANNELS).samples_f32();
        let mut encoder =
            StreamEncoder::new(SOURCE_RATE, Consts::CHANNELS, Consts::BIT_RATE, TIMESCALE)
                .expect("stream encoder");
        let mut units = encoder.push(&samples).expect("push");
        units.extend(encoder.finish().expect("finish"));

        let mut expected_pts = 0;
        for unit in &units {
            assert_eq!(
                unit.pts, expected_pts,
                "durations must tile the pts timeline"
            );
            expected_pts += u64::from(unit.duration);
        }

        let frames =
            u64::try_from(Consts::FRAMES + StreamEncoder::FRAME_SAMPLES).expect("fits u64");
        let ticks = frames * u64::from(TIMESCALE);
        assert_eq!(
            expected_pts,
            (ticks + u64::from(SOURCE_RATE) / 2) / u64::from(SOURCE_RATE),
            "the stream ends on the rounded rescale of the encoded frames"
        );
    }

    #[test]
    fn new_rejects_a_channel_count_the_encoder_cannot_carry() {
        assert!(
            StreamEncoder::new(
                Consts::SAMPLE_RATE,
                9,
                Consts::BIT_RATE,
                Consts::SAMPLE_RATE
            )
            .is_err(),
            "AAC-LC carries no 9-channel layout"
        );
    }

    #[test]
    fn push_rejects_a_partial_frame() {
        let mut encoder = StreamEncoder::new(
            Consts::SAMPLE_RATE,
            Consts::CHANNELS,
            Consts::BIT_RATE,
            Consts::SAMPLE_RATE,
        )
        .expect("stream encoder");

        let error = encoder.push(&[0.0, 0.0, 0.0]).expect_err("partial frame");

        assert!(matches!(error, EncodeError::InvalidInput(_)), "{error}");
    }
}
