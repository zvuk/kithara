use core::{fmt, num::NonZeroU32};

use kithara_bufpool::HasPool;
use kithara_dsp::{
    fade::FadeCurve,
    filter::FilterError,
    param::{Mix, MixDSP},
};

use super::{EqBandConfig, EqConfig, IsolatorEq};
use crate::{EffectPrepareError, GainDb, consts::EQ_BLOCK_FRAMES};

/// One band layout built for both channels of a stereo signal. It is built off the audio
/// thread, so taking it in allocates nothing there.
pub struct EqLayout {
    pub(super) left: IsolatorEq,
    right: IsolatorEq,
    prepared_rate: Option<NonZeroU32>,
}

impl EqLayout {
    /// Builds the two channel isolators of `bands` at `sample_rate`.
    ///
    /// # Errors
    ///
    /// Returns [`EffectPrepareError`] for an invalid crossover design or exhausted pool.
    pub fn new<S>(
        config: &EqConfig<S>,
        bands: &[EqBandConfig],
        sample_rate: NonZeroU32,
    ) -> Result<Self, EffectPrepareError>
    where
        S: HasPool<f32>,
    {
        Ok(Self {
            left: IsolatorEq::new(config, bands, sample_rate.get())?,
            right: IsolatorEq::new(config, bands, sample_rate.get())?,
            prepared_rate: None,
        })
    }

    fn set_gain(&mut self, band: usize, gain_db: GainDb) {
        self.left.set_gain(band, gain_db);
        self.right.set_gain(band, gain_db);
    }

    delegate::delegate! {
        to self.left {
            fn band_count(&self) -> usize;
            fn target_gain(&self, band: usize) -> Option<GainDb>;
        }
    }

    /// Stages both channels without changing filter history or gain targets.
    ///
    /// # Errors
    /// Refuses an unsupported design and invalidates previous preparation.
    pub fn prepare_sample_rate(&mut self, sample_rate: NonZeroU32) -> Result<(), FilterError> {
        self.prepared_rate = None;
        self.left.prepare_sample_rate(sample_rate.get())?;
        self.right.prepare_sample_rate(sample_rate.get())?;
        self.prepared_rate = Some(sample_rate);
        Ok(())
    }

    fn update_sample_rate(&mut self, sample_rate: NonZeroU32) -> Result<(), FilterError> {
        if self.prepared_rate != Some(sample_rate) {
            self.prepare_sample_rate(sample_rate)?;
        }
        self.apply_sample_rate(sample_rate);
        Ok(())
    }

    fn apply_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.prepared_rate = None;
        self.left.apply_sample_rate(sample_rate);
        self.right.apply_sample_rate(sample_rate);
    }
}

impl fmt::Debug for EqLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EqLayout")
            .field("bands", &self.left.band_count())
            .finish_non_exhaustive()
    }
}

/// A layout a stereo EQ was handed and does not sound.
enum LayoutUpdate {
    /// Crossed over to once the crossover in progress settles.
    Pending(Box<EqLayout>),
    /// Retired by the last crossover; handed back with the next layout taken.
    Retired(Box<EqLayout>),
}

impl LayoutUpdate {
    fn into_inner(self) -> Box<EqLayout> {
        match self {
            Self::Pending(layout) | Self::Retired(layout) => layout,
        }
    }
}

/// A stereo equaliser whose band layout changes by crossing over from the layout it replaces.
pub struct StereoEq {
    active: Option<Box<EqLayout>>,
    retiring: Option<Box<EqLayout>>,
    update: Option<LayoutUpdate>,
    pub(super) crossover: MixDSP,
    sample_rate: NonZeroU32,
    prepared_rate: Option<NonZeroU32>,
}

impl StereoEq {
    /// A stereo EQ with no layout: it passes the signal through until it takes one.
    #[must_use]
    pub fn new<S>(config: &EqConfig<S>, sample_rate: NonZeroU32) -> Self {
        Self {
            active: None,
            retiring: None,
            update: None,
            crossover: MixDSP::new(
                Mix::FULLY_WET,
                FadeCurve::Linear,
                config.smoothing(),
                sample_rate,
            ),
            sample_rate,
            prepared_rate: None,
        }
    }

    /// A stereo EQ sounding `layout` immediately, without an initial crossover.
    ///
    /// # Errors
    /// Returns the design error and unchanged owning layout if its retune is refused.
    pub fn with_layout<S>(
        config: &EqConfig<S>,
        sample_rate: NonZeroU32,
        mut layout: Box<EqLayout>,
    ) -> Result<Self, (FilterError, Box<EqLayout>)> {
        if let Err(error) = layout.update_sample_rate(sample_rate) {
            return Err((error, layout));
        }
        Ok(Self {
            active: Some(layout),
            ..Self::new(config, sample_rate)
        })
    }

    /// Takes `layout`, retuned to this EQ's rate, to cross over to once the crossover in
    /// progress settles, and hands back the layout it displaces: one still waiting, or the one
    /// the last crossover retired. Nothing it held is freed here.
    ///
    /// # Errors
    /// Returns the design error and owning incoming layout without replacing live state.
    pub fn take_layout(
        &mut self,
        mut layout: Box<EqLayout>,
    ) -> Result<Option<Box<EqLayout>>, (FilterError, Box<EqLayout>)> {
        if let Err(error) = layout.update_sample_rate(self.sample_rate) {
            return Err((error, layout));
        }
        self.prepared_rate = None;
        Ok(self
            .update
            .replace(LayoutUpdate::Pending(layout))
            .map(LayoutUpdate::into_inner))
    }

    /// Ramps `band` to `gain_db` on the layout a gain speaks of: the one waiting to be crossed
    /// over to, else the one sounding.
    pub fn set_gain(&mut self, band: usize, gain_db: GainDb) {
        let layout = match &mut self.update {
            Some(LayoutUpdate::Pending(layout)) => Some(layout),
            _ => self.active.as_mut(),
        };
        if let Some(layout) = layout {
            layout.set_gain(band, gain_db);
        }
    }

    /// Reads target gains into storage supplied by the mixer, without allocation.
    pub fn read_gains(&self, gains: &mut [GainDb]) -> usize {
        let layout = match &self.update {
            Some(LayoutUpdate::Pending(layout)) => Some(layout),
            _ => self.active.as_ref(),
        };
        let Some(layout) = layout else {
            return 0;
        };
        for (band, gain) in gains.iter_mut().enumerate().take(layout.band_count()) {
            if let Some(target) = layout.target_gain(band) {
                *gain = target;
            }
        }
        layout.band_count()
    }

    /// Retunes every layout this EQ holds, and its crossover, to `sample_rate`.
    ///
    /// # Errors
    /// Returns [`FilterError::Parameters`] before changing any active channel or mix state
    /// if any retained layout cannot represent the rate.
    pub fn update_sample_rate(&mut self, sample_rate: NonZeroU32) -> Result<(), FilterError> {
        self.prepare_sample_rate(sample_rate)?;
        let _ = self.apply_prepared_sample_rate();
        Ok(())
    }

    /// Stages both channels of every retained layout without changing sounding state.
    ///
    /// # Errors
    /// Refuses an unsupported design and invalidates any earlier preparation.
    pub fn prepare_sample_rate(&mut self, sample_rate: NonZeroU32) -> Result<(), FilterError> {
        self.prepared_rate = None;
        for layout in self.retained_layouts() {
            layout.prepare_sample_rate(sample_rate)?;
        }
        self.prepared_rate = Some(sample_rate);
        Ok(())
    }

    /// Discards pending coefficients without changing sounding state or layout ownership.
    pub fn discard_prepared_sample_rate(&mut self) {
        self.prepared_rate = None;
    }

    /// Consumes the prepared rate once, with no factory calls or allocation.
    /// Returns `None` without mutation after refusal or when no preparation remains.
    pub fn apply_prepared_sample_rate(&mut self) -> Option<NonZeroU32> {
        let sample_rate = self.prepared_rate.take()?;
        for layout in self.retained_layouts() {
            layout.apply_sample_rate(sample_rate);
        }
        self.sample_rate = sample_rate;
        self.crossover.update_sample_rate(sample_rate);
        Some(sample_rate)
    }

    fn retained_layouts(&mut self) -> impl Iterator<Item = &mut EqLayout> {
        let update = self.update.as_mut().map(|update| match update {
            LayoutUpdate::Pending(layout) | LayoutUpdate::Retired(layout) => layout.as_mut(),
        });
        [
            self.active.as_deref_mut(),
            self.retiring.as_deref_mut(),
            update,
        ]
        .into_iter()
        .flatten()
    }

    /// Runs both channels through the sounding layout in place, crossing over from the layout
    /// it replaced until the crossover settles.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.advance();
        if let Some(active) = self.active.as_deref_mut() {
            render_stereo(
                active,
                self.retiring.as_deref_mut(),
                &mut self.crossover,
                left,
                right,
            );
        }
    }

    /// Crosses over to a waiting layout once the crossover in progress settled.
    fn advance(&mut self) {
        if !self.crossover.has_settled() {
            return;
        }
        match self.update.take() {
            Some(LayoutUpdate::Pending(incoming)) => {
                self.update = self.retiring.take().map(LayoutUpdate::Retired);
                self.retiring = self.active.replace(incoming);
                self.crossover.set_mix(Mix::FULLY_DRY, FadeCurve::Linear);
                self.crossover.reset_to_target();
                self.crossover.set_mix(Mix::FULLY_WET, FadeCurve::Linear);
            }
            update => self.update = update,
        }
    }
}

fn render_stereo(
    active: &mut EqLayout,
    mut retiring: Option<&mut EqLayout>,
    crossover: &mut MixDSP,
    left: &mut [f32],
    right: &mut [f32],
) {
    let frames = left.len().min(right.len());
    for (left, right) in left[..frames]
        .chunks_mut(EQ_BLOCK_FRAMES)
        .zip(right[..frames].chunks_mut(EQ_BLOCK_FRAMES))
    {
        let frames = left.len();
        let mut wet_left = [0.0; EQ_BLOCK_FRAMES];
        let mut wet_right = [0.0; EQ_BLOCK_FRAMES];
        active.left.process(left, &mut wet_left[..frames]);
        active.right.process(right, &mut wet_right[..frames]);
        for ((left, right), (wet_left, wet_right)) in left
            .iter_mut()
            .zip(right.iter_mut())
            .zip(wet_left.iter_mut().zip(wet_right.iter_mut()))
        {
            if !crossover.has_settled() {
                let (from_left, from_right) = match retiring.as_deref_mut() {
                    Some(retiring) => (
                        retiring.left.process_sample(*left),
                        retiring.right.process_sample(*right),
                    ),
                    None => (*left, *right),
                };
                crossover.mix_dry_into_wet_stereo(
                    &[from_left],
                    &[from_right],
                    std::slice::from_mut(wet_left),
                    std::slice::from_mut(wet_right),
                    1,
                );
            }
            *left = *wet_left;
            *right = *wet_right;
        }
    }
}
