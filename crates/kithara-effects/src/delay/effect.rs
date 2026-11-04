use std::{f64::consts::FRAC_1_SQRT_2, fmt, num::NonZeroUsize};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_dsp::{
    delay::{DelayError, DelayLine, Tap},
    filter::{Biquad, Coefficients, FilterError, Hertz, Type},
};
use num_traits::ToPrimitive;

use super::{DelayParam, DelayTime, FeedbackDelayConfig, FeedbackDelayError, FeedbackHighPass};
use crate::{DeckEffect, FxParamChange, consts};

/// Stereo feedback delay a deck FX unit holds.
///
/// Its wet signal is the history read at the delay time. The history takes
/// the input plus that read, high-passed once and scaled by the feedback:
/// the first echo is the input unfiltered and every repeat passes the
/// high-pass once more, so the filter sits in the loop, not on the input. A
/// time change crosses linearly from the old read to the new one over 20 ms
/// of the one history; a change arriving while a cross runs waits for it to
/// end, the latest such change winning. The tail counts as spent once the
/// input has stayed silent for the capacity plus one delay per pass the
/// feedback takes to fall under -120 dBFS.
pub struct FeedbackDelay {
    line: DelayLine,
    filter: Biquad,
    sample_rate: f64,
    feedback: f32,
    time: Read,
    ramp: Option<Ramp>,
    pending: Option<Read>,
    ramp_frames: usize,
    tail: usize,
    quiet: usize,
}

/// A read position and the oldest frame it touches.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Read {
    tap: Tap,
    reach: usize,
}

/// A cross-read from `from` toward the current time, `done` frames in.
#[derive(Clone, Copy, Debug)]
struct Ramp {
    from: Read,
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
        let time = read(&line, config.time())?;
        let sample_rate = f64::from(config.sample_rate().get());
        let mut filter = Biquad::new(consts::DELAY_CHANNELS, NonZeroUsize::MIN)?;
        filter.retune(0, high_pass(sample_rate, config.high_pass())?)?;
        let ramp_frames = (sample_rate * consts::DELAY_RAMP_SECONDS)
            .round()
            .to_usize()
            .map_or(1, |frames| frames.max(1));
        let mut delay = Self {
            line,
            filter,
            sample_rate,
            feedback: f32::from(config.feedback()),
            time,
            ramp: None,
            pending: None,
            ramp_frames,
            tail: 0,
            quiet: usize::MAX,
        };
        delay.tail = delay.tail_bound();
        Ok(delay)
    }

    /// Frames the next span renders: at most `rest`, the chunk, and what each
    /// read and the cross allow.
    fn span(&self, rest: usize) -> usize {
        let span = rest.min(consts::DELAY_CHUNK).min(self.time.tap.whole());
        self.ramp.map_or(span, |ramp| {
            span.min(ramp.from.tap.whole())
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
            self.line.read(channel, self.time.tap, wet)?;
            if let Some(ramp) = self.ramp {
                self.line.read(channel, ramp.from.tap, send)?;
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

    fn retime(&mut self, time: Read) {
        if self.ramp.is_some() {
            self.pending = Some(time);
        } else if time != self.time {
            self.ramp = Some(Ramp {
                from: self.time,
                done: 0,
            });
            self.time = time;
        }
        self.tail = self.tail_bound();
    }

    fn listen(&mut self, [left, right]: [&[f32]; 2]) {
        self.quiet = left
            .iter()
            .zip(right)
            .rposition(|(left, right)| *left != 0.0 || *right != 0.0)
            .map_or_else(
                || self.quiet.saturating_add(left.len()),
                |frame| left.len().saturating_sub(frame).saturating_sub(1),
            );
    }

    /// Frames of silent input after which the tail is spent: the capacity,
    /// then the longest read once per pass the feedback takes to fall under
    /// the floor.
    fn tail_bound(&self) -> usize {
        let reach = [self.ramp.map(|ramp| ramp.from), self.pending]
            .into_iter()
            .flatten()
            .fold(self.time.reach, |reach, read| reach.max(read.reach));
        let passes = (consts::DELAY_TAIL_FLOOR.ln() / f64::from(self.feedback).ln())
            .ceil()
            .to_usize()
            .unwrap_or(0);
        self.line
            .capacity()
            .get()
            .saturating_add(passes.saturating_mul(reach))
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
            self.listen(input);
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
                if let Ok(time) = read(&self.line, time) {
                    self.retime(time);
                }
            }
            DelayParam::Feedback(feedback) => {
                self.feedback = f32::from(feedback);
                self.tail = self.tail_bound();
            }
            DelayParam::HighPass(cutoff) => {
                high_pass(self.sample_rate, cutoff)
                    .and_then(|coefficients| self.filter.retune(0, coefficients))
                    .ok();
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
        self.tail = self.tail_bound();
        self.quiet = usize::MAX;
    }

    fn tail_exhausted(&self) -> bool {
        self.quiet >= self.tail
    }
}

impl fmt::Debug for FeedbackDelay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FeedbackDelay")
            .field("line", &self.line)
            .field("feedback", &self.feedback)
            .field("time", &self.time)
            .field("ramp", &self.ramp)
            .field("pending", &self.pending)
            .field("tail", &self.tail)
            .field("quiet", &self.quiet)
            .finish_non_exhaustive()
    }
}

/// `time` on `line`, with the oldest frame it touches.
fn read(line: &DelayLine, time: DelayTime) -> Result<Read, DelayError> {
    let frames = f64::from(time);
    let tap = line.tap(frames)?;
    Ok(Read {
        tap,
        reach: tap
            .whole()
            .saturating_add(usize::from(frames.fract() > 0.0)),
    })
}

/// The Butterworth high-pass at `cutoff` for `sample_rate`.
fn high_pass(sample_rate: f64, cutoff: FeedbackHighPass) -> Result<Coefficients<f64>, FilterError> {
    let hertz = |value: f64| Hertz::<f64>::from_hz(value).map_err(|_| FilterError::Parameters);
    Coefficients::from_params(
        Type::HighPass,
        hertz(sample_rate)?,
        hertz(f64::from(f32::from(cutoff)))?,
        FRAC_1_SQRT_2,
    )
    .map_err(|_| FilterError::Parameters)
}

fn to_f32(frames: usize) -> f32 {
    frames.to_f32().unwrap_or(0.0)
}
