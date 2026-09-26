use kithara_bufpool::{HasPool, PoolRegion};
use kithara_stream::AudioCodec;

use super::pcm::pump_pcm_samples;
use crate::{
    EncodeResult,
    stream::{StreamBackend, StreamEncoder},
    types::{EncodedAccessUnit, EncodedTrack, PackagedEncodeRequest},
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct AacFFmpegEncoder;

impl AacFFmpegEncoder {
    pub(crate) fn encode<S>(
        pools: &PoolRegion<S>,
        request: &PackagedEncodeRequest<'_>,
    ) -> EncodeResult<EncodedTrack>
    where
        S: HasPool<u8> + HasPool<f32>,
    {
        request.validate()?;

        let pcm = request.pcm;
        let mut encoder = StreamEncoder::builder()
            .backend(StreamBackend::Ffmpeg)
            .sample_rate(pcm.sample_rate())
            .channels(pcm.channels())
            .bit_rate(request.bit_rate)
            .timescale(request.timescale)
            .build()?;

        let mut access_units: Vec<EncodedAccessUnit> = Vec::new();
        pump_pcm_samples(pcm, pools, Self::frame_samples(), |samples| {
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
            access_units,
            timescale: request.timescale,
            bit_rate: request.bit_rate,
            codec_config: Vec::new(),
            packets_per_segment: request.packets_per_segment,
            encoder_delay: request.encoder_delay,
            trailing_delay: request.trailing_delay,
        })
    }

    pub(crate) const fn frame_samples() -> usize {
        StreamEncoder::FRAME_SAMPLES
    }
}

#[cfg(test)]
mod tests {
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
    use kithara_test_fixtures::unit_fixtures::encode_saw_i16;
    use kithara_test_utils::kithara;

    use super::{AacFFmpegEncoder, PackagedEncodeRequest};
    use crate::{
        EncodedTrack, consts,
        stream::{StreamBackend, StreamEncoder},
        test_pcm::TestPcm,
        test_pools,
    };

    fn encode_offline(pcm: &TestPcm) -> EncodedTrack {
        let pools = test_pools::pools();
        AacFFmpegEncoder::encode(
            &pools,
            &PackagedEncodeRequest::builder()
                .pcm(pcm)
                .media_info(
                    MediaInfo::builder()
                        .container(ContainerFormat::Fmp4)
                        .build(),
                )
                .encoder_delay(consts::ENCODER_DELAY)
                .timescale(consts::AAC_SAMPLE_RATE)
                .trailing_delay(consts::TRAILING_DELAY)
                .bit_rate(consts::AAC_BIT_RATE)
                .packets_per_segment(2)
                .build(),
        )
        .expect("offline AAC-LC encode")
    }

    #[kithara::test(native, flash(false))]
    fn the_offline_wrapper_keeps_every_streamed_access_unit(encode_saw_i16: &'static [u8]) {
        let pcm = TestPcm::from_bytes(
            encode_saw_i16.to_vec(),
            consts::AAC_SAMPLE_RATE,
            consts::AAC_CHANNELS,
        );
        let offline = encode_offline(&pcm);

        let mut encoder = StreamEncoder::builder()
            .backend(StreamBackend::Ffmpeg)
            .sample_rate(consts::AAC_SAMPLE_RATE)
            .channels(consts::AAC_CHANNELS)
            .bit_rate(consts::AAC_BIT_RATE)
            .timescale(consts::AAC_SAMPLE_RATE)
            .build()
            .expect("stream encoder");
        let mut streamed = encoder.push(&pcm.samples_f32()).expect("push");
        streamed.extend(encoder.finish().expect("finish"));

        assert_eq!(offline.access_units, streamed);
    }

    #[kithara::test(native, flash(false))]
    fn offline_track_holds_the_golden_shape(encode_saw_i16: &'static [u8]) {
        let track = encode_offline(&TestPcm::from_bytes(
            encode_saw_i16.to_vec(),
            consts::AAC_SAMPLE_RATE,
            consts::AAC_CHANNELS,
        ));
        let units = &track.access_units;

        assert_eq!(track.media_info.codec, Some(AudioCodec::AacLc));
        assert_eq!(track.media_info.sample_rate, Some(consts::AAC_SAMPLE_RATE));
        assert_eq!(track.media_info.channels, Some(consts::AAC_CHANNELS));
        assert_eq!(track.encoder_delay, consts::ENCODER_DELAY);
        assert_eq!(track.trailing_delay, consts::TRAILING_DELAY);
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
