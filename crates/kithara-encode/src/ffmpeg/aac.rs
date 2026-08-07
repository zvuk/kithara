use kithara_stream::AudioCodec;

use super::{pcm::pump_pcm_samples, stream::StreamEncoder};
use crate::{
    EncodeResult,
    types::{EncodedAccessUnit, EncodedTrack, PackagedEncodeRequest},
};

/// Offline AAC-LC encoding: the whole PCM source pushed through
/// [`StreamEncoder`], then flushed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AacFFmpegEncoder;

impl AacFFmpegEncoder {
    pub(crate) fn encode(request: &PackagedEncodeRequest<'_>) -> EncodeResult<EncodedTrack> {
        request.validate()?;

        let pcm = request.pcm;
        let mut encoder = StreamEncoder::new(
            pcm.sample_rate(),
            pcm.channels(),
            request.bit_rate,
            request.timescale,
        )?;

        let mut access_units: Vec<EncodedAccessUnit> = Vec::new();
        pump_pcm_samples(pcm, Self::frame_samples(), |samples| {
            access_units.extend(encoder.push(samples)?);
            Ok(())
        })?;
        access_units.extend(encoder.finish()?);

        let mut media_info = request.media_info.clone();
        media_info.codec = Some(AudioCodec::AacLc);
        media_info.sample_rate = Some(pcm.sample_rate());
        media_info.channels = Some(pcm.channels());

        Ok(EncodedTrack {
            media_info,
            timescale: request.timescale,
            bit_rate: request.bit_rate,
            codec_config: Vec::new(),
            packets_per_segment: request.packets_per_segment,
            encoder_delay: request.encoder_delay,
            trailing_delay: request.trailing_delay,
            access_units,
        })
    }

    pub(crate) const fn frame_samples() -> usize {
        StreamEncoder::FRAME_SAMPLES
    }
}

#[cfg(test)]
mod tests {
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};

    use super::{AacFFmpegEncoder, PackagedEncodeRequest};
    use crate::{
        EncodedTrack,
        ffmpeg::{stream::StreamEncoder, test_pcm::TestPcm},
    };

    struct Consts;

    impl Consts {
        const BIT_RATE: u64 = 128_000;
        const CHANNELS: u16 = 2;
        const ENCODER_DELAY: u32 = 2_112;
        const FRAMES: usize = 4_096;
        const SAMPLE_RATE: u32 = 48_000;
        const TRAILING_DELAY: u32 = 1_920;
    }

    fn encode_offline(pcm: &TestPcm) -> EncodedTrack {
        AacFFmpegEncoder::encode(&PackagedEncodeRequest {
            pcm,
            media_info: MediaInfo::builder()
                .container(ContainerFormat::Fmp4)
                .build(),
            encoder_delay: Consts::ENCODER_DELAY,
            timescale: Consts::SAMPLE_RATE,
            trailing_delay: Consts::TRAILING_DELAY,
            bit_rate: Consts::BIT_RATE,
            packets_per_segment: 2,
        })
        .expect("offline AAC-LC encode")
    }

    #[test]
    fn the_offline_wrapper_keeps_every_streamed_access_unit() {
        let pcm = TestPcm::sawtooth(Consts::FRAMES, Consts::SAMPLE_RATE, Consts::CHANNELS);
        let offline = encode_offline(&pcm);

        let mut encoder = StreamEncoder::new(
            Consts::SAMPLE_RATE,
            Consts::CHANNELS,
            Consts::BIT_RATE,
            Consts::SAMPLE_RATE,
        )
        .expect("stream encoder");
        let mut streamed = encoder.push(&pcm.samples_f32()).expect("push");
        streamed.extend(encoder.finish().expect("finish"));

        assert_eq!(offline.access_units, streamed);
    }

    #[test]
    fn offline_track_holds_the_golden_shape() {
        let track = encode_offline(&TestPcm::sawtooth(
            Consts::FRAMES,
            Consts::SAMPLE_RATE,
            Consts::CHANNELS,
        ));
        let units = &track.access_units;

        assert_eq!(track.media_info.codec, Some(AudioCodec::AacLc));
        assert_eq!(track.media_info.sample_rate, Some(Consts::SAMPLE_RATE));
        assert_eq!(track.media_info.channels, Some(Consts::CHANNELS));
        assert_eq!(track.encoder_delay, Consts::ENCODER_DELAY);
        assert_eq!(track.trailing_delay, Consts::TRAILING_DELAY);
        assert!(track.codec_config.is_empty());

        assert_eq!(units.len(), 5);
        assert_eq!(units.first().map(|unit| unit.pts), Some(0));
        assert_eq!(units.last().map(|unit| unit.pts), Some(4_096));
        assert_eq!(
            units
                .iter()
                .map(|unit| u64::from(unit.duration))
                .sum::<u64>(),
            5_120
        );
    }
}
