use fdk_aac_sys as sys;
use num_traits::cast;

use super::encoder::{Encoder, EncoderParams};
use crate::{
    error::{EncodeError, EncodeResult},
    stream::{AacStream, StreamEncoder, StreamParams},
    types::EncodedAccessUnit,
};

mod consts {
    use super::StreamEncoder;

    pub(super) const ACCESS_UNIT_CAPACITY: usize = 8 * 1024;
    pub(super) const I16_SCALE: f32 = 32_768.0;
    pub(super) const MAX_CHANNELS: usize = 6;
    pub(super) const MAX_FRAME_INPUT_SAMPLES: usize = StreamEncoder::FRAME_SAMPLES * MAX_CHANNELS;
    pub(super) const MAX_SAMPLE_RATE: u32 = 96_000;
    pub(super) const MIN_SAMPLE_RATE: u32 = 8_000;
}

pub(crate) struct FdkStream {
    encoder: Encoder,
    pending: [i16; consts::MAX_FRAME_INPUT_SAMPLES],
    output: [u8; consts::ACCESS_UNIT_CAPACITY],
    sample_rate: u32,
    timescale: u32,
    emitted: u64,
    channels: usize,
    frame_samples: usize,
    pending_len: usize,
}

impl AacStream for FdkStream {
    fn finish(mut self: Box<Self>) -> EncodeResult<Vec<EncodedAccessUnit>> {
        let mut units: Vec<EncodedAccessUnit> = Vec::new();
        if self.pending_len > 0 {
            let frame_input = self.frame_input();
            self.pending[self.pending_len..frame_input].fill(0);
            self.pending_len = frame_input;
            self.encode_pending(&mut units)?;
        }

        while let Some(encoded) = self.encoder.flush(&mut self.output)? {
            if encoded.output_size > 0 {
                units.push(self.access_unit(encoded.output_size)?);
            }
        }
        Ok(units)
    }

    fn push(&mut self, mut samples: &[f32]) -> EncodeResult<Vec<EncodedAccessUnit>> {
        let frame_input = self.frame_input();
        let mut units: Vec<EncodedAccessUnit> = Vec::new();
        while !samples.is_empty() {
            let count = (frame_input - self.pending_len).min(samples.len());
            let pending_end = self.pending_len + count;
            for (pending, &sample) in self.pending[self.pending_len..pending_end]
                .iter_mut()
                .zip(&samples[..count])
            {
                *pending = to_i16(sample);
            }
            self.pending_len = pending_end;
            samples = &samples[count..];

            if self.pending_len == frame_input {
                self.encode_pending(&mut units)?;
            }
        }
        Ok(units)
    }
}

impl FdkStream {
    pub(crate) fn new(params: &StreamParams) -> EncodeResult<Self> {
        let StreamParams {
            sample_rate,
            channels,
            bit_rate,
            timescale,
        } = *params;
        if usize::from(channels) > consts::MAX_CHANNELS {
            return Err(EncodeError::InvalidInput(format!(
                "fdk-aac carries no channel mode for {channels} channels"
            )));
        }
        if !(consts::MIN_SAMPLE_RATE..=consts::MAX_SAMPLE_RATE).contains(&sample_rate) {
            return Err(EncodeError::InvalidInput(format!(
                "fdk-aac encodes {} Hz to {} Hz audio, got {sample_rate}",
                consts::MIN_SAMPLE_RATE,
                consts::MAX_SAMPLE_RATE
            )));
        }

        let encoder = Encoder::new(&EncoderParams {
            channels,
            sample_rate,
            aot: sys::AUDIO_OBJECT_TYPE_AOT_AAC_LC,
            bit_rate: u32::try_from(bit_rate).map_err(|_| {
                EncodeError::InvalidInput("bit_rate does not fit into u32".to_owned())
            })?,
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
            sample_rate,
            timescale,
            pending: [0; consts::MAX_FRAME_INPUT_SAMPLES],
            pending_len: 0,
            output: [0; consts::ACCESS_UNIT_CAPACITY],
            emitted: 0,
        })
    }

    fn access_unit(&mut self, size: usize) -> EncodeResult<EncodedAccessUnit> {
        let frame_samples = u64::try_from(self.frame_samples).map_err(|_| {
            EncodeError::backend_message("frame size does not fit into u64".to_owned())
        })?;
        let pts = self.rescale(self.emitted * frame_samples)?;
        let end = self.rescale((self.emitted + 1) * frame_samples)?;
        let duration = u32::try_from(end.saturating_sub(pts)).map_err(|_| {
            EncodeError::backend_message(
                "access-unit duration does not fit into u32 in the target time base".to_owned(),
            )
        })?;
        self.emitted += 1;

        Ok(EncodedAccessUnit {
            bytes: self.output[..size].to_vec(),
            pts,
            dts: pts,
            duration,
            is_sync: true,
        })
    }

    fn encode_pending(&mut self, units: &mut Vec<EncodedAccessUnit>) -> EncodeResult<()> {
        let frame_input = self.frame_input();
        let encoded = self
            .encoder
            .encode(&self.pending[..frame_input], &mut self.output)?;
        if encoded.input_consumed != frame_input {
            return Err(EncodeError::backend_message(format!(
                "fdk-aac took {} samples of a {frame_input} sample frame",
                encoded.input_consumed
            )));
        }
        self.pending_len = 0;
        if encoded.output_size > 0 {
            units.push(self.access_unit(encoded.output_size)?);
        }
        Ok(())
    }

    const fn frame_input(&self) -> usize {
        self.frame_samples * self.channels
    }

    fn rescale(&self, frames: u64) -> EncodeResult<u64> {
        let sample_rate = u128::from(self.sample_rate);
        let ticks = u128::from(frames) * u128::from(self.timescale) + sample_rate / 2;
        u64::try_from(ticks / sample_rate)
            .map_err(|_| EncodeError::backend_message("timestamp does not fit into u64".to_owned()))
    }
}

fn to_i16(sample: f32) -> i16 {
    let scaled = (sample * consts::I16_SCALE)
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX));
    cast(scaled).unwrap_or(0)
}
