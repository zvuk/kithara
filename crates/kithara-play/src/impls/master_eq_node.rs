use core::num::NonZeroU32;

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
use kithara_audio::{EqBandConfig, IsolatorEq};

use super::shared_eq::{EQ_MAX_GAIN_DB, EQ_MIN_GAIN_DB};

/// Minimum stereo channel count for processing.
const MIN_STEREO: usize = 2;

#[derive(Diff, Patch, Debug, Clone, Copy, PartialEq)]
pub(crate) struct MasterEqBand {
    pub(crate) frequency: f32,
    pub(crate) gain_db: f32,
    pub(crate) q_factor: f32,
    pub(crate) kind: u8,
}

#[derive(Diff, Patch, Debug, Clone, PartialEq)]
pub(crate) struct MasterEqNode {
    pub(crate) bands: Vec<MasterEqBand>,
    pub(crate) enabled: bool,
}

impl MasterEqNode {
    pub(crate) fn new(layout: &[EqBandConfig]) -> Self {
        let bands = layout
            .iter()
            .map(|band| MasterEqBand {
                frequency: band.frequency,
                gain_db: band.gain_db,
                q_factor: band.q_factor,
                kind: band.kind as u8,
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
    eq_l: IsolatorEq,
    eq_r: IsolatorEq,
    params: MasterEqNode,
    sample_rate: NonZeroU32,
}

impl MasterEqProcessor {
    fn new(params: MasterEqNode, sample_rate: NonZeroU32) -> Self {
        let bands = bands_from_params(&params);
        let mut eq_l = IsolatorEq::new(&bands, sample_rate.get());
        let mut eq_r = IsolatorEq::new(&bands, sample_rate.get());

        for (i, band) in params.bands.iter().enumerate() {
            eq_l.set_gain(i, band.gain_db);
            eq_r.set_gain(i, band.gain_db);
        }

        Self {
            eq_l,
            eq_r,
            params,
            sample_rate,
        }
    }

    fn sync_gains(&mut self) {
        for (i, band) in self.params.bands.iter().enumerate() {
            self.eq_l.set_gain(i, band.gain_db);
            self.eq_r.set_gain(i, band.gain_db);
        }
    }
}

fn bands_from_params(params: &MasterEqNode) -> Vec<EqBandConfig> {
    params
        .bands
        .iter()
        .map(|b| EqBandConfig {
            frequency: b.frequency,
            q_factor: b.q_factor,
            gain_db: b.gain_db,
            kind: b.kind.into(),
        })
        .collect()
}

impl AudioNodeProcessor for MasterEqProcessor {
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        events: &mut ProcEvents,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let mut dirty = false;
        for patch in events.drain_patches::<MasterEqNode>() {
            self.params.apply(patch);
            dirty = true;
        }
        if dirty {
            self.sync_gains();
        }

        if buffers.inputs.len() < MIN_STEREO || buffers.outputs.len() < MIN_STEREO {
            return ProcessStatus::Bypass;
        }

        if !self.params.enabled || info.in_silence_mask.all_channels_silent(MIN_STEREO) {
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
            out_l[frame] = self.eq_l.process_sample(in_l[frame]);
            out_r[frame] = self.eq_r.process_sample(in_r[frame]);
        }

        ProcessStatus::OutputsModified
    }

    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.sample_rate = stream_info.sample_rate;
        self.eq_l.update_sample_rate(self.sample_rate.get());
        self.eq_r.update_sample_rate(self.sample_rate.get());
    }
}
