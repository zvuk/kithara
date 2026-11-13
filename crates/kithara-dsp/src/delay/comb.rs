use std::{
    fmt,
    num::NonZeroUsize,
    ops::{Deref, DerefMut, Range},
};

use kithara_bufpool::{HasPool, PoolRegion};

use super::{CombError, DelayError, DelayLine, Tap};
use crate::{
    add, consts, crossfade_linear,
    filter::{self, Biquad, Coefficients, FilterError},
    scale_in_place,
};

/// Feedback comb of `CHANNELS` planes over one [`DelayLine`].
///
/// Each frame reads the history at the delay; that read is the output. The
/// history takes the input plus the read passed once through one biquad
/// section and scaled by the gain, so the section sits in the loop and every
/// repeat passes it once more. The loop is stable while the gain times the
/// section's peak magnitude stays below one; the comb does not check it.
///
/// A delay change crosses linearly from the old read to the new one over the
/// cross length, both read from the one history. A change arriving while a
/// cross runs waits for it to end and starts on the frame after, the latest
/// such change winning.
///
/// The comb has rung out once nothing above -120 dBFS has entered its history
/// for the capacity plus the frames the section's slowest pole takes to decay
/// under that level. Storage is taken once in [`new`](Self::new); nothing
/// after it allocates.
pub struct FeedbackComb<const CHANNELS: usize> {
    line: DelayLine,
    filter: Biquad,
    gain: f32,
    delay: Tap,
    cross: Option<Cross>,
    pending: Option<Tap>,
    cross_frames: NonZeroUsize,
    window: usize,
    quiet: usize,
}

/// A cross from the read at `from` toward the current delay, `done` frames
/// in.
#[derive(Clone, Copy, Debug)]
struct Cross {
    from: Tap,
    done: usize,
}

impl<const CHANNELS: usize> FeedbackComb<CHANNELS> {
    /// A silent comb holding `capacity` frames of history taken from `pools`,
    /// reading `delay` frames back, crossing over `cross` frames when the
    /// delay changes, and feeding back through `section` scaled by `gain`.
    ///
    /// # Errors
    /// [`CombError::Delay`] when `CHANNELS` is zero or `delay` lies outside
    /// `1..=capacity`, [`CombError::Pool`] when the history does not fit
    /// `pools`, [`CombError::Filter`] when `section` is unstable in `f32` or
    /// the platform refuses the filter.
    pub fn new<S: HasPool<f32>>(
        capacity: NonZeroUsize,
        delay: f64,
        cross: NonZeroUsize,
        section: Coefficients<f64>,
        gain: f32,
        pools: &PoolRegion<S>,
    ) -> Result<Self, CombError> {
        let channels = NonZeroUsize::new(CHANNELS).ok_or(DelayError::Shape)?;
        let line = DelayLine::new(channels, capacity, pools)?;
        let delay = line.tap(delay)?;
        let mut comb = Self {
            line,
            filter: Biquad::new(channels, NonZeroUsize::MIN)?,
            gain,
            delay,
            cross: None,
            pending: None,
            cross_frames: cross,
            window: 0,
            quiet: usize::MAX,
        };
        comb.retune(section)?;
        Ok(comb)
    }

    /// A read position `frames` behind the next write of this comb's history.
    ///
    /// # Errors
    /// [`DelayError::OutOfRange`] unless `frames` lies in `1..=capacity`.
    pub fn tap(&self, frames: f64) -> Result<Tap, DelayError> {
        self.line.tap(frames)
    }

    /// Moves the read to `delay`, crossing from the current read, or after
    /// the running cross ends.
    pub fn retime(&mut self, delay: Tap) {
        if self.cross.is_some() {
            self.pending = Some(delay);
        } else if delay != self.delay {
            self.cross = Some(Cross {
                from: self.delay,
                done: 0,
            });
            self.delay = delay;
        }
    }

    /// Scales every later repeat by `gain`.
    pub const fn set_gain(&mut self, gain: f32) {
        self.gain = gain;
    }

    /// Crosses later delay changes over `frames`; a running cross already
    /// that far in ends here.
    pub fn set_cross(&mut self, frames: NonZeroUsize) {
        self.cross_frames = frames;
        self.advance(0);
    }

    /// Retunes the loop section to `section`, its state carrying over, and
    /// sizes the ring-out window by its slowest pole.
    ///
    /// # Errors
    /// [`FilterError::Parameters`] when `section` is unstable in `f32`; the
    /// previous tuning stays then.
    pub fn retune(&mut self, section: Coefficients<f64>) -> Result<(), FilterError> {
        self.filter.retune(0, section)?;
        let decay = filter::decay_frames(section, f64::from(consts::COMB_FLOOR));
        self.window = self.line.capacity().get().saturating_add(decay);
        Ok(())
    }

    /// Runs `range` of `input` through the comb into the same range of
    /// `output`, in spans no longer than either read's delay.
    ///
    /// # Errors
    /// [`DelayError::Shape`] when either plane count differs from `CHANNELS`
    /// or a plane does not cover `range`; nothing runs then.
    pub fn process<I, O>(
        &mut self,
        input: &[I],
        output: &mut [O],
        range: Range<usize>,
    ) -> Result<(), CombError>
    where
        I: Deref<Target = [f32]>,
        O: DerefMut<Target = [f32]>,
    {
        let covers = |plane: &[f32]| plane.get(range.clone()).is_some();
        if input.len() != CHANNELS
            || output.len() != CHANNELS
            || !input.iter().all(|plane| covers(plane))
            || !output.iter().all(|plane| covers(plane))
        {
            return Err(DelayError::Shape.into());
        }
        let mut start = range.start;
        while start < range.end {
            let end = start.saturating_add(self.span(range.end.saturating_sub(start)));
            self.render(input, output, start..end)?;
            self.advance(end.saturating_sub(start));
            start = end;
        }
        Ok(())
    }

    /// Silences the history and the loop section and lands a waiting delay
    /// change at once; the gain and the tuning stay.
    pub fn reset(&mut self) {
        self.line.reset();
        self.filter.reset();
        self.cross = None;
        if let Some(delay) = self.pending.take() {
            self.delay = delay;
        }
        self.quiet = usize::MAX;
    }

    /// Whether nothing above -120 dBFS has entered the history over the
    /// ring-out window; a fresh or reset comb has rung out.
    #[must_use]
    pub const fn rung_out(&self) -> bool {
        self.quiet >= self.window
    }

    /// Frames the next span runs: at most `rest`, the chunk, what each read
    /// allows and what is left of the cross.
    fn span(&self, rest: usize) -> usize {
        let span = rest.min(consts::COMB_CHUNK).min(self.delay.whole());
        self.cross.map_or(span, |cross| {
            span.min(cross.from.whole())
                .min(self.cross_frames.get().saturating_sub(cross.done))
        })
    }

    fn render<I, O>(
        &mut self,
        input: &[I],
        output: &mut [O],
        range: Range<usize>,
    ) -> Result<(), CombError>
    where
        I: Deref<Target = [f32]>,
        O: DerefMut<Target = [f32]>,
    {
        let frames = range.len();
        let mut scratch = [[0.0; consts::COMB_CHUNK]; CHANNELS];
        let mut sends = scratch
            .each_mut()
            .map(|send| send.get_mut(..frames).unwrap_or_default());
        for (channel, (plane, send)) in output.iter_mut().zip(&mut sends).enumerate() {
            let read = plane.get_mut(range.clone()).unwrap_or_default();
            self.line.read(channel, self.delay, read)?;
            if let Some(cross) = self.cross {
                self.line.read(channel, cross.from, send)?;
                crossfade_linear(
                    send,
                    read,
                    NonZeroUsize::MIN,
                    cross.done.saturating_add(1),
                    self.cross_frames.get(),
                );
            }
            send.copy_from_slice(read);
        }
        self.filter.process(&mut sends, 0..frames)?;
        for (send, plane) in sends.iter_mut().zip(input) {
            scale_in_place(send, self.gain);
            add(plane.get(range.clone()).unwrap_or_default(), send);
        }
        self.line.write(&sends, 0..frames)?;
        self.listen(&sends);
        Ok(())
    }

    fn advance(&mut self, frames: usize) {
        let Some(cross) = &mut self.cross else {
            return;
        };
        cross.done = cross.done.saturating_add(frames);
        if cross.done >= self.cross_frames.get() {
            self.cross = None;
            if let Some(delay) = self.pending.take() {
                self.retime(delay);
            }
        }
    }

    /// Counts the frames since the history last took a sample above the
    /// floor, given the span just `written` to it.
    fn listen(&mut self, written: &[&mut [f32]]) {
        let frames = written.first().map_or(0, |plane| plane.len());
        self.quiet = written
            .iter()
            .filter_map(|plane| {
                plane
                    .iter()
                    .rposition(|sample| sample.abs() > consts::COMB_FLOOR)
            })
            .max()
            .map_or_else(
                || self.quiet.saturating_add(frames),
                |frame| frames.saturating_sub(frame).saturating_sub(1),
            );
    }
}

impl<const CHANNELS: usize> fmt::Debug for FeedbackComb<CHANNELS> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FeedbackComb")
            .field("line", &self.line)
            .field("gain", &self.gain)
            .field("delay", &self.delay)
            .field("cross", &self.cross)
            .field("pending", &self.pending)
            .field("cross_frames", &self.cross_frames)
            .field("window", &self.window)
            .field("quiet", &self.quiet)
            .finish_non_exhaustive()
    }
}
