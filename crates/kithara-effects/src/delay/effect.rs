use std::{
    f64::consts::FRAC_1_SQRT_2,
    fmt,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_dsp::{
    delay::{DelayLine, Tap},
    filter::{Biquad, Coefficients, FilterError, Hertz, Type},
};
use num_traits::ToPrimitive;

use super::{DelayParam, FeedbackDelayConfig, FeedbackDelayError, FeedbackHighPass};
use crate::{DeckEffect, FxParamChange, consts};

/// Stereo feedback delay a deck FX unit holds.
///
/// Its wet signal is the history read at the delay time. The history takes
/// the input plus that read, high-passed once and scaled by the feedback:
/// the first echo is the input unfiltered and every repeat passes the
/// high-pass once more, so the filter sits in the loop, not on the input. A
/// time change crosses linearly from the old read to the new one over 20 ms
/// of the one history; a change arriving while a cross runs waits for it to
/// end and starts on the frame after, the latest such change winning. The
/// tail counts as spent once nothing above -120 dBFS has entered the history
/// for the capacity plus the frames the high-pass's slowest pole takes to
/// decay under that level. The delay time and the capacity count frames, so a
/// new sample rate keeps them and retunes the high-pass, the cross length and
/// that decay.
pub struct FeedbackDelay {
    line: DelayLine,
    filter: Biquad,
    sample_rate: NonZeroU32,
    high_pass: FeedbackHighPass,
    feedback: f32,
    time: Tap,
    ramp: Option<Ramp>,
    pending: Option<Tap>,
    ramp_frames: usize,
    window: usize,
    quiet: usize,
}

/// A cross-read from `from` toward the current time, `done` frames in.
#[derive(Clone, Copy, Debug)]
struct Ramp {
    from: Tap,
    done: usize,
}

impl FeedbackDelay {
    /// Prepares the history and the feedback filter `config` asks for, the
    /// history taken from `pools`.
    ///
    /// # Errors
    /// [`FeedbackDelayError::Delay`] when the starting time exceeds the
    /// capacity, [`FeedbackDelayError::Pool`] when the history does not fit
    /// `pools`, [`FeedbackDelayError::Filter`] when the starting cutoff does
    /// not fit under Nyquist or the platform refuses the filter.
    pub fn new<S: HasPool<f32>>(
        config: FeedbackDelayConfig,
        pools: &PoolRegion<S>,
    ) -> Result<Self, FeedbackDelayError> {
        let line = DelayLine::new(consts::DELAY_CHANNELS, config.capacity(), pools)?;
        let time = line.tap(f64::from(config.time()))?;
        let sample_rate = config.sample_rate();
        let mut delay = Self {
            line,
            filter: Biquad::new(consts::DELAY_CHANNELS, NonZeroUsize::MIN)?,
            sample_rate,
            high_pass: config.high_pass(),
            feedback: f32::from(config.feedback()),
            time,
            ramp: None,
            pending: None,
            ramp_frames: ramp_frames(sample_rate),
            window: 0,
            quiet: usize::MAX,
        };
        delay.tune(config.high_pass())?;
        Ok(delay)
    }

    /// Frames the next span renders: at most `rest`, the chunk, and what each
    /// read and the cross allow.
    fn span(&self, rest: usize) -> usize {
        let span = rest.min(consts::DELAY_CHUNK).min(self.time.whole());
        self.ramp.map_or(span, |ramp| {
            span.min(ramp.from.whole())
                .min(self.ramp_frames.saturating_sub(ramp.done))
        })
    }

    fn render(
        &mut self,
        input: [&[f32]; 2],
        wet: [&mut [f32]; 2],
    ) -> Result<(), FeedbackDelayError> {
        let frames = input[0].len();
        let mut shares = [0.0; consts::DELAY_CHUNK];
        let shares = &mut shares[..frames];
        if let Some(ramp) = self.ramp {
            kithara_dsp::ramp(shares, to_f32(ramp.done.saturating_add(1)), 1.0);
            kithara_dsp::div(shares, to_f32(self.ramp_frames));
        }
        let mut send = [[0.0; consts::DELAY_CHUNK], [0.0; consts::DELAY_CHUNK]];
        for (channel, (wet, send)) in wet.into_iter().zip(&mut send).enumerate() {
            let send = &mut send[..frames];
            self.line.read(channel, self.time, wet)?;
            if let Some(ramp) = self.ramp {
                self.line.read(channel, ramp.from, send)?;
                kithara_dsp::sub(wet, send);
                kithara_dsp::mul_add(wet, shares, send);
                wet.copy_from_slice(send);
            } else {
                send.copy_from_slice(wet);
            }
        }
        let [left, right] = &mut send;
        let mut planes = [&mut left[..frames], &mut right[..frames]];
        self.filter.process(&mut planes, 0..frames)?;
        for (plane, input) in planes.iter_mut().zip(input) {
            kithara_dsp::scale_in_place(plane, self.feedback);
            kithara_dsp::add(input, plane);
        }
        self.line.write(&planes, 0..frames)?;
        self.listen(&planes);
        Ok(())
    }

    fn advance(&mut self, frames: usize) {
        let Some(ramp) = &mut self.ramp else {
            return;
        };
        ramp.done = ramp.done.saturating_add(frames);
        if ramp.done >= self.ramp_frames {
            self.ramp = None;
            if let Some(time) = self.pending.take() {
                self.retime(time);
            }
        }
    }

    fn retime(&mut self, time: Tap) {
        if self.ramp.is_some() {
            self.pending = Some(time);
        } else if time != self.time {
            self.ramp = Some(Ramp {
                from: self.time,
                done: 0,
            });
            self.time = time;
        }
    }

    /// Counts the frames since the history last took a sample above the
    /// tail floor, given the span just `written` to it.
    fn listen(&mut self, written: &[&mut [f32]; 2]) {
        let [left, right] = written;
        let frames = left.len();
        self.quiet = left
            .iter()
            .zip(right.iter())
            .rposition(|(left, right)| left.abs().max(right.abs()) > consts::DELAY_TAIL_FLOOR)
            .map_or_else(
                || self.quiet.saturating_add(frames),
                |frame| frames.saturating_sub(frame).saturating_sub(1),
            );
    }

    /// Tunes the feedback high-pass to `cutoff` at the current sample rate and
    /// sizes the tail window by its slowest pole.
    fn tune(&mut self, cutoff: FeedbackHighPass) -> Result<(), FilterError> {
        let coefficients = high_pass(self.sample_rate, cutoff)?;
        let decay = decay(&coefficients);
        self.filter.retune(0, coefficients)?;
        self.high_pass = cutoff;
        self.window = self.line.capacity().get().saturating_add(decay);
        Ok(())
    }
}

impl DeckEffect for FeedbackDelay {
    fn process(&mut self, [left, right]: [&[f32]; 2], [wet_left, wet_right]: [&mut [f32]; 2]) {
        let frames = left
            .len()
            .min(right.len())
            .min(wet_left.len())
            .min(wet_right.len());
        let mut start = 0;
        while start < frames {
            let end = start.saturating_add(self.span(frames - start));
            let input = [&left[start..end], &right[start..end]];
            let wet = [&mut wet_left[start..end], &mut wet_right[start..end]];
            if self.render(input, wet).is_err() {
                wet_left[start..end].fill(0.0);
                wet_right[start..end].fill(0.0);
            }
            self.advance(end - start);
            start = end;
        }
    }

    fn accepts(&self, change: &FxParamChange) -> bool {
        let FxParamChange::Delay(param) = change;
        match param {
            DelayParam::Time(time) => self.line.tap(f64::from(*time)).is_ok(),
            DelayParam::Feedback(_) => true,
            DelayParam::HighPass(cutoff) => high_pass(self.sample_rate, *cutoff).is_ok(),
        }
    }

    fn set(&mut self, change: FxParamChange) {
        let FxParamChange::Delay(param) = change;
        match param {
            DelayParam::Time(time) => {
                if let Ok(time) = self.line.tap(f64::from(time)) {
                    self.retime(time);
                }
            }
            DelayParam::Feedback(feedback) => self.feedback = f32::from(feedback),
            DelayParam::HighPass(cutoff) => {
                self.tune(cutoff).ok();
            }
        }
    }

    /// Silences the history and the filter and lands any time change; the
    /// parameters set so far stay.
    fn reset(&mut self) {
        self.line.reset();
        self.filter.settle(&[0.0, 0.0]).ok();
        self.ramp = None;
        if let Some(time) = self.pending.take() {
            self.time = time;
        }
        self.quiet = usize::MAX;
    }

    fn tail_exhausted(&self) -> bool {
        self.quiet >= self.window
    }

    /// Keeps the history, the delay time in frames and the feedback, and
    /// retunes the high-pass, the cross length and the tail window to
    /// `sample_rate`; a cutoff at or past its Nyquist keeps the previous
    /// tuning. A cross already past its new length ends here.
    fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        if sample_rate == self.sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        self.ramp_frames = ramp_frames(sample_rate);
        self.tune(self.high_pass).ok();
        self.advance(0);
    }
}

impl fmt::Debug for FeedbackDelay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FeedbackDelay")
            .field("line", &self.line)
            .field("sample_rate", &self.sample_rate)
            .field("high_pass", &self.high_pass)
            .field("feedback", &self.feedback)
            .field("time", &self.time)
            .field("ramp", &self.ramp)
            .field("pending", &self.pending)
            .field("window", &self.window)
            .field("quiet", &self.quiet)
            .finish_non_exhaustive()
    }
}

/// The 20 ms a time change crosses over, in frames at `sample_rate`; at
/// least one.
fn ramp_frames(sample_rate: NonZeroU32) -> usize {
    (f64::from(sample_rate.get()) * consts::DELAY_RAMP_SECONDS)
        .round()
        .to_usize()
        .map_or(1, |frames| frames.max(1))
}

/// The Butterworth high-pass at `cutoff` for `sample_rate`.
fn high_pass(
    sample_rate: NonZeroU32,
    cutoff: FeedbackHighPass,
) -> Result<Coefficients<f64>, FilterError> {
    let hertz = |value: f64| Hertz::<f64>::from_hz(value).map_err(|_| FilterError::Parameters);
    Coefficients::from_params(
        Type::HighPass,
        hertz(f64::from(sample_rate.get()))?,
        hertz(f64::from(f32::from(cutoff)))?,
        FRAC_1_SQRT_2,
    )
    .map_err(|_| FilterError::Parameters)
}

/// Frames the slowest pole of the Butterworth high-pass `coefficients` takes
/// to decay under the tail floor. Its two poles are a conjugate pair, both of
/// radius `√a2`.
fn decay(coefficients: &Coefficients<f64>) -> usize {
    (f64::from(consts::DELAY_TAIL_FLOOR).ln() / coefficients.a2.sqrt().ln())
        .ceil()
        .to_usize()
        .unwrap_or(0)
}

fn to_f32(frames: usize) -> f32 {
    frames.to_f32().unwrap_or(0.0)
}
