#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use core::{mem, num::NonZeroU32};

use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    diff::{Diff, Patch, PatchError},
    event::{NodeEventType, ParamData, ProcEvents},
    mask::MaskType,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_bufpool::{HasPool, PoolError};
use kithara_dsp::{
    fade::FadeCurve,
    param::{Mix, MixDSP},
};
use kithara_test_utils::kithara;
use tracing::warn;

use crate::{
    GainDb,
    eq::{EqBandConfig, EqConfig, IsolatorEq},
};

#[derive(Diff, Patch, Debug, Clone, Copy, PartialEq)]
pub(crate) struct MasterEqBand {
    pub(crate) frequency: f32,
    pub(crate) gain_db: f32,
    pub(crate) q_factor: f32,
    pub(crate) kind: u8,
}

#[derive(Diff, Debug)]
#[derive_where::derive_where(Clone)]
pub struct MasterEqNode<S> {
    pub(crate) bands: Vec<MasterEqBand>,
    pub(crate) enabled: bool,
    #[diff(skip)]
    config: EqConfig<S>,
}

/// A band layout on its way to the processor. Built on the control thread;
/// leaves the audio thread carrying the retired pair, so nothing is freed there.
#[derive(Default)]
pub(in crate::node) struct MasterEqLayout {
    equalizers: Option<(IsolatorEq, IsolatorEq)>,
    bands: Vec<MasterEqBand>,
}

enum LayoutUpdate {
    Pending(MasterEqLayout),
    Retired(MasterEqLayout),
}

impl LayoutUpdate {
    fn into_inner(self) -> MasterEqLayout {
        match self {
            Self::Pending(layout) | Self::Retired(layout) => layout,
        }
    }
}

/// An opaque runtime parameter patch for [`MasterEqNode`].
pub struct MasterEqNodePatch(MasterEqNodePatchKind);

enum MasterEqNodePatchKind {
    Bands(<Vec<MasterEqBand> as Patch>::Patch),
    Enabled(<bool as Patch>::Patch),
}

impl<S> Patch for MasterEqNode<S> {
    type Patch = MasterEqNodePatch;

    fn apply(&mut self, patch: Self::Patch) {
        match patch.0 {
            MasterEqNodePatchKind::Bands(patch) => self.bands.apply(patch),
            MasterEqNodePatchKind::Enabled(patch) => self.enabled.apply(patch),
        }
    }

    fn patch(data: &ParamData, path: &[u32]) -> Result<Self::Patch, PatchError> {
        match path {
            [0, tail @ ..] => Ok(MasterEqNodePatch(MasterEqNodePatchKind::Bands(<Vec<
                MasterEqBand,
            > as Patch>::patch(
                data, tail,
            )?))),
            [1, tail @ ..] => Ok(MasterEqNodePatch(MasterEqNodePatchKind::Enabled(
                bool::patch(data, tail)?,
            ))),
            _ => Err(PatchError::InvalidPath),
        }
    }
}

impl<S> MasterEqNode<S> {
    #[must_use]
    pub fn new(config: EqConfig<S>, layout: &[EqBandConfig]) -> Self {
        let bands = layout
            .iter()
            .map(|band| MasterEqBand {
                frequency: band.frequency(),
                gain_db: f32::from(band.gain_db()),
                q_factor: band.q_factor(),
                kind: band.kind() as u8,
            })
            .collect();

        Self {
            bands,
            config,
            enabled: true,
        }
    }

    #[must_use]
    pub fn band_count(&self) -> usize {
        self.bands.len()
    }

    /// Prepares a layout event without changing the running equalizer.
    ///
    /// # Errors
    /// Returns the pool error if the replacement cannot be prepared.
    pub fn layout_event(&self, sample_rate: NonZeroU32) -> Result<NodeEventType, PoolError>
    where
        S: HasPool<f32>,
    {
        let equalizers = build_equalizers(self, sample_rate)?;
        Ok(NodeEventType::custom(MasterEqLayout {
            bands: self.bands.clone(),
            equalizers: Some(equalizers),
        }))
    }

    pub fn set_gain(&mut self, index: usize, gain_db: GainDb) {
        if let Some(band) = self.bands.get_mut(index) {
            band.gain_db = f32::from(gain_db);
        }
    }
}

impl<S> AudioNode for MasterEqNode<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> Result<impl AudioNodeProcessor, NodeError> {
        Ok(MasterEqProcessor::new(self.clone(), cx.stream_info))
    }

    fn info(&self, _config: &Self::Configuration) -> Result<AudioNodeInfo, NodeError> {
        Ok(AudioNodeInfo::new()
            .debug_name("master_eq")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::STEREO,
                num_outputs: ChannelCount::STEREO,
            }))
    }
}

struct MasterEqProcessor<S> {
    params: MasterEqNode<S>,
    crossover: MixDSP,
    sample_rate: NonZeroU32,
    active: Option<(IsolatorEq, IsolatorEq)>,
    layout: Option<LayoutUpdate>,
    retiring: Option<(IsolatorEq, IsolatorEq)>,
}

impl<S> MasterEqProcessor<S>
where
    S: HasPool<f32>,
{
    fn new(params: MasterEqNode<S>, stream_info: &StreamInfo) -> Self {
        let active = match build_equalizers(&params, stream_info.sample_rate) {
            Ok(equalizers) => Some(equalizers),
            Err(error) => {
                warn!(%error, "master EQ disabled because its pooled scratch allocation failed");
                None
            }
        };
        let crossover = MixDSP::new(
            Mix::FULLY_WET,
            FadeCurve::Linear,
            params.config.smoothing(),
            stream_info.sample_rate,
        );

        Self {
            params,
            active,
            crossover,
            retiring: None,
            layout: None,
            sample_rate: stream_info.sample_rate,
        }
    }

    fn advance_layout(&mut self) {
        if !self.crossover.has_settled() {
            return;
        }
        match self.layout.take() {
            Some(LayoutUpdate::Pending(mut layout)) => {
                mem::swap(&mut self.params.bands, &mut layout.bands);
                let incoming = layout.equalizers.take();
                layout.equalizers = self.retiring.take();
                self.retiring = self.active.take();
                self.active = incoming;
                self.crossover.set_mix(Mix::FULLY_DRY, FadeCurve::Linear);
                self.crossover.reset_to_target();
                self.crossover.set_mix(Mix::FULLY_WET, FadeCurve::Linear);
                self.layout = Some(LayoutUpdate::Retired(layout));
                self.sync_gains();
            }
            previous => self.layout = previous,
        }
    }

    fn apply_patch(&mut self, patch: MasterEqNodePatch) {
        match (&mut self.layout, patch.0) {
            (Some(LayoutUpdate::Pending(layout)), MasterEqNodePatchKind::Bands(patch)) => {
                layout.bands.apply(patch);
            }
            (_, patch) => self.params.apply(MasterEqNodePatch(patch)),
        }
    }

    fn sync_gains(&mut self) {
        for (i, band) in self.params.bands.iter().enumerate() {
            if let Some((left, right)) = self.active.as_mut() {
                left.set_gain(i, GainDb::from(band.gain_db));
                right.set_gain(i, GainDb::from(band.gain_db));
            }
        }
    }

    fn take_layout(&mut self, layout: &mut MasterEqLayout) {
        let incoming = mem::take(layout);
        if let Some(previous) = self.layout.replace(LayoutUpdate::Pending(incoming)) {
            *layout = previous.into_inner();
        }
    }
}

fn build_equalizers<S: HasPool<f32>>(
    params: &MasterEqNode<S>,
    sample_rate: NonZeroU32,
) -> Result<(IsolatorEq, IsolatorEq), PoolError> {
    let bands = bands_from_params(params);
    let left = IsolatorEq::new(&params.config, &bands, sample_rate.get())?;
    let right = IsolatorEq::new(&params.config, &bands, sample_rate.get())?;
    Ok((left, right))
}

fn bands_from_params<S>(params: &MasterEqNode<S>) -> Vec<EqBandConfig> {
    params
        .bands
        .iter()
        .map(|b| {
            EqBandConfig::builder()
                .frequency(b.frequency)
                .q_factor(b.q_factor)
                .gain_db(GainDb::from(b.gain_db))
                .kind(b.kind.into())
                .build()
        })
        .collect()
}

impl<S> AudioNodeProcessor for MasterEqProcessor<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    #[kithara::rtsan_forbid_blocking]
    fn events(&mut self, _info: &ProcInfo, events: &mut ProcEvents, _extra: &mut ProcExtra) {
        let mut dirty = false;
        for mut event in events.drain() {
            if let Some(layout) = event.downcast_mut::<MasterEqLayout>() {
                self.take_layout(layout);
            } else if let Some(patch) = MasterEqNode::<S>::patch_event(&event) {
                self.apply_patch(patch);
            } else {
                continue;
            }
            dirty = true;
        }
        if dirty {
            self.sync_gains();
        }
    }

    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.sample_rate = stream_info.sample_rate;
        if let Some((left, right)) = self.active.as_mut() {
            left.update_sample_rate(self.sample_rate.get());
            right.update_sample_rate(self.sample_rate.get());
        }
        if let Some((left, right)) = self.retiring.as_mut() {
            left.update_sample_rate(self.sample_rate.get());
            right.update_sample_rate(self.sample_rate.get());
        }
        self.crossover.update_sample_rate(self.sample_rate);
        if let Some(LayoutUpdate::Pending(layout)) = &mut self.layout
            && let Some((left, right)) = &mut layout.equalizers
        {
            left.update_sample_rate(self.sample_rate.get());
            right.update_sample_rate(self.sample_rate.get());
        }
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        /// Minimum stereo channel count for processing.
        const MIN_STEREO: usize = 2;
        self.advance_layout();

        if buffers.inputs.len() < MIN_STEREO || buffers.outputs.len() < MIN_STEREO {
            return ProcessStatus::Bypass;
        }

        if !self.params.enabled
            || self.active.is_none()
            || info.in_silence_mask.all_channels_silent(MIN_STEREO)
        {
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

        let Some((active_l, active_r)) = self.active.as_mut() else {
            return ProcessStatus::Bypass;
        };
        for frame in 0..info.frames {
            let mut left = [active_l.process_sample(in_l[frame])];
            let mut right = [active_r.process_sample(in_r[frame])];
            if !self.crossover.has_settled() {
                let (dry_l, dry_r) = match self.retiring.as_mut() {
                    Some((retiring_l, retiring_r)) => (
                        retiring_l.process_sample(in_l[frame]),
                        retiring_r.process_sample(in_r[frame]),
                    ),
                    None => (in_l[frame], in_r[frame]),
                };
                self.crossover.mix_dry_into_wet_stereo(
                    &[dry_l],
                    &[dry_r],
                    &mut left,
                    &mut right,
                    1,
                );
            }
            out_l[frame] = left[0];
            out_r[frame] = right[0];
        }

        ProcessStatus::OutputsModified
    }
}
