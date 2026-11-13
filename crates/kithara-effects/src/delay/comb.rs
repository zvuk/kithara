use std::{
    fmt,
    num::NonZeroUsize,
    ops::{Deref, DerefMut, Range},
};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_dsp::{
    add, crossfade_linear,
    delay::{DelayError, DelayLine, Tap},
    filter::{self, StateVariableFilter, SvfDesign},
    last_loud, scale_in_place,
};

use super::CombError;
use crate::consts;

/// Feedback comb of `CHANNELS` planes over one [`DelayLine`].
///
/// Each read is output; history takes input plus that read passed through one
/// LP/HP state-variable filter and scaled by gain. The caller owns loop stability.
///
/// A time change crosses between reads of one history; a change during a
/// cross waits in one slot, latest wins. Nothing after construction allocates.
///
/// Ring-out measures quiet writes; [`rung_out`](Self::rung_out) states the
/// floor and window.
pub(super) struct FeedbackComb<const CHANNELS: usize> {
    line: DelayLine,
    cross_frames: NonZeroUsize,
    cross: Option<Cross>,
    pending: Option<Tap>,
    filter: StateVariableFilter<CHANNELS>,
    design: SvfDesign,
    delay: Tap,
    gain: f32,
    quiet: usize,
    window: usize,
}

#[derive(Clone, Copy, Debug)]
struct Cross {
    from: Tap,
    done: usize,
}

impl<const CHANNELS: usize> FeedbackComb<CHANNELS> {
    /// A silent comb holding `capacity` frames of history taken from `pools`,
    /// reading `delay` frames back, crossing over `cross` frames when the
    /// delay changes, and feeding back through `design` scaled by `gain`.
    ///
    /// # Errors
    /// [`CombError::Delay`] when `CHANNELS` is zero or `delay` lies outside
    /// `1..=capacity`, [`CombError::Pool`] when the history does not fit
    /// `pools`.
    pub(super) fn new<S: HasPool<f32>>(
        capacity: NonZeroUsize,
        delay: f64,
        cross: NonZeroUsize,
        design: SvfDesign,
        gain: f32,
        pools: &PoolRegion<S>,
    ) -> Result<Self, CombError> {
        let channels = NonZeroUsize::new(CHANNELS).ok_or(DelayError::Shape)?;
        let line = DelayLine::new(channels, capacity, pools)?;
        let delay = line.tap(delay)?;
        let mut comb = Self {
            line,
            gain,
            delay,
            design,
            filter: StateVariableFilter::new(),
            cross: None,
            pending: None,
            cross_frames: cross,
            window: 0,
            quiet: usize::MAX,
        };
        comb.retune(design);
        Ok(comb)
    }

    /// A read position `frames` behind the next write of this comb's history.
    ///
    /// # Errors
    /// [`DelayError::OutOfRange`] unless `frames` lies in `1..=capacity`.
    pub(super) fn tap(&self, frames: f64) -> Result<Tap, DelayError> {
        self.line.tap(frames)
    }

    /// Moves the read to `delay`, crossing from the current read, or after
    /// the running cross ends.
    pub(super) fn retime(&mut self, delay: Tap) {
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
    pub(super) const fn set_gain(&mut self, gain: f32) {
        self.gain = gain;
    }

    /// Crosses later delay changes over `frames`; a running cross already
    /// that far in ends here.
    pub(super) fn set_cross(&mut self, frames: NonZeroUsize) {
        self.cross_frames = frames;
        self.advance(0);
    }

    /// Replaces the prepared LP/HP design without changing filter history.
    pub(super) fn retune(&mut self, design: SvfDesign) {
        self.window = self
            .line
            .capacity()
            .get()
            .saturating_add(filter::decay_frames(design, f64::from(consts::COMB_FLOOR)));
        self.design = design;
    }

    /// Runs `range` of `input` through the comb into the same range of
    /// `output`, in spans no longer than either read's delay.
    ///
    /// # Errors
    /// [`DelayError::Shape`] when either plane count differs from `CHANNELS`
    /// or a plane does not cover `range`; nothing runs then.
    pub(super) fn process<I, O>(
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
    pub(super) fn reset(&mut self) {
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
    pub(super) const fn rung_out(&self) -> bool {
        self.quiet >= self.window
    }

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
        self.filter
            .process_fixed(&mut sends, 0..frames, &self.design)?;
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

    fn listen(&mut self, written: &[&mut [f32]]) {
        let frames = written.first().map_or(0, |plane| plane.len());
        self.quiet = written
            .iter()
            .filter_map(|plane| last_loud(plane, consts::COMB_FLOOR))
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
