use core::num::NonZeroU32;

use biquad::{Biquad, Coefficients, DirectForm1, ToHertz, Type};
use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    diff::{Diff, Patch},
    event::ProcEvents,
    mask::MaskType,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_audio::effects::generate_log_spaced_bands;

use super::shared_eq::{EQ_MAX_GAIN_DB, EQ_MIN_GAIN_DB};

const PASSTHROUGH: Coefficients<f32> = Coefficients {
    a1: 0.0,
    a2: 0.0,
    b0: 1.0,
    b1: 0.0,
    b2: 0.0,
};

#[derive(Diff, Patch, Debug, Clone, Copy, PartialEq)]
pub(crate) struct MasterEqBand {
    pub(crate) frequency: f32,
    pub(crate) gain_db: f32,
    pub(crate) q_factor: f32,
}

#[derive(Diff, Patch, Debug, Clone, PartialEq)]
pub(crate) struct MasterEqNode {
    pub(crate) bands: Vec<MasterEqBand>,
    pub(crate) enabled: bool,
}

impl MasterEqNode {
    pub(crate) fn new(bands: usize) -> Self {
        let bands = generate_log_spaced_bands(bands)
            .into_iter()
            .map(|band| MasterEqBand {
                frequency: band.frequency,
                gain_db: band.gain_db,
                q_factor: band.q_factor,
            })
            .collect();

        Self {
            bands,
            enabled: true,
        }
    }

    pub(crate) fn set_gain(&mut self, index: usize, gain_db: f32) {
        if let Some(band) = self.bands.get_mut(index) {
            band.gain_db = gain_db.clamp(EQ_MIN_GAIN_DB, EQ_MAX_GAIN_DB);
        }
    }
}

impl AudioNode for MasterEqNode {
    type Configuration = EmptyConfig;

    fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name("master_eq")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::STEREO,
                num_outputs: ChannelCount::STEREO,
            })
    }

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        MasterEqProcessor::new(self.clone(), cx.stream_info.sample_rate)
    }
}

struct MasterEqProcessor {
    filters_l: Vec<DirectForm1<f32>>,
    filters_r: Vec<DirectForm1<f32>>,
    params: MasterEqNode,
    sample_rate: NonZeroU32,
}

impl MasterEqProcessor {
    fn new(params: MasterEqNode, sample_rate: NonZeroU32) -> Self {
        let mut this = Self {
            filters_l: Vec::new(),
            filters_r: Vec::new(),
            params,
            sample_rate,
        };
        this.update_filters();
        this
    }

    fn update_filters(&mut self) {
        let band_count = self.params.bands.len();

        if self.filters_l.len() != band_count {
            self.filters_l = vec![DirectForm1::<f32>::new(PASSTHROUGH); band_count];
            self.filters_r = vec![DirectForm1::<f32>::new(PASSTHROUGH); band_count];
        }

        for (idx, band) in self.params.bands.iter().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "Firewheel sample rates are far below f32 integer precision bounds"
            )]
            let sample_rate_hz = (self.sample_rate.get() as f32).hz();
            let coeffs = if band.gain_db.abs() < 0.01 {
                PASSTHROUGH
            } else {
                Coefficients::<f32>::from_params(
                    Type::PeakingEQ(band.gain_db),
                    sample_rate_hz,
                    band.frequency.hz(),
                    band.q_factor,
                )
                .unwrap_or(PASSTHROUGH)
            };

            self.filters_l[idx] = DirectForm1::new(coeffs);
            self.filters_r[idx] = DirectForm1::new(coeffs);
        }
    }
}

impl AudioNodeProcessor for MasterEqProcessor {
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        events: &mut ProcEvents,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let mut updated = false;
        for patch in events.drain_patches::<MasterEqNode>() {
            self.params.apply(patch);
            updated = true;
        }

        if updated {
            self.update_filters();
        }

        if buffers.inputs.len() < 2 || buffers.outputs.len() < 2 {
            return ProcessStatus::Bypass;
        }

        if !self.params.enabled || info.in_silence_mask.all_channels_silent(2) {
            buffers.outputs[0].copy_from_slice(buffers.inputs[0]);
            buffers.outputs[1].copy_from_slice(buffers.inputs[1]);
            return ProcessStatus::OutputsModifiedWithMask(MaskType::Silence(info.in_silence_mask));
        }

        let in_l = &buffers.inputs[0][..info.frames];
        let in_r = &buffers.inputs[1][..info.frames];
        let Some((out_l_slice, out_r_slice_slice)) = buffers.outputs.split_first_mut() else {
            return ProcessStatus::Bypass;
        };
        let Some(out_r_slice) = out_r_slice_slice.first_mut() else {
            return ProcessStatus::Bypass;
        };
        let out_l = &mut out_l_slice[..info.frames];
        let out_r = &mut out_r_slice[..info.frames];

        for frame in 0..info.frames {
            let mut sample_l = in_l[frame];
            let mut sample_r = in_r[frame];

            for band in 0..self.params.bands.len() {
                sample_l = self.filters_l[band].run(sample_l);
                sample_r = self.filters_r[band].run(sample_r);
            }

            out_l[frame] = sample_l;
            out_r[frame] = sample_r;
        }

        ProcessStatus::OutputsModified
    }

    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.sample_rate = stream_info.sample_rate;
        self.update_filters();
    }
}
