use fdk_aac_sys as sys;

use super::encoder::{Encoder, EncoderParams};
use crate::{
    error::{EncodeError, EncodeResult},
    stream::{StreamEncoder, StreamParams},
    types::EncodedAccessUnit,
};

struct Consts;
impl Consts {
    const ACCESS_UNIT_CAPACITY: usize = 8 * 1024;
    const I16_SCALE: f32 = 32_768.0;
    const MAX_SAMPLE_RATE: u32 = 96_000;
    const MIN_SAMPLE_RATE: u32 = 8_000;
}

/// In-tree fdk-aac AAC-LC encoder behind [`crate::StreamEncoder`]. Raw access
/// units: whoever carries them adds the transport headers.
pub(crate) struct FdkStream {
    encoder: Encoder,
    channels: usize,
    frame_samples: usize,
    pending: Vec<i16>,
    output: Vec<u8>,
    emitted: u64,
    sample_rate: u32,
    timescale: u32,
}

impl FdkStream {
    pub(crate) fn new(params: &StreamParams) -> EncodeResult<Self> {
        let StreamParams {
            sample_rate,
            channels,
            bit_rate,
            timescale,
        } = *params;
        if !(Consts::MIN_SAMPLE_RATE..=Consts::MAX_SAMPLE_RATE).contains(&sample_rate) {
            return Err(EncodeError::InvalidInput(format!(
                "fdk-aac encodes {} Hz to {} Hz audio, got {sample_rate}",
                Consts::MIN_SAMPLE_RATE,
                Consts::MAX_SAMPLE_RATE
            )));
        }

        let encoder = Encoder::new(&EncoderParams {
            aot: sys::AUDIO_OBJECT_TYPE_AOT_AAC_LC,
            bit_rate: u32::try_from(bit_rate).map_err(|_| {
                EncodeError::InvalidInput("bit_rate does not fit into u32".to_owned())
            })?,
            channels,
            sample_rate,
            sbr: false,
        })?;

        let frame_samples = usize::try_from(encoder.info()?.frameLength).unwrap_or(0);
        if frame_samples != StreamEncoder::FRAME_SAMPLES {
            return Err(EncodeError::backend_message(format!(
                "fdk-aac opened a {frame_samples} sample AAC-LC frame, not {}",
                StreamEncoder::FRAME_SAMPLES
            )));
        }

        let channels = usize::from(channels);
        Ok(Self {
            encoder,
            channels,
            frame_samples,
            pending: Vec::with_capacity(frame_samples * channels),
            output: vec![0; Consts::ACCESS_UNIT_CAPACITY],
            emitted: 0,
            sample_rate,
            timescale,
        })
    }

    pub(crate) fn push(&mut self, samples: &[f32]) -> EncodeResult<Vec<EncodedAccessUnit>> {
        self.pending.extend(samples.iter().copied().map(to_i16));
        self.drain_full_frames()
    }

    pub(crate) fn finish(mut self) -> EncodeResult<Vec<EncodedAccessUnit>> {
        let mut units = self.drain_full_frames()?;
        if !self.pending.is_empty() {
            self.pending.resize(self.frame_samples * self.channels, 0);
            units.extend(self.drain_full_frames()?);
        }

        while let Some(encoded) = self.encoder.flush(&mut self.output)? {
            if encoded.output_size == 0 {
                continue;
            }
            let unit = Self::access_unit(
                &self.output[..encoded.output_size],
                self.emitted,
                self.frame_samples,
                self.sample_rate,
                self.timescale,
            )?;
            self.emitted += 1;
            units.push(unit);
        }
        Ok(units)
    }

    fn drain_full_frames(&mut self) -> EncodeResult<Vec<EncodedAccessUnit>> {
        let frame_input = self.frame_samples * self.channels;
        let mut units = Vec::new();
        while self.pending.len() >= frame_input {
            let encoded = self
                .encoder
                .encode(&self.pending[..frame_input], &mut self.output)?;
            if encoded.input_consumed == 0 {
                break;
            }
            self.pending.drain(..encoded.input_consumed);
            if encoded.output_size > 0 {
                let unit = Self::access_unit(
                    &self.output[..encoded.output_size],
                    self.emitted,
                    self.frame_samples,
                    self.sample_rate,
                    self.timescale,
                )?;
                self.emitted += 1;
                units.push(unit);
            }
        }
        Ok(units)
    }

    /// Access-unit boundaries are what gets rescaled, so durations tile the pts
    /// timeline exactly even when the ratio is fractional.
    fn access_unit(
        bytes: &[u8],
        index: u64,
        frame_samples: usize,
        sample_rate: u32,
        timescale: u32,
    ) -> EncodeResult<EncodedAccessUnit> {
        let frame_samples = u64::try_from(frame_samples).map_err(|_| {
            EncodeError::backend_message("frame size does not fit into u64".to_owned())
        })?;
        let pts = rescale(index * frame_samples, sample_rate, timescale)?;
        let end = rescale((index + 1) * frame_samples, sample_rate, timescale)?;
        let duration = u32::try_from(end.saturating_sub(pts)).map_err(|_| {
            EncodeError::backend_message(
                "access-unit duration does not fit into u32 in the target time base".to_owned(),
            )
        })?;

        Ok(EncodedAccessUnit {
            bytes: bytes.to_vec(),
            pts,
            dts: pts,
            duration,
            is_sync: true,
        })
    }
}

fn rescale(frames: u64, sample_rate: u32, timescale: u32) -> EncodeResult<u64> {
    let sample_rate = u128::from(sample_rate);
    let ticks = u128::from(frames) * u128::from(timescale) + sample_rate / 2;
    u64::try_from(ticks / sample_rate)
        .map_err(|_| EncodeError::backend_message("timestamp does not fit into u64".to_owned()))
}

fn to_i16(sample: f32) -> i16 {
    let scaled = (sample * Consts::I16_SCALE).clamp(f32::from(i16::MIN), f32::from(i16::MAX));
    #[cfg_attr(
        all(),
        expect(
            clippy::cast_possible_truncation,
            reason = "the value is clamped onto the i16 range on the line above"
        )
    )]
    let sample = scaled as i16;
    sample
}
