use core::{fmt, num::NonZeroU32};

use kithara_bufpool::{HasPool, PoolError};
use kithara_dsp::{
    fade::FadeCurve,
    param::{Mix, MixDSP},
};

use super::{EqBandConfig, EqConfig, IsolatorEq};
use crate::GainDb;

/// One band layout built for both channels of a stereo signal. It is built off the audio
/// thread, so taking it in allocates nothing there.
pub struct EqLayout {
    pub(super) left: IsolatorEq,
    right: IsolatorEq,
}

impl EqLayout {
    /// Builds the two channel isolators of `bands` at `sample_rate`.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError`] when the region cannot hand out the crossover filter states and
    /// the gain banks the two isolators run on.
    pub fn new<S>(
        config: &EqConfig<S>,
        bands: &[EqBandConfig],
        sample_rate: NonZeroU32,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        Ok(Self {
            left: IsolatorEq::new(config, bands, sample_rate.get())?,
            right: IsolatorEq::new(config, bands, sample_rate.get())?,
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

    fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.left.update_sample_rate(sample_rate.get());
        self.right.update_sample_rate(sample_rate.get());
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
        }
    }

    /// A stereo EQ sounding `layout` immediately, without an initial crossover.
    #[must_use]
    pub fn with_layout<S>(
        config: &EqConfig<S>,
        sample_rate: NonZeroU32,
        mut layout: Box<EqLayout>,
    ) -> Self {
        layout.update_sample_rate(sample_rate);
        Self {
            active: Some(layout),
            ..Self::new(config, sample_rate)
        }
    }

    /// Takes `layout`, retuned to this EQ's rate, to cross over to once the crossover in
    /// progress settles, and hands back the layout it displaces: one still waiting, or the one
    /// the last crossover retired. Nothing it held is freed here.
    pub fn take_layout(&mut self, mut layout: Box<EqLayout>) -> Option<Box<EqLayout>> {
        layout.update_sample_rate(self.sample_rate);
        self.update
            .replace(LayoutUpdate::Pending(layout))
            .map(LayoutUpdate::into_inner)
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
    pub fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.sample_rate = sample_rate;
        self.crossover.update_sample_rate(sample_rate);
        let pending = match &mut self.update {
            Some(LayoutUpdate::Pending(layout)) => Some(layout),
            _ => None,
        };
        for layout in [self.active.as_mut(), self.retiring.as_mut(), pending]
            .into_iter()
            .flatten()
        {
            layout.update_sample_rate(sample_rate);
        }
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
    for (left, right) in left.iter_mut().zip(right.iter_mut()) {
        let (dry_left, dry_right) = (*left, *right);
        let mut wet_left = [active.left.process_sample(dry_left)];
        let mut wet_right = [active.right.process_sample(dry_right)];
        if !crossover.has_settled() {
            let (from_left, from_right) = match retiring.as_deref_mut() {
                Some(retiring) => (
                    retiring.left.process_sample(dry_left),
                    retiring.right.process_sample(dry_right),
                ),
                None => (dry_left, dry_right),
            };
            crossover.mix_dry_into_wet_stereo(
                &[from_left],
                &[from_right],
                &mut wet_left,
                &mut wet_right,
                1,
            );
        }
        *left = wet_left[0];
        *right = wet_right[0];
    }
}
