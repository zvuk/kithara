use std::{cmp, mem::size_of};

use fdk_aac_sys as sys;
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_stream::{AudioCodec, ContainerFormat};

use super::encoder::{Encoder, EncoderParams, audio_specific_config};
use crate::{
    EncodeError, EncodeResult,
    types::{EncodedAccessUnit, EncodedTrack, PackagedEncodeRequest, PcmSource},
};

mod consts {
    pub(super) const ACCESS_UNIT_CAPACITY: usize = 8 * 1024;
    pub(super) const CHANNELS: u16 = 2;
    pub(super) const FRAME_OUTPUT_SAMPLES: usize = 2048;
    pub(super) const MAX_FRAME_INPUT_SAMPLES: usize = FRAME_OUTPUT_SAMPLES * CHANNELS as usize;
}

#[derive(Clone, Copy)]
pub(crate) enum AacHeProfile {
    V1,
    V2,
}

impl AacHeProfile {
    const fn aot(self) -> sys::AUDIO_OBJECT_TYPE {
        match self {
            Self::V1 => sys::AUDIO_OBJECT_TYPE_AOT_SBR,
            Self::V2 => sys::AUDIO_OBJECT_TYPE_AOT_PS,
        }
    }

    const fn codec(self) -> AudioCodec {
        match self {
            Self::V1 => AudioCodec::AacHe,
            Self::V2 => AudioCodec::AacHeV2,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::V1 => "HE-AAC v1",
            Self::V2 => "HE-AAC v2",
        }
    }
}

pub(crate) struct AacHeEncoder;

impl AacHeEncoder {
    pub(crate) fn encode<S>(
        pools: &PoolRegion<S>,
        request: &PackagedEncodeRequest<'_>,
        profile: AacHeProfile,
    ) -> EncodeResult<EncodedTrack>
    where
        S: HasPool<u8>,
    {
        request.validate()?;

        let sample_rate = request.pcm.sample_rate();
        let channels = request.pcm.channels();
        if channels != consts::CHANNELS {
            return Err(EncodeError::InvalidInput(format!(
                "{} requires stereo input (channels={})",
                profile.name(),
                consts::CHANNELS
            )));
        }

        let mut encoder = Encoder::new(&EncoderParams {
            channels,
            sample_rate,
            aot: profile.aot(),
            bit_rate: request.bit_rate.try_into().map_err(|_| {
                EncodeError::InvalidInput("bit_rate does not fit into u32".to_owned())
            })?,
            sbr: true,
        })?;
        let info = encoder.info()?;
        let default_frame_input = consts::FRAME_OUTPUT_SAMPLES * usize::from(channels);
        let frame_input_samples = usize::try_from(info.frameLength)
            .ok()
            .map(|n| n * usize::from(channels))
            .filter(|n| *n > 0)
            .unwrap_or(default_frame_input);
        if frame_input_samples > consts::MAX_FRAME_INPUT_SAMPLES {
            return Err(EncodeError::backend_message(format!(
                "{} requested {frame_input_samples} input samples, maximum is {}",
                profile.name(),
                consts::MAX_FRAME_INPUT_SAMPLES
            )));
        }

        let asc = audio_specific_config(&info);

        let mut units: Vec<EncodedAccessUnit> = Vec::new();
        let mut pts: u64 = 0;
        let timescale = request.timescale;
        let frame_samples = u64::try_from(frame_input_samples).map_err(|_| {
            EncodeError::backend_message("frame sample count does not fit into u64".to_owned())
        })? / u64::from(channels);
        let frame_pts_step = frame_samples * u64::from(timescale) / u64::from(sample_rate);
        let frame_pts_step = frame_pts_step.max(1);
        let frame_duration = u32::try_from(frame_pts_step).map_err(|_| {
            EncodeError::backend_message("frame duration does not fit into u32".to_owned())
        })?;

        pump_pcm_into_encoder(request.pcm, pools, frame_input_samples, |input| {
            let mut output = [0u8; consts::ACCESS_UNIT_CAPACITY];
            let encoded = encoder.encode(input, &mut output)?;
            if encoded.output_size > 0 {
                units.push(EncodedAccessUnit {
                    pts,
                    bytes: output[..encoded.output_size].to_vec(),
                    dts: pts,
                    duration: frame_duration,
                    is_sync: true,
                });
                pts = pts.saturating_add(frame_pts_step);
            }
            Ok::<usize, EncodeError>(encoded.input_consumed)
        })?;

        let empty: [i16; 0] = [];
        loop {
            let mut output = [0u8; consts::ACCESS_UNIT_CAPACITY];
            let encoded = encoder.encode(&empty, &mut output)?;
            if encoded.output_size == 0 {
                break;
            }
            units.push(EncodedAccessUnit {
                pts,
                bytes: output[..encoded.output_size].to_vec(),
                dts: pts,
                duration: frame_duration,
                is_sync: true,
            });
            pts = pts.saturating_add(frame_pts_step);
        }

        let mut media_info = request.media_info.clone();
        media_info.codec = Some(profile.codec());
        media_info.container = Some(ContainerFormat::Fmp4);
        media_info.sample_rate = Some(sample_rate);
        media_info.channels = Some(channels);

        Ok(EncodedTrack {
            media_info,
            timescale: request.timescale,
            bit_rate: request.bit_rate,
            codec_config: asc,
            packets_per_segment: request.packets_per_segment,
            encoder_delay: request.encoder_delay,
            trailing_delay: request.trailing_delay,
            access_units: units,
        })
    }

    pub(crate) const fn frame_samples() -> usize {
        consts::FRAME_OUTPUT_SAMPLES
    }
}

fn pump_pcm_into_encoder<S, F>(
    pcm: &dyn PcmSource,
    pools: &PoolRegion<S>,
    frame_input_samples: usize,
    mut feed: F,
) -> EncodeResult<()>
where
    S: HasPool<u8>,
    F: FnMut(&[i16]) -> EncodeResult<usize>,
{
    let total = pcm.total_byte_len().unwrap_or(0);
    let bytes_per_sample = size_of::<i16>();
    let frame_bytes = frame_input_samples
        .checked_mul(bytes_per_sample)
        .ok_or_else(|| {
            EncodeError::backend_message("HE-AAC frame byte count overflow".to_owned())
        })?;
    let mut byte_offset: usize = 0;
    let mut samples = [0_i16; consts::MAX_FRAME_INPUT_SAMPLES];
    let mut sample_count = 0;
    let mut raw = pools
        .get_with_len::<u8>(frame_bytes)
        .map_err(|error| EncodeError::Backend(Box::new(error)))?;
    while byte_offset < total {
        let remaining_samples = frame_input_samples - sample_count;
        let want = cmp::min(remaining_samples * bytes_per_sample, total - byte_offset);
        let n = pcm.read_pcm_at(byte_offset, &mut raw[..want]);
        if n == 0 {
            break;
        }
        if !n.is_multiple_of(bytes_per_sample) {
            return Err(EncodeError::InvalidInput(format!(
                "PCM source returned {n} bytes, not a whole i16 sample"
            )));
        }
        byte_offset += n;
        for chunk in raw[..n].chunks_exact(bytes_per_sample) {
            samples[sample_count] = i16::from_le_bytes([chunk[0], chunk[1]]);
            sample_count += 1;
        }
        if sample_count == frame_input_samples {
            let consumed = feed(&samples[..frame_input_samples])?;
            if consumed == 0 {
                return Err(EncodeError::backend_message(
                    "HE-AAC encoder made no input progress".to_owned(),
                ));
            }
            if consumed > sample_count {
                return Err(EncodeError::backend_message(format!(
                    "HE-AAC encoder consumed {consumed} samples from a {sample_count}-sample frame"
                )));
            }
            samples.copy_within(consumed..sample_count, 0);
            sample_count -= consumed;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kithara_test_fixtures::mock_fixtures::i16_ramp;
    use kithara_test_utils::kithara;

    use super::pump_pcm_into_encoder;
    use crate::{EncodeError, PcmSource, test_pools};

    struct ChunkedPcm {
        reads: AtomicUsize,
        bytes: Vec<u8>,
        max_read: usize,
    }

    impl ChunkedPcm {
        fn new(samples: &[i16], max_read: usize) -> Self {
            let bytes = samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect();
            Self {
                bytes,
                max_read,
                reads: AtomicUsize::new(0),
            }
        }
    }

    impl PcmSource for ChunkedPcm {
        fn channels(&self) -> u16 {
            1
        }

        fn read_pcm_at(&self, offset: usize, buf: &mut [u8]) -> usize {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let Some(remaining) = self.bytes.get(offset..) else {
                return 0;
            };
            let read = remaining.len().min(buf.len()).min(self.max_read);
            buf[..read].copy_from_slice(&remaining[..read]);
            read
        }

        fn sample_rate(&self) -> u32 {
            48_000
        }

        fn total_byte_len(&self) -> Option<usize> {
            Some(self.bytes.len())
        }
    }

    fn run(
        pcm: &ChunkedPcm,
        mut feed: impl FnMut(&[i16]) -> Result<usize, EncodeError>,
    ) -> Result<(), EncodeError> {
        pump_pcm_into_encoder(pcm, &test_pools::pools(), 4, |input| feed(input))
    }

    #[kithara::test(native, flash(false))]
    fn partial_source_reads_fill_complete_frames(i16_ramp: Vec<i16>) {
        let pcm = ChunkedPcm::new(&i16_ramp, 2);
        let mut frames = Vec::new();

        run(&pcm, |input| {
            frames.push(<[i16; 4]>::try_from(input).expect("four-sample frame"));
            Ok(input.len())
        })
        .expect("partial reads");

        assert_eq!(frames, [[1, 2, 3, 4], [5, 6, 7, 8]]);
        assert_eq!(pcm.reads.load(Ordering::Relaxed), 8);
    }

    #[kithara::test(native, flash(false))]
    fn odd_byte_read_is_invalid_input(i16_ramp: Vec<i16>) {
        let pcm = ChunkedPcm::new(&i16_ramp[..4], 3);

        let error = run(&pcm, |_| Ok(4)).expect_err("odd byte read");

        assert!(matches!(error, EncodeError::InvalidInput(_)), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn zero_feed_progress_is_an_error(i16_ramp: Vec<i16>) {
        let pcm = ChunkedPcm::new(&i16_ramp[..4], usize::MAX);

        let error = run(&pcm, |_| Ok(0)).expect_err("zero progress");

        assert!(
            error.to_string().contains("made no input progress"),
            "{error}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn feed_cannot_consume_past_the_frame(i16_ramp: Vec<i16>) {
        let pcm = ChunkedPcm::new(&i16_ramp[..4], usize::MAX);

        let error = run(&pcm, |input| Ok(input.len() + 1)).expect_err("over-consumption");

        assert!(error.to_string().contains("consumed 5 samples"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn partial_consumption_preserves_remaining_sample_order(i16_ramp: Vec<i16>) {
        let pcm = ChunkedPcm::new(&i16_ramp[..6], usize::MAX);
        let mut frames = Vec::new();

        run(&pcm, |input| {
            frames.push(<[i16; 4]>::try_from(input).expect("four-sample frame"));
            Ok(if frames.len() == 1 { 2 } else { input.len() })
        })
        .expect("partial consumption");

        assert_eq!(frames, [[1, 2, 3, 4], [3, 4, 5, 6]]);
    }
}
