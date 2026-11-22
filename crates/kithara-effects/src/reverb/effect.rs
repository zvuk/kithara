use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_dsp::{
    add,
    delay::{DelayDiffuser, DelayError, DelayLine, DiffuserError, Tap},
    filter::{FilterError, OnePoleLowPass},
    last_loud, scale_add, scale_in_place,
};
use num_traits::ToPrimitive;

use super::{Damping, Decay, ReverbConfig, Width};
use crate::{DeckEffect, FxParamChange, FxParamDescriptor, FxParamMapping, FxParamUnit, consts};

/// Why a reverb could not be prepared or a buffer shape was refused.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReverbError {
    #[error(transparent)]
    Pool(#[from] PoolError),
    #[error(transparent)]
    Delay(#[from] DelayError),
    #[error(transparent)]
    Diffuser(#[from] DiffuserError),
    #[error(transparent)]
    Filter(#[from] FilterError),
    #[error("reverb section geometry is not representable at {0} Hz")]
    Geometry(NonZeroU32),
    #[error("reverb input and wet output must have the same stereo frame count")]
    Shape,
    #[error("reverb parameter {index} rejects value {value}")]
    Parameter { index: usize, value: f64 },
    #[error("reverb parameter bounds do not form valid native domains")]
    ParameterDomain,
}

#[derive(Debug)]
struct Comb {
    line: DelayLine,
    tap: Tap,
    feedback: f32,
    input_gain: f32,
}

impl Comb {
    fn new<S: HasPool<f32>>(
        rate: NonZeroU32,
        length: NonZeroUsize,
        capacity: NonZeroUsize,
        decay: Decay,
        pools: &PoolRegion<S>,
    ) -> Result<Self, ReverbError> {
        let line = DelayLine::new(NonZeroUsize::MIN, capacity, pools)?;
        let tap = line.tap(length.get().to_f64().ok_or(ReverbError::Geometry(rate))?)?;
        let (feedback, input_gain) = comb_gains(rate, length.get(), decay)?;
        Ok(Self {
            line,
            tap,
            feedback,
            input_gain,
        })
    }

    fn retune(
        &mut self,
        rate: NonZeroU32,
        length: NonZeroUsize,
        decay: Decay,
    ) -> Result<(), ReverbError> {
        let tap = self
            .line
            .tap(length.get().to_f64().ok_or(ReverbError::Geometry(rate))?)?;
        let (feedback, input_gain) = comb_gains(rate, length.get(), decay)?;
        self.tap = tap;
        self.feedback = feedback;
        self.input_gain = input_gain;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
struct Geometry {
    combs: [NonZeroUsize; consts::REVERB_COMBS],
    diffusers: [NonZeroUsize; consts::REVERB_DIFFUSER_LENGTHS.len()],
    shortest: NonZeroUsize,
    longest_comb: NonZeroUsize,
    diffuser_tail: u64,
}

impl Geometry {
    fn new(rate: NonZeroU32, channel: usize) -> Result<Self, ReverbError> {
        let mut combs = [NonZeroUsize::MIN; consts::REVERB_COMBS];
        let mut shortest =
            NonZeroUsize::MIN.saturating_add(consts::REVERB_SCRATCH.saturating_sub(1));
        let mut longest_comb = NonZeroUsize::MIN;
        for (slot, reference) in combs.iter_mut().zip(consts::REVERB_COMB_LENGTHS) {
            let length = frames(rate, reference, channel)?;
            *slot = length;
            shortest = shortest.min(length);
            longest_comb = longest_comb.max(length);
        }
        let rounds = (consts::REVERB_TAIL_THRESHOLD.ln()
            / f64::from(consts::REVERB_DIFFUSION).ln())
        .ceil()
        .to_u64()
        .and_then(|rounds| rounds.checked_add(1))
        .ok_or(ReverbError::Geometry(rate))?;
        let mut diffusers = [NonZeroUsize::MIN; consts::REVERB_DIFFUSER_LENGTHS.len()];
        let mut diffuser_tail = 0_u64;
        for (slot, reference) in diffusers.iter_mut().zip(consts::REVERB_DIFFUSER_LENGTHS) {
            let length = frames(rate, reference, channel)?;
            *slot = length;
            shortest = shortest.min(length);
            let contribution = length
                .get()
                .to_u64()
                .and_then(|length| length.checked_mul(rounds))
                .ok_or(ReverbError::Geometry(rate))?;
            diffuser_tail = diffuser_tail
                .checked_add(contribution)
                .ok_or(ReverbError::Geometry(rate))?;
        }
        Ok(Self {
            combs,
            diffusers,
            shortest,
            longest_comb,
            diffuser_tail,
        })
    }
}

#[derive(Debug)]
struct Bank {
    combs: Vec<Comb>,
    diffusers: Vec<DelayDiffuser<1>>,
    damping: OnePoleLowPass<{ consts::REVERB_COMBS }>,
    shortest: NonZeroUsize,
    longest_comb: NonZeroUsize,
    diffuser_tail: u64,
}

impl Bank {
    fn new<S: HasPool<f32>>(
        rate: NonZeroU32,
        max_rate: NonZeroU32,
        channel: usize,
        config: ReverbConfig,
        pools: &PoolRegion<S>,
    ) -> Result<Self, ReverbError> {
        let geometry = Geometry::new(rate, channel)?;
        let mut combs: Vec<Comb> = Vec::with_capacity(consts::REVERB_COMBS);
        for (length, reference) in geometry.combs.into_iter().zip(consts::REVERB_COMB_LENGTHS) {
            let capacity = frames(max_rate, reference, channel)?;
            combs.push(Comb::new(rate, length, capacity, config.decay(), pools)?);
        }
        let mut diffusers: Vec<DelayDiffuser<1>> =
            Vec::with_capacity(consts::REVERB_DIFFUSER_LENGTHS.len());
        for (length, reference) in geometry
            .diffusers
            .into_iter()
            .zip(consts::REVERB_DIFFUSER_LENGTHS)
        {
            let capacity = frames(max_rate, reference, channel)?;
            let line = DelayLine::new(NonZeroUsize::MIN, capacity, pools)?;
            let mut diffuser = DelayDiffuser::new(line, consts::REVERB_DIFFUSION)?;
            diffuser.retime(length)?;
            diffusers.push(diffuser);
        }
        // The normalized control never reaches a frozen damping pole.
        let coefficient =
            damping_coefficient(config.damping()).ok_or(ReverbError::Geometry(rate))?;
        Ok(Self {
            combs,
            diffusers,
            damping: OnePoleLowPass::new(coefficient)?,
            shortest: geometry.shortest,
            longest_comb: geometry.longest_comb,
            diffuser_tail: geometry.diffuser_tail,
        })
    }

    fn retune(
        &mut self,
        rate: NonZeroU32,
        channel: usize,
        decay: Decay,
    ) -> Result<Geometry, ReverbError> {
        let geometry = Geometry::new(rate, channel)?;
        for (comb, length) in self.combs.iter_mut().zip(geometry.combs) {
            comb.retune(rate, length, decay)?;
        }
        for (diffuser, length) in self.diffusers.iter_mut().zip(geometry.diffusers) {
            diffuser.retime(length)?;
        }
        self.shortest = geometry.shortest;
        self.longest_comb = geometry.longest_comb;
        self.diffuser_tail = geometry.diffuser_tail;
        Ok(geometry)
    }

    fn process_span(&mut self, input: &[f32], output: &mut [f32]) -> Result<(), ReverbError> {
        let len = input.len();
        let mut history = [[0.0_f32; consts::REVERB_SCRATCH]; consts::REVERB_COMBS];
        output.fill(0.0);
        for (comb, history) in self.combs.iter().zip(&mut history) {
            let history = history.get_mut(..len).ok_or(ReverbError::Shape)?;
            comb.line.read(0, comb.tap, history)?;
            add(history, output);
        }
        let mut planes = history
            .each_mut()
            .map(<[f32; consts::REVERB_SCRATCH]>::as_mut_slice);
        self.damping.process(&mut planes, 0..len)?;
        for (comb, history) in self.combs.iter_mut().zip(&mut history) {
            let history = history.get_mut(..len).ok_or(ReverbError::Shape)?;
            scale_in_place(history, comb.feedback);
            scale_add(input, comb.input_gain, history);
            comb.line.write(&[&*history], 0..len)?;
        }
        for diffuser in &mut self.diffusers {
            diffuser.process(&mut [&mut *output], 0..len)?;
        }
        Ok(())
    }

    fn reset(&mut self) {
        for comb in &mut self.combs {
            comb.line.reset();
        }
        for diffuser in &mut self.diffusers {
            diffuser.reset();
        }
        self.damping.reset();
    }
}

/// Prepared stereo wet network of parallel damped combs and serial diffusers.
#[derive(Debug)]
pub struct Reverb {
    banks: [Bank; 2],
    rate: NonZeroU32,
    max_rate: NonZeroU32,
    decay: Decay,
    parameters: [FxParamDescriptor; 3],
    width: f32,
    span: NonZeroUsize,
    tail_bound: u64,
    tail_age: u64,
    is_decay_epoch_pending: bool,
}

impl Reverb {
    /// Prepares section geometry and storage for every rate up to `max_rate` off the audio thread.
    /// Rate-scaled section lengths have a one-frame minimum.
    ///
    /// # Errors
    /// Refuses a maximum below the current rate, unavailable pool storage or a failed
    /// DSP design or native parameter domain; no partially prepared network is returned.
    pub fn new<S: HasPool<f32>>(
        rate: NonZeroU32,
        max_rate: NonZeroU32,
        config: ReverbConfig,
        pools: &PoolRegion<S>,
    ) -> Result<Self, ReverbError> {
        if max_rate < rate {
            return Err(ReverbError::Geometry(max_rate));
        }
        let parameters = parameter_domains()?;
        let left = Bank::new(rate, max_rate, 0, config, pools)?;
        let right = Bank::new(rate, max_rate, 1, config, pools)?;
        let span = left.shortest.min(right.shortest);
        let banks = [left, right];
        let tail_bound = tail_frames(rate, config.decay(), &banks)?;
        Ok(Self {
            banks,
            rate,
            max_rate,
            decay: config.decay(),
            parameters,
            width: f64::from(config.width())
                .to_f32()
                .ok_or(ReverbError::Geometry(rate))?,
            span,
            tail_bound,
            tail_age: tail_bound,
            is_decay_epoch_pending: false,
        })
    }

    /// Prepared native domains in decay, damping and width order.
    #[must_use]
    pub fn parameters(&self) -> &[FxParamDescriptor] {
        &self.parameters
    }

    /// Applies a native parameter from the next frame, retaining section histories.
    ///
    /// # Errors
    /// Refuses undeclared indices, values outside their ranged domains or a
    /// failed coefficient design without changing the running configuration.
    pub fn set(&mut self, change: FxParamChange) -> Result<(), ReverbError> {
        let invalid = || ReverbError::Parameter {
            index: change.index,
            value: change.value,
        };
        match change.index {
            0 => self.set_decay(Decay::checked(change.value).ok_or_else(invalid)?),
            1 => {
                let damping = Damping::checked(change.value).ok_or_else(invalid)?;
                let coefficient = damping_coefficient(damping).ok_or_else(invalid)?;
                for bank in &mut self.banks {
                    bank.damping.retune(coefficient)?;
                }
                Ok(())
            }
            2 => {
                let width = Width::checked(change.value).ok_or_else(invalid)?;
                self.width = f64::from(width).to_f32().ok_or_else(invalid)?;
                Ok(())
            }
            _ => Err(invalid()),
        }
    }

    fn set_decay(&mut self, decay: Decay) -> Result<(), ReverbError> {
        if decay == self.decay {
            return Ok(());
        }
        let mut coefficients = [[(0.0, 0.0); consts::REVERB_COMBS]; 2];
        for (bank, coefficients) in self.banks.iter().zip(&mut coefficients) {
            for (comb, coefficients) in bank.combs.iter().zip(coefficients) {
                *coefficients = comb_gains(self.rate, comb.tap.whole(), decay)?;
            }
        }
        let bound = tail_frames(self.rate, decay, &self.banks)?;
        let active = !self.tail_exhausted();
        for (bank, coefficients) in self.banks.iter_mut().zip(coefficients) {
            for (comb, (feedback, input_gain)) in bank.combs.iter_mut().zip(coefficients) {
                comb.feedback = feedback;
                comb.input_gain = input_gain;
            }
        }
        self.decay = decay;
        self.tail_bound = bound;
        self.tail_age = if active { 0 } else { bound };
        self.is_decay_epoch_pending = active;
        Ok(())
    }

    /// Writes the stereo wet signal, retaining history across arbitrary partitions.
    ///
    /// # Errors
    /// [`ReverbError::Shape`] when frame counts differ; no signal or state is
    /// changed on shape refusal. DSP errors propagate without a bypass path.
    pub fn process(&mut self, input: [&[f32]; 2], wet: [&mut [f32]; 2]) -> Result<(), ReverbError> {
        let [first, second] = input;
        let frames = first.len();
        if input.iter().any(|plane| plane.len() != frames)
            || wet.iter().any(|plane| plane.len() != frames)
        {
            return Err(ReverbError::Shape);
        }
        let [left, right] = wet;
        let mut cursor = 0;
        while cursor < frames {
            let active = !self.tail_exhausted();
            let mut count = self.span.get().min(frames.wrapping_sub(cursor));
            if active {
                let remaining = self
                    .tail_bound
                    .wrapping_sub(self.tail_age)
                    .saturating_add(u64::from(self.is_decay_epoch_pending));
                count = remaining
                    .min(count.to_u64().ok_or(ReverbError::Shape)?)
                    .to_usize()
                    .ok_or(ReverbError::Shape)?;
            }
            let end = cursor.wrapping_add(count);
            let input = [
                first.get(cursor..end).ok_or(ReverbError::Shape)?,
                second.get(cursor..end).ok_or(ReverbError::Shape)?,
            ];
            let output = [
                left.get_mut(cursor..end).ok_or(ReverbError::Shape)?,
                right.get_mut(cursor..end).ok_or(ReverbError::Shape)?,
            ];
            self.process_span(input, output)?;
            cursor = end;
        }
        Ok(())
    }

    fn process_span(
        &mut self,
        input: [&[f32]; 2],
        mut output: [&mut [f32]; 2],
    ) -> Result<(), ReverbError> {
        let [first, _] = input;
        let count = first.len();
        let last = input.iter().filter_map(|plane| last_loud(plane, 0.0)).max();
        if self.tail_exhausted() && last.is_none() {
            for plane in output {
                plane.fill(0.0);
            }
            return Ok(());
        }
        for ((input, output), bank) in input
            .into_iter()
            .zip(output.iter_mut())
            .zip(&mut self.banks)
        {
            bank.process_span(input, output)?;
        }
        mix_width(output, self.width)?;
        // A Decay command's origin is its first processed frame, like an input event.
        let event = last.or_else(|| self.is_decay_epoch_pending.then_some(0));
        self.is_decay_epoch_pending = false;
        let silent = event
            .map_or(count, |frame| count.wrapping_sub(frame).wrapping_sub(1))
            .to_u64()
            .ok_or(ReverbError::Shape)?;
        self.tail_age = if event.is_some() {
            silent
        } else {
            self.tail_age.saturating_add(silent)
        };
        if self.tail_exhausted() {
            self.reset();
        }
        Ok(())
    }

    /// Moves the running network to `sample_rate` in its prepared storage.
    ///
    /// # Errors
    /// [`ReverbError::Geometry`] when `sample_rate` exceeds the prepared maximum.
    /// The running network remains unchanged on refusal.
    pub fn retune(&mut self, sample_rate: NonZeroU32) -> Result<(), ReverbError> {
        if sample_rate > self.max_rate {
            return Err(ReverbError::Geometry(sample_rate));
        }
        if sample_rate == self.rate {
            return Ok(());
        }
        let was_exhausted = self.tail_exhausted();
        let left = self.banks[0].retune(sample_rate, 0, self.decay)?;
        let right = self.banks[1].retune(sample_rate, 1, self.decay)?;
        let elapsed = (u128::from(self.tail_age) * u128::from(sample_rate.get()))
            .div_ceil(u128::from(self.rate.get()));
        self.rate = sample_rate;
        self.span = left.shortest.min(right.shortest);
        self.tail_bound = tail_frames(sample_rate, self.decay, &self.banks)?;
        self.tail_age = if was_exhausted {
            self.tail_bound
        } else {
            u64::try_from(elapsed).unwrap_or(u64::MAX)
        };
        if !was_exhausted && self.tail_exhausted() {
            self.reset();
        }
        Ok(())
    }

    /// Returns every section to silence while preserving its configuration.
    pub fn reset(&mut self) {
        for bank in &mut self.banks {
            bank.reset();
        }
        self.tail_age = self.tail_bound;
        self.is_decay_epoch_pending = false;
    }

    /// Whether the bound since the latest input or active Decay change has elapsed.
    #[must_use]
    pub fn tail_exhausted(&self) -> bool {
        self.tail_age >= self.tail_bound
    }
}

impl DeckEffect for Reverb {
    /// Silences `wet` when the planes disagree in length.
    fn process(&mut self, input: [&[f32]; 2], wet: [&mut [f32]; 2]) {
        let [left, right] = wet;
        if Self::process(self, input, [&mut *left, &mut *right]).is_err() {
            left.fill(0.0);
            right.fill(0.0);
        }
    }

    fn parameters(&self) -> &[FxParamDescriptor] {
        Self::parameters(self)
    }

    /// Keeps the current value when `change` lies outside the declared domains.
    fn set(&mut self, change: FxParamChange) {
        let _ = Self::set(self, change);
    }

    fn reset(&mut self) {
        Self::reset(self);
    }

    fn tail_exhausted(&self) -> bool {
        Self::tail_exhausted(self)
    }

    /// Keeps the current tuning when `sample_rate` exceeds the prepared maximum.
    fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        let _ = self.retune(sample_rate);
    }
}

fn parameter_domains() -> Result<[FxParamDescriptor; 3], ReverbError> {
    let domains = [
        (
            f64::from(Decay::MIN)..=f64::from(Decay::MAX),
            FxParamUnit::Seconds,
            FxParamMapping::Log,
        ),
        (
            f64::from(Damping::MIN)..=f64::from(Damping::MAX),
            FxParamUnit::Ratio,
            FxParamMapping::Linear,
        ),
        (
            f64::from(Width::MIN)..=f64::from(Width::MAX),
            FxParamUnit::Ratio,
            FxParamMapping::Linear,
        ),
    ]
    .map(|(range, unit, mapping)| FxParamDescriptor::checked(range, unit, Some(mapping)));
    let [Some(decay), Some(damping), Some(width)] = domains else {
        return Err(ReverbError::ParameterDomain);
    };
    Ok([decay, damping, width])
}

fn damping_coefficient(damping: Damping) -> Option<f32> {
    (consts::REVERB_DAMPING_SCALE * f64::from(damping)).to_f32()
}

fn comb_gains(rate: NonZeroU32, frames: usize, decay: Decay) -> Result<(f32, f32), ReverbError> {
    let frames = frames.to_f64().ok_or(ReverbError::Geometry(rate))?;
    let feedback = 10.0_f64
        .powf(-3.0 * frames / (f64::from(rate.get()) * f64::from(decay)))
        .to_f32()
        .ok_or(ReverbError::Geometry(rate))?;
    if !(0.0..1.0).contains(&feedback) {
        return Err(FilterError::Parameters.into());
    }
    let input_gain = (1.0 - feedback)
        / consts::REVERB_COMBS
            .to_f32()
            .ok_or(ReverbError::Geometry(rate))?;
    Ok((feedback, input_gain))
}

fn tail_frames(rate: NonZeroU32, decay: Decay, banks: &[Bank; 2]) -> Result<u64, ReverbError> {
    let [left, right] = banks;
    let longest = left
        .longest_comb
        .max(right.longest_comb)
        .get()
        .to_u64()
        .ok_or(ReverbError::Geometry(rate))?;
    let diffusion = left.diffuser_tail.max(right.diffuser_tail);
    (consts::REVERB_TAIL_RT60_MULTIPLIER * f64::from(decay) * f64::from(rate.get()))
        .ceil()
        .to_u64()
        .and_then(|frames| frames.checked_add(longest))
        .and_then(|frames| frames.checked_add(diffusion))
        .ok_or(ReverbError::Geometry(rate))
}

fn frames(rate: NonZeroU32, reference: u32, channel: usize) -> Result<NonZeroUsize, ReverbError> {
    let spread = if channel == 0 {
        0
    } else {
        consts::REVERB_STEREO_SPREAD
    };
    let reference = reference
        .checked_add(spread)
        .ok_or(ReverbError::Geometry(rate))?;
    (f64::from(reference) * f64::from(rate.get()) / consts::REVERB_REFERENCE_RATE)
        .round()
        .max(1.0)
        .to_usize()
        .and_then(NonZeroUsize::new)
        .ok_or(ReverbError::Geometry(rate))
}

fn mix_width(wet: [&mut [f32]; 2], width: f32) -> Result<(), ReverbError> {
    if width == 1.0 {
        return Ok(());
    }
    let [left, right] = wet;
    let mut mid = [0.0_f32; consts::REVERB_SCRATCH];
    let toward_mid = 1.0 - width;
    let mid = mid.get_mut(..left.len()).ok_or(ReverbError::Shape)?;
    mid.copy_from_slice(left);
    add(right, mid);
    scale_in_place(mid, 0.5);
    if width == 0.0 {
        left.copy_from_slice(mid);
        right.copy_from_slice(mid);
        return Ok(());
    }
    scale_in_place(left, width);
    scale_add(mid, toward_mid, left);
    scale_in_place(right, width);
    scale_add(mid, toward_mid, right);
    Ok(())
}
