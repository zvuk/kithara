#[cfg(feature = "ffmpeg")]
use crate::ffmpeg::stream::FfmpegStream;
use crate::{
    error::{EncodeError, EncodeResult},
    types::EncodedAccessUnit,
};

#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(feature = "ffmpeg", feature = "fdk-aac"))
))]
compile_error!(
    "kithara-encode: enable an encode backend — `ffmpeg` for the offline byte, \
     FLAC and packaged paths, `fdk-aac` for HE-AAC and the streaming AAC-LC \
     the live broadcast runs on. With neither, this crate encodes nothing."
);

/// Backend a [`StreamEncoder`] runs on, chosen by the caller.
///
/// Variants are gated on cargo features: a variant exists in the type only when
/// its backend is compiled in, so asking for one this build does not carry is a
/// compile error. Failures of the chosen backend are terminal — there is no
/// fallback to the other one.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamBackend {
    /// `FFmpeg`'s AAC encoder, linked against the system `FFmpeg` (requires the
    /// `ffmpeg` feature).
    #[cfg(feature = "ffmpeg")]
    Ffmpeg,
}

/// Audio the caller hands one stream, validated once for every backend.
#[derive(Clone, Copy)]
pub(crate) struct StreamParams {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) bit_rate: u64,
    pub(crate) timescale: u32,
}

/// Streaming AAC-LC encoder: interleaved f32 in, encoded access units out.
///
/// One instance encodes one continuous stream; [`push`](Self::push) returns the
/// access units the pushed audio completed, [`finish`](Self::finish) drains the
/// encoder's remaining frames.
pub struct StreamEncoder {
    inner: Inner,
    channels: u16,
}

enum Inner {
    #[cfg(feature = "ffmpeg")]
    Ffmpeg(FfmpegStream),
}

impl StreamEncoder {
    /// Samples per channel in one AAC-LC access unit.
    pub const FRAME_SAMPLES: usize = 1024;

    /// Open `backend` on `sample_rate`/`channels` audio, emitting access-unit
    /// timestamps in `timescale` units.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::InvalidInput`] for a zero sample rate, channel
    /// count, or timescale, and a backend error when the chosen encoder cannot
    /// carry the requested audio.
    pub fn new(
        backend: StreamBackend,
        sample_rate: u32,
        channels: u16,
        bit_rate: u64,
        timescale: u32,
    ) -> EncodeResult<Self> {
        let params = StreamParams {
            sample_rate,
            channels,
            bit_rate,
            timescale,
        };
        params.validate()?;

        let inner = match backend {
            #[cfg(feature = "ffmpeg")]
            StreamBackend::Ffmpeg => Inner::Ffmpeg(FfmpegStream::new(&params)?),
        };

        Ok(Self { inner, channels })
    }

    /// Encode interleaved `samples` and return the access units they completed.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::InvalidInput`] when the slice length is not a
    /// multiple of the channel count, and [`EncodeError::Backend`] when the
    /// encoder rejects the audio.
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

        match &mut self.inner {
            #[cfg(feature = "ffmpeg")]
            Inner::Ffmpeg(stream) => stream.push(samples),
        }
    }

    /// Flush the encoder and return its remaining access units.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Backend`] when the encoder fails to drain.
    pub fn finish(self) -> EncodeResult<Vec<EncodedAccessUnit>> {
        match self.inner {
            #[cfg(feature = "ffmpeg")]
            Inner::Ffmpeg(stream) => stream.finish(),
        }
    }
}

impl StreamParams {
    fn validate(&self) -> EncodeResult<()> {
        if self.sample_rate == 0 {
            return Err(EncodeError::InvalidInput(
                "sample_rate must be > 0, got 0".to_owned(),
            ));
        }
        if self.channels == 0 {
            return Err(EncodeError::InvalidInput(
                "channels must be > 0, got 0".to_owned(),
            ));
        }
        if self.timescale == 0 {
            return Err(EncodeError::InvalidInput(
                "timescale must be > 0, got 0".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{StreamBackend, StreamEncoder};
    use crate::{EncodeError, EncodedAccessUnit, test_pcm::TestPcm};

    struct Consts;

    impl Consts {
        const BIT_RATE: u64 = 128_000;
        const CHANNELS: u16 = 2;
        const FRAMES: usize = 4_096;
        const SAMPLE_RATE: u32 = 48_000;
    }

    fn encode_in_chunks(
        backend: StreamBackend,
        samples: &[f32],
        chunk_frames: usize,
        timescale: u32,
    ) -> Vec<EncodedAccessUnit> {
        let mut encoder = StreamEncoder::new(
            backend,
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

    /// The same audio pushed in any chunking yields the same access units.
    fn chunking_does_not_change_the_encoded_stream(backend: StreamBackend) {
        let samples =
            TestPcm::sawtooth(Consts::FRAMES, Consts::SAMPLE_RATE, Consts::CHANNELS).samples_f32();

        let whole = encode_in_chunks(backend, &samples, Consts::FRAMES, Consts::SAMPLE_RATE);
        let framed = encode_in_chunks(
            backend,
            &samples,
            StreamEncoder::FRAME_SAMPLES,
            Consts::SAMPLE_RATE,
        );
        let ragged = encode_in_chunks(backend, &samples, 333, Consts::SAMPLE_RATE);

        assert!(!whole.is_empty(), "encoder produced no access units");
        assert_eq!(whole, framed);
        assert_eq!(whole, ragged);
    }

    /// Timestamps start at zero and every access unit carries one frame, on a
    /// timescale that divides the sample rate and on one that does not.
    fn timestamps_start_at_zero_and_advance_by_one_frame(
        backend: StreamBackend,
        priming_frames: usize,
    ) {
        let frame_samples =
            u64::try_from(StreamEncoder::FRAME_SAMPLES).expect("frame size fits u64");
        let encoded_frames =
            u64::try_from(Consts::FRAMES + priming_frames).expect("frame count fits u64");
        let samples =
            TestPcm::sawtooth(Consts::FRAMES, Consts::SAMPLE_RATE, Consts::CHANNELS).samples_f32();

        for timescale in [Consts::SAMPLE_RATE, 90_000] {
            let rescale =
                |frames: u64| frames * u64::from(timescale) / u64::from(Consts::SAMPLE_RATE);
            let units =
                encode_in_chunks(backend, &samples, StreamEncoder::FRAME_SAMPLES, timescale);

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
                rescale(encoded_frames),
                "the stream covers the pushed audio plus {priming_frames} frames of priming"
            );
        }
    }

    /// Access-unit boundaries are what gets rescaled, so durations tile the pts
    /// timeline even when the timescale ratio is fractional.
    fn a_fractional_timescale_ratio_keeps_durations_on_the_pts_timeline(
        backend: StreamBackend,
        priming_frames: usize,
    ) {
        const SOURCE_RATE: u32 = 44_100;
        const TIMESCALE: u32 = 90_000;

        let samples =
            TestPcm::sawtooth(Consts::FRAMES, SOURCE_RATE, Consts::CHANNELS).samples_f32();
        let mut encoder = StreamEncoder::new(
            backend,
            SOURCE_RATE,
            Consts::CHANNELS,
            Consts::BIT_RATE,
            TIMESCALE,
        )
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

        let frames = u64::try_from(Consts::FRAMES + priming_frames).expect("fits u64");
        let ticks = frames * u64::from(TIMESCALE);
        assert_eq!(
            expected_pts,
            (ticks + u64::from(SOURCE_RATE) / 2) / u64::from(SOURCE_RATE),
            "the stream ends on the rounded rescale of the encoded frames"
        );
    }

    fn new_rejects_a_channel_count_the_encoder_cannot_carry(backend: StreamBackend) {
        assert!(
            StreamEncoder::new(
                backend,
                Consts::SAMPLE_RATE,
                9,
                Consts::BIT_RATE,
                Consts::SAMPLE_RATE
            )
            .is_err(),
            "AAC-LC carries no 9-channel layout"
        );
    }

    fn push_rejects_a_partial_frame(backend: StreamBackend) {
        let mut encoder = StreamEncoder::new(
            backend,
            Consts::SAMPLE_RATE,
            Consts::CHANNELS,
            Consts::BIT_RATE,
            Consts::SAMPLE_RATE,
        )
        .expect("stream encoder");

        let error = encoder.push(&[0.0, 0.0, 0.0]).expect_err("partial frame");

        assert!(matches!(error, EncodeError::InvalidInput(_)), "{error}");
    }

    fn new_rejects_audio_no_backend_can_carry(backend: StreamBackend) {
        for (sample_rate, channels, timescale) in [
            (0, Consts::CHANNELS, Consts::SAMPLE_RATE),
            (Consts::SAMPLE_RATE, 0, Consts::SAMPLE_RATE),
            (Consts::SAMPLE_RATE, Consts::CHANNELS, 0),
        ] {
            let error =
                StreamEncoder::new(backend, sample_rate, channels, Consts::BIT_RATE, timescale)
                    .map(|_| ())
                    .expect_err("zero is not audio");

            assert!(matches!(error, EncodeError::InvalidInput(_)), "{error}");
        }
    }

    /// `FFmpeg`'s AAC encoder holds one frame of priming and flushes it.
    #[cfg(feature = "ffmpeg")]
    const FFMPEG_PRIMING_FRAMES: usize = StreamEncoder::FRAME_SAMPLES;

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn ffmpeg_chunking_does_not_change_the_encoded_stream() {
        chunking_does_not_change_the_encoded_stream(StreamBackend::Ffmpeg);
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn ffmpeg_timestamps_start_at_zero_and_advance_by_one_frame() {
        timestamps_start_at_zero_and_advance_by_one_frame(
            StreamBackend::Ffmpeg,
            FFMPEG_PRIMING_FRAMES,
        );
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn ffmpeg_fractional_timescale_ratio_keeps_durations_on_the_pts_timeline() {
        a_fractional_timescale_ratio_keeps_durations_on_the_pts_timeline(
            StreamBackend::Ffmpeg,
            FFMPEG_PRIMING_FRAMES,
        );
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn ffmpeg_new_rejects_a_channel_count_the_encoder_cannot_carry() {
        new_rejects_a_channel_count_the_encoder_cannot_carry(StreamBackend::Ffmpeg);
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn ffmpeg_push_rejects_a_partial_frame() {
        push_rejects_a_partial_frame(StreamBackend::Ffmpeg);
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn ffmpeg_new_rejects_audio_no_backend_can_carry() {
        new_rejects_audio_no_backend_can_carry(StreamBackend::Ffmpeg);
    }
}
