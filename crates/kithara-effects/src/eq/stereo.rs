use core::{fmt, num::NonZeroU32};

use kithara_bufpool::{HasPool, PoolError};
use kithara_dsp::{
    fade::FadeCurve,
    param::{Mix, MixDSP},
};
use num_traits::AsPrimitive;

use super::{EqBandConfig, EqConfig, IsolatorEq};
use crate::{GainDb, consts::EQ_BLOCK_FRAMES};

/// An active gain row must cover every frame of the stereo call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("EQ band {band} has {actual} gain values for {expected} frames")]
pub struct GainShapeError {
    pub band: usize,
    pub expected: usize,
    pub actual: usize,
}

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
    ///
    /// # Panics
    ///
    /// Panics when adjacent band frequencies do not form a finite, positive,
    /// strictly increasing crossover design.
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

    fn set_kill(&mut self, band: usize, on: bool) {
        self.left.set_kill(band, on);
        self.right.set_kill(band, on);
    }

    fn set_explicit(&mut self, band: usize, gain_db: GainDb) {
        self.left.set_explicit(band, gain_db);
        self.right.set_explicit(band, gain_db);
    }

    fn apply_gains(&mut self, gains: &[impl AsRef<[f64]>], frame: usize) {
        for (band, values) in gains.iter().enumerate() {
            if let Some(value) = values.as_ref().get(frame) {
                self.set_explicit(band, GainDb::from(AsPrimitive::<f32>::as_(*value)));
            }
        }
    }

    delegate::delegate! {
        to self.left {
            /// Number of bands prepared in this layout.
            #[must_use]
            pub fn band_count(&self) -> usize;
            fn killed(&self, band: usize) -> Option<bool>;
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
        layout.left.clear_kills();
        layout.right.clear_kills();
        self.update
            .replace(LayoutUpdate::Pending(layout))
            .map(LayoutUpdate::into_inner)
    }

    /// Ramps `band` to `gain_db` on the layout a gain speaks of: the one waiting to be crossed
    /// over to, else the one sounding.
    pub fn set_gain(&mut self, band: usize, gain_db: GainDb) {
        if let Some(layout) = self.target_layout_mut() {
            layout.set_gain(band, gain_db);
        }
    }

    /// Applies a kill change to a band in the command-addressable layout.
    pub fn set_kill(&mut self, band: usize, on: bool) {
        if let Some(layout) = self.target_layout_mut() {
            layout.set_kill(band, on);
        }
    }

    /// Returns every addressed band to neutral gain with kill off.
    pub fn reset_gains(&mut self) {
        if let Some(layout) = self.target_layout_mut() {
            for band in 0..layout.band_count() {
                layout.set_gain(band, GainDb::DEFAULT);
                layout.set_kill(band, false);
            }
        }
    }

    /// Target gain of `band` in the layout that receives commands.
    #[must_use]
    pub fn target_gain(&self, band: usize) -> Option<GainDb> {
        self.target_layout()?.target_gain(band)
    }

    /// Whether kill is engaged for `band` in the same layout as [`Self::target_gain`].
    #[must_use]
    pub fn killed(&self, band: usize) -> Option<bool> {
        self.target_layout()?.killed(band)
    }

    /// The number of bands in the layout that receives commands.
    #[must_use]
    pub fn band_count(&self) -> usize {
        self.target_layout().map_or(0, EqLayout::band_count)
    }

    fn target_layout(&self) -> Option<&EqLayout> {
        match &self.update {
            Some(LayoutUpdate::Pending(layout)) => Some(layout),
            _ => self.active.as_ref(),
        }
        .map(Box::as_ref)
    }

    fn target_layout_mut(&mut self) -> Option<&mut EqLayout> {
        match &mut self.update {
            Some(LayoutUpdate::Pending(layout)) => Some(layout.as_mut()),
            _ => self.active.as_deref_mut(),
        }
    }

    /// Whether a settled sounding layout contains the requested band.
    #[must_use]
    pub fn accepts_gain_program(&self, band: usize) -> bool {
        !matches!(self.update, Some(LayoutUpdate::Pending(_)))
            && self.crossover.has_settled()
            && self
                .active
                .as_ref()
                .is_some_and(|layout| band < layout.band_count())
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
        self.process_gains(left, right, &[] as &[&[f64]]);
    }

    /// Apply one prepared dB row per band; an empty row leaves that band under manual control.
    /// # Errors
    /// Refuses a nonempty row whose length differs from the common channel prefix, before mutation.
    pub fn process_with_gains<G: AsRef<[f64]>>(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        gains: &[G],
    ) -> Result<(), GainShapeError> {
        if validate_gains(gains, left.len().min(right.len()))? {
            self.process_gains(left, right, gains);
        } else {
            self.process(left, right);
        }
        Ok(())
    }

    /// Apply gain values for silent frames without advancing filter or crossover history.
    /// # Errors
    /// Refuses a nonempty row whose length differs from `frames`, before mutation.
    pub fn advance_gains<G: AsRef<[f64]>>(
        &mut self,
        gains: &[G],
        frames: usize,
    ) -> Result<(), GainShapeError> {
        if validate_gains(gains, frames)?
            && let Some(active) = self.active.as_deref_mut()
        {
            for frame in 0..frames {
                active.apply_gains(gains, frame);
            }
        }
        Ok(())
    }

    fn process_gains(&mut self, left: &mut [f32], right: &mut [f32], gains: &[impl AsRef<[f64]>]) {
        self.advance();
        if let Some(active) = self.active.as_deref_mut() {
            render_stereo(
                active,
                self.retiring.as_deref_mut(),
                &mut self.crossover,
                left,
                right,
                gains,
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

fn validate_gains(gains: &[impl AsRef<[f64]>], expected: usize) -> Result<bool, GainShapeError> {
    let mut has_gains = false;
    for (band, values) in gains.iter().enumerate() {
        let actual = values.as_ref().len();
        if actual != 0 && actual != expected {
            return Err(GainShapeError {
                band,
                expected,
                actual,
            });
        }
        has_gains |= actual != 0;
    }
    Ok(has_gains)
}

fn render_stereo(
    active: &mut EqLayout,
    mut retiring: Option<&mut EqLayout>,
    crossover: &mut MixDSP,
    left: &mut [f32],
    right: &mut [f32],
    gains: &[impl AsRef<[f64]>],
) {
    let frames = left.len().min(right.len());
    let left = left.split_at_mut(frames).0;
    let right = right.split_at_mut(frames).0;
    if !gains.is_empty() {
        for (frame, (left, right)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            active.apply_gains(gains, frame);
            render_frame(active, &mut retiring, crossover, left, right);
        }
        return;
    }
    for (left, right) in left
        .chunks_mut(EQ_BLOCK_FRAMES)
        .zip(right.chunks_mut(EQ_BLOCK_FRAMES))
    {
        let wet_left = active.left.process_block(left);
        let wet_right = active.right.process_block(right);
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

fn render_frame(
    active: &mut EqLayout,
    retiring: &mut Option<&mut EqLayout>,
    crossover: &mut MixDSP,
    left: &mut f32,
    right: &mut f32,
) {
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(1)]
    #[case(2)]
    fn eq_kill_reaches_exact_stereo_silence_at_neutral_or_with_a_floor_band(#[case] bands: usize) {
        let config = EqConfig::builder(crate::test_pools::pools()).build();
        let rate = NonZeroU32::new(48_000).expect("rate");
        let mut bands = super::super::generate_log_spaced_bands(bands);
        if let Some(band) = bands.get_mut(1) {
            band.set_gain_db(GainDb::MIN);
        }
        let layout = EqLayout::new(&config, &bands, rate).expect("layout");
        let mut eq = StereoEq::with_layout(&config, rate, Box::new(layout));
        eq.set_kill(0, true);
        for _ in 0..64 {
            let mut left = [0.25; 128];
            let mut right = [-0.5; 128];
            eq.process(&mut left, &mut right);
            assert!(
                left.iter()
                    .chain(&right)
                    .all(|value| *value == 0.0 || value.is_normal())
            );
        }
        let mut left = [0.25; 128];
        let mut right = [-0.5; 128];
        eq.process(&mut left, &mut right);
        assert!(left.iter().chain(&right).all(|value| value.to_bits() == 0));
        eq.set_gain(0, GainDb::MAX);
        left.fill(0.25);
        right.fill(-0.5);
        eq.process(&mut left, &mut right);
        assert!(left.iter().chain(&right).all(|value| value.to_bits() == 0));
        eq.set_kill(0, false);
        assert_eq!(eq.target_gain(0), Some(GainDb::MAX));
    }

    #[kithara::test]
    fn eq_kill_is_cleared_when_a_returned_pending_layout_is_reused() {
        let config = EqConfig::builder(crate::test_pools::pools()).build();
        let rate = NonZeroU32::new(48_000).expect("rate");
        let make_layout = || {
            Box::new(
                EqLayout::new(&config, &[EqBandConfig::builder().build()], rate).expect("layout"),
            )
        };
        let mut eq = StereoEq::with_layout(&config, rate, make_layout());
        eq.take_layout(make_layout());
        eq.set_kill(0, true);
        assert_eq!(eq.killed(0), Some(true));
        let returned = eq
            .take_layout(make_layout())
            .expect("displaced pending layout");
        eq.take_layout(returned);
        assert_eq!(eq.killed(0), Some(false));
    }

    #[kithara::test]
    fn eq_kill_on_a_pending_layout_sounds_once_that_layout_is_active() {
        let config = EqConfig::builder(crate::test_pools::pools()).build();
        let rate = NonZeroU32::new(48_000).expect("rate");
        let make_layout = || {
            Box::new(
                EqLayout::new(&config, &[EqBandConfig::builder().build()], rate).expect("layout"),
            )
        };
        let mut eq = StereoEq::with_layout(&config, rate, make_layout());
        eq.take_layout(make_layout());
        assert!(!eq.accepts_gain_program(0));
        eq.set_kill(0, true);
        assert_eq!(eq.killed(0), Some(true));
        let mut left = [0.25; 128];
        let mut right = [-0.5; 128];
        for _ in 0..512 {
            left.fill(0.25);
            right.fill(-0.5);
            eq.process(&mut left, &mut right);
        }
        assert!(eq.accepts_gain_program(0));
        assert!(left.iter().chain(&right).all(|value| value.to_bits() == 0));
    }

    #[kithara::test]
    fn prepared_gain_rows_refuse_wrong_lengths_before_pcm_or_state_changes() {
        let config = EqConfig::builder(crate::test_pools::pools()).build();
        let rate = NonZeroU32::new(48_000).expect("rate");
        let layout =
            EqLayout::new(&config, &[EqBandConfig::builder().build()], rate).expect("layout");
        let mut eq = StereoEq::with_layout(&config, rate, Box::new(layout));
        for count in [1, 2, 4, 5] {
            let row = vec![-6.0; count];
            let rows = [&[-6.0; 3][..], row.as_slice()];
            let error = Err(GainShapeError {
                band: 1,
                expected: 3,
                actual: count,
            });
            let mut left = [0.25; 3];
            let mut right = [-0.5; 3];
            assert_eq!(eq.process_with_gains(&mut left, &mut right, &rows), error);
            assert_eq!(eq.advance_gains(&rows, 3), error);
            assert_eq!(left, [0.25; 3]);
            assert_eq!(right, [-0.5; 3]);
            assert_eq!(eq.band_count(), 1);
            assert_eq!(eq.target_gain(0), Some(GainDb::DEFAULT));
        }
        let row = [-6.0, -3.0, 0.0];
        let mut left = [0.25; 3];
        let mut right = [-0.5; 3];
        eq.process_with_gains(&mut left, &mut right, &[&row[..]])
            .expect("one value per frame");
        for ((left, right), db) in left.into_iter().zip(right).zip([-6.0_f32, -3.0, 0.0]) {
            let gain = 10.0_f32.powf(db / 20.0);
            assert_eq!(left.to_bits(), (0.25 * gain).to_bits());
            assert_eq!(right.to_bits(), (-0.5 * gain).to_bits());
        }
    }
}
