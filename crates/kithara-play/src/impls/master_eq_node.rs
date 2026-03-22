use core::num::NonZeroU32;

use biquad::{Biquad, Coefficients, DirectForm1, ToHertz};
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
use kithara_audio::{EqBandConfig, FilterKind, compute_coefficients};

use super::shared_eq::{EQ_MAX_GAIN_DB, EQ_MIN_GAIN_DB};

const PASSTHROUGH: Coefficients<f32> = Coefficients {
    a1: 0.0,
    a2: 0.0,
    b0: 1.0,
    b1: 0.0,
    b2: 0.0,
};

/// Smoothing time constant in milliseconds.
const SMOOTH_TIME_MS: f32 = 10.0;

/// Number of samples between coefficient recalculations during smoothing.
const SMOOTH_BLOCK_SIZE: usize = 32;

/// Minimum stereo channel count for processing.
const MIN_STEREO: usize = 2;

/// Convergence threshold for gain smoothing (dB).
const SMOOTH_CONVERGENCE_THRESHOLD: f32 = 0.01;

/// Factor to convert milliseconds to seconds.
const MS_TO_SECONDS: f32 = 1000.0;

/// Filter kind ordinal for high-shelf filter.
const FILTER_KIND_HIGH_SHELF: u8 = 2;

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
    filters_l: Vec<DirectForm1<f32>>,
    filters_r: Vec<DirectForm1<f32>>,
    /// Current smoothed gains (one per band).
    current_gains: Vec<f32>,
    params: MasterEqNode,
    sample_rate: NonZeroU32,
    /// One-pole smoother coefficient per block boundary.
    smooth_coeff: f32,
}

impl MasterEqProcessor {
    fn new(params: MasterEqNode, sample_rate: NonZeroU32) -> Self {
        let band_count = params.bands.len();
        // Pre-warm: current gains start at target to avoid fade-in click.
        let current_gains: Vec<f32> = params.bands.iter().map(|b| b.gain_db).collect();
        let smooth_coeff = Self::calc_smooth_coeff(sample_rate);

        let mut this = Self {
            filters_l: vec![DirectForm1::<f32>::new(PASSTHROUGH); band_count],
            filters_r: vec![DirectForm1::<f32>::new(PASSTHROUGH); band_count],
            current_gains,
            params,
            sample_rate,
            smooth_coeff,
        };
        this.rebuild_all_coefficients();
        this
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "sample rate and block size are small integers"
    )]
    fn calc_smooth_coeff(sample_rate: NonZeroU32) -> f32 {
        let tau = SMOOTH_TIME_MS / MS_TO_SECONDS;
        let effective_rate = sample_rate.get() as f32 / SMOOTH_BLOCK_SIZE as f32;
        1.0 - (-1.0 / (tau * effective_rate)).exp()
    }

    /// Rebuild all filter coefficients from current gains (used at init / stream change).
    fn rebuild_all_coefficients(&mut self) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "Firewheel sample rates are far below f32 integer precision bounds"
        )]
        let sample_rate_hz = (self.sample_rate.get() as f32).hz();

        for (idx, band) in self.params.bands.iter().enumerate() {
            let eq_band = to_eq_band_config(band);
            let coeffs = compute_coefficients(&eq_band, self.current_gains[idx], sample_rate_hz);
            self.filters_l[idx] = DirectForm1::new(coeffs);
            self.filters_r[idx] = DirectForm1::new(coeffs);
        }
    }

    /// Smooth one band's gain toward target, returning true if coefficients were updated.
    fn smooth_and_update_band(&mut self, idx: usize, sample_rate_hz: biquad::Hertz<f32>) -> bool {
        let target = self.params.bands[idx].gain_db;
        let current = self.current_gains[idx];
        let diff = target - current;

        if diff.abs() < SMOOTH_CONVERGENCE_THRESHOLD {
            if (current - target).abs() > f32::EPSILON {
                self.current_gains[idx] = target;
                let eq_band = to_eq_band_config(&self.params.bands[idx]);
                let coeffs = compute_coefficients(&eq_band, target, sample_rate_hz);
                self.filters_l[idx].update_coefficients(coeffs);
                self.filters_r[idx].update_coefficients(coeffs);
                return true;
            }
            return false;
        }

        let new_gain = current + diff * self.smooth_coeff;
        self.current_gains[idx] = new_gain;

        let eq_band = to_eq_band_config(&self.params.bands[idx]);
        let coeffs = compute_coefficients(&eq_band, new_gain, sample_rate_hz);
        self.filters_l[idx].update_coefficients(coeffs);
        self.filters_r[idx].update_coefficients(coeffs);
        true
    }
}

fn to_eq_band_config(band: &MasterEqBand) -> EqBandConfig {
    EqBandConfig {
        frequency: band.frequency,
        q_factor: band.q_factor,
        gain_db: band.gain_db,
        kind: match band.kind {
            0 => FilterKind::LowShelf,
            FILTER_KIND_HIGH_SHELF => FilterKind::HighShelf,
            _ => FilterKind::Peaking,
        },
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
        for patch in events.drain_patches::<MasterEqNode>() {
            self.params.apply(patch);
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

        #[expect(
            clippy::cast_precision_loss,
            reason = "Firewheel sample rates are far below f32 integer precision bounds"
        )]
        let sample_rate_hz = (self.sample_rate.get() as f32).hz();
        let band_count = self.params.bands.len();

        // Process in blocks of SMOOTH_BLOCK_SIZE for smooth coefficient updates.
        let mut offset = 0;
        while offset < info.frames {
            let block_end = (offset + SMOOTH_BLOCK_SIZE).min(info.frames);

            // Update smoothed coefficients at block boundaries.
            for band in 0..band_count {
                self.smooth_and_update_band(band, sample_rate_hz);
            }

            // Run filters on this block.
            for frame in offset..block_end {
                let mut sample_l = in_l[frame];
                let mut sample_r = in_r[frame];

                for band in 0..band_count {
                    sample_l = self.filters_l[band].run(sample_l);
                    sample_r = self.filters_r[band].run(sample_r);
                }

                out_l[frame] = sample_l;
                out_r[frame] = sample_r;
            }

            offset = block_end;
        }

        ProcessStatus::OutputsModified
    }

    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.sample_rate = stream_info.sample_rate;
        self.smooth_coeff = Self::calc_smooth_coeff(self.sample_rate);
        self.rebuild_all_coefficients();
    }
}
