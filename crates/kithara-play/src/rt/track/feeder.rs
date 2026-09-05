use std::{collections::VecDeque, num::NonZeroU32, ops::Range};

use kithara_audio::{SourceEnd, SourceSpan};
use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_platform::{maybe_send::WasmSend, sync::Arc};
use kithara_signal::FrameCount;
use kithara_test_macros as kithara;
use kithara_warp::{PresentationFrontier, RenderContext, RenderReader};

#[rustfmt::skip]
use crate::resource::Resource;
use crate::{bridge::RtMetrics, worker::ServiceClass};

/// RT-safe resource wrapper with internal scratch buffers.
///
/// Wraps a [`Resource`] and maintains per-channel scratch buffers
/// that are filled from the underlying `AudioReader`. The audio thread
/// reads from these buffers, avoiding direct interaction with the
/// potentially-blocking decoder on every callback.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct PlayerResource {
    #[field(get, deref = false)]
    src: Arc<str>,
    resource: WasmSend<Resource>,
    channel_buffers: [SampleBuffer; Self::STEREO_CHANNELS],
    eof_seen: bool,
    failed: bool,
    write_len: usize,
    write_pos: usize,
    source_spans: VecDeque<SourceWindow>,
    last_source_end: Option<SourceEnd>,
}

#[derive(Clone, Copy)]
struct SourceWindow {
    frames: usize,
    source: Option<SourceSpan>,
}

impl SourceWindow {
    fn source_for(&self, frames: usize) -> Option<SourceSpan> {
        let source = self.source?;
        let end = partial_source_end(source, frames.min(self.frames), self.frames)?;
        SourceSpan::new(source.start(), end, source.sample_rate())
            .map(|span| span.with_render_revision(source.render_revision()))
    }

    fn take(&mut self, frames: usize) -> Option<SourceSpan> {
        let consumed = frames.min(self.frames);
        let taken = self.source_for(consumed);
        if let (Some(source), Some(taken)) = (self.source, taken) {
            self.source = SourceSpan::new(taken.end(), source.end(), source.sample_rate())
                .filter(|remaining| remaining.start() < remaining.end())
                .map(|remaining| remaining.with_render_revision(source.render_revision()));
        }
        self.frames -= consumed;
        taken
    }
}

fn partial_source_end(source: SourceSpan, frames: usize, span_frames: usize) -> Option<u64> {
    let source_frames = source.end().checked_sub(source.start())?;
    let numerator = u128::from(source_frames).checked_mul(u128::try_from(frames).ok()?)?;
    let denominator = u128::try_from(span_frames).ok()?;
    let consumed = u64::try_from(numerator.checked_div(denominator)?).ok()?;
    source.start().checked_add(consumed)
}

/// Result of a bounded audio-thread read from [`PlayerResource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// The requested range was filled completely.
    ///
    /// `frames` counts real audio frames copied out of the wrapped reader or
    /// scratch buffer. The remainder may be zero-filled during a non-terminal
    /// underrun and must not advance playback position.
    Full { frames: usize },
    /// A strict prefix of the requested range was written.
    ///
    /// The payload is the number of written frames. This outcome is reserved
    /// for natural EOF inside the requested block; the next read must return
    /// [`ReadOutcome::Eof`].
    Partial { frames: usize },
    /// The resource was already drained and nothing was written.
    Eof,
    /// The underlying decoder/source reported a non-recoverable error
    /// mid-stream. Distinct from [`Eof`](Self::Eof): the track did NOT
    /// reach its natural end — surface this as a track-failed signal
    /// upstream instead of letting the queue auto-advance as if the
    /// track played out.
    Failed,
}

impl PlayerResource {
    /// Buffer duration divisor: `sample_rate` / `BUFFER_DURATION_DIVISOR` gives ~200ms of frames.
    const BUFFER_DURATION_DIVISOR: usize = 5;

    /// Number of stereo output channels.
    const STEREO_CHANNELS: usize = 2;

    /// Create a new `PlayerResource` wrapping the given resource.
    ///
    /// Allocates two per-channel scratch buffers through the given pool facade,
    /// each holding [`Self::scratch_frames`] frames.
    pub fn new<S>(
        resource: Resource,
        src: Arc<str>,
        pools: &PoolRegion<S>,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let buffer_frames = Self::scratch_frames(resource.spec().sample_rate.get()).get();
        let left = pools.get_with_len::<f32>(buffer_frames)?;
        let right = pools.get_with_len::<f32>(buffer_frames)?;

        Ok(Self {
            channel_buffers: [left, right],
            source_spans: VecDeque::with_capacity(buffer_frames),
            src,
            resource: WasmSend::new(resource),
            write_len: 0,
            write_pos: 0,
            last_source_end: None,
            eof_seen: false,
            failed: false,
        })
    }

    pub(crate) fn apply_playback_rate(&self, rate: f32) -> f32 {
        self.resource.get().apply_playback_rate(rate)
    }

    /// Cached span in seconds: how much of the source is on disk and needs no
    /// further network.
    #[must_use]
    pub fn cached_span(&self) -> f64 {
        self.resource.get().cached_span().as_secs_f64()
    }

    /// Decoded-ahead frontier in seconds: how much content has been decoded
    /// and is ready to play (always `>=` the served playback position).
    #[must_use]
    pub fn decoded_frontier(&self) -> f64 {
        self.resource.get().decoded_frontier().as_secs_f64()
    }

    fn fill_scratch(&mut self, target_frames: usize, metrics: &RtMetrics) -> bool {
        let mut eof_reached = self.eof_seen;

        while target_frames > self.write_len && !eof_reached {
            let avail = self.channel_buffers[0].len() - self.write_pos;
            if avail == 0 {
                break;
            }

            let channel_buffers = &mut self.channel_buffers;
            let (left_buf, right_buf) = channel_buffers.split_at_mut(1);
            let left = &mut left_buf[0][self.write_pos..self.write_pos + avail];
            let right = &mut right_buf[0][self.write_pos..self.write_pos + avail];
            let mut planar: [&mut [f32]; Self::STEREO_CHANNELS] = [left, right];

            let (n, source) = match self.resource.get_mut().read_planar(&mut planar) {
                Ok(kithara_audio::ReadOutcome::Frames {
                    count, source_span, ..
                }) => (count.get(), source_span),
                Ok(kithara_audio::ReadOutcome::Pending { .. }) => (0, None),
                Ok(kithara_audio::ReadOutcome::Eof { .. }) => {
                    self.eof_seen = true;
                    eof_reached = true;
                    (0, None)
                }
                Err(_) => {
                    metrics.record_decode_error();
                    self.failed = true;
                    (0, None)
                }
            };
            if n == 0 {
                break;
            }
            self.source_spans
                .push_back(SourceWindow { frames: n, source });
            self.write_len += n;
            self.write_pos += n;
        }

        eof_reached
    }

    /// Remaining buffered frames when the wrapped reader has reached EOF.
    ///
    /// `Some(0)` means the current read drained the last buffered frame exactly;
    /// the next read will return [`ReadOutcome::Eof`].
    #[must_use]
    pub fn frames_until_eof(&self) -> Option<usize> {
        self.eof_seen.then_some(self.write_len)
    }

    pub(crate) fn playback_rate(&self) -> f32 {
        self.resource.get().playback_rate()
    }

    fn prefetch_target(&self, callback_frames: usize) -> usize {
        self.write_len
            .saturating_add(callback_frames)
            .min(self.channel_buffers[0].len())
    }

    #[kithara::probe(
        render_revision = source.render_revision(),
        session_epoch = u64::from(context.session_epoch()),
        output_start = i64::from(context.output_frames().start)
            .saturating_add(i64::try_from(output.start).unwrap_or(i64::MAX)),
        output_end = i64::from(context.output_frames().start)
            .saturating_add(i64::try_from(output.end).unwrap_or(i64::MAX)),
        source_start = source.start(),
        source_end = source.end()
    )]
    fn pcm_consumed(&mut self, context: &RenderContext, output: Range<usize>, source: SourceSpan) {
        self.last_source_end = Some(SourceEnd::new(source.end(), source.sample_rate()));
    }

    fn consume_source(&mut self, mut frames: usize, context: Option<&RenderContext>) {
        let mut output_start = 0usize;
        while frames > 0 {
            let Some(mut span) = self.source_spans.pop_front() else {
                break;
            };
            let consumed = frames.min(span.frames);
            let output_end = output_start.saturating_add(consumed);
            match (context, span.take(consumed)) {
                (Some(context), Some(source)) => {
                    self.pcm_consumed(context, output_start..output_end, source);
                }
                (_, source) => {
                    self.last_source_end =
                        source.map(|source| SourceEnd::new(source.end(), source.sample_rate()));
                }
            }
            frames -= consumed;
            output_start = output_end;
            if span.frames > 0 {
                self.source_spans.push_front(span);
            }
        }
        if frames > 0 {
            self.last_source_end = None;
        }
    }

    pub(crate) fn presentation_source_end(&self, sample_rate: NonZeroU32) -> Option<SourceEnd> {
        let source_end = self.last_source_end?;
        (source_end.sample_rate() == sample_rate
            && source_end.sample_rate() == self.resource.get().spec().sample_rate)
            .then_some(source_end)
    }

    /// Read audio frames into the output buffers for the given range.
    ///
    /// Fills internal scratch buffers from the underlying resource as needed,
    /// then copies the requested frames into `output`. Shifts any remaining
    /// data to the front of the scratch buffers.
    ///
    /// When the underlying reader temporarily returns zero frames without EOF
    /// (for example, while an async seek is still settling), this method
    /// zero-fills the requested range and reports [`ReadOutcome::Full`].
    /// That silence is not a terminal condition and must not trigger track
    /// advancement.
    pub fn read(
        &mut self,
        output: &mut [&mut [f32]],
        range: Range<usize>,
        metrics: &RtMetrics,
    ) -> ReadOutcome {
        self.read_with_context(None, output, range, metrics)
    }

    pub(crate) fn read_with_context(
        &mut self,
        context: Option<&RenderContext>,
        output: &mut [&mut [f32]],
        range: Range<usize>,
        metrics: &RtMetrics,
    ) -> ReadOutcome {
        let frames_to_read = range.end - range.start;
        let mut eof_reached = self.fill_scratch(frames_to_read, metrics);

        if self.write_len == 0 && self.failed && !self.eof_seen {
            let range_len = range.len();
            for ch in output.iter_mut() {
                ch[..range_len].fill(0.0);
            }
            return ReadOutcome::Failed;
        }

        if self.write_len > 0 {
            let frames_to_write = frames_to_read.min(self.write_len);
            let tail_size = self.write_len - frames_to_write;

            if output.len() >= Self::STEREO_CHANNELS {
                output[0][..frames_to_write]
                    .copy_from_slice(&self.channel_buffers[0][..frames_to_write]);
                output[1][..frames_to_write]
                    .copy_from_slice(&self.channel_buffers[1][..frames_to_write]);
            }

            self.consume_source(frames_to_write, context);

            if tail_size > 0 {
                self.channel_buffers[0]
                    .copy_within(frames_to_write..frames_to_write + tail_size, 0);
                self.channel_buffers[1]
                    .copy_within(frames_to_write..frames_to_write + tail_size, 0);
            }

            self.write_len -= frames_to_write;
            self.write_pos = tail_size;

            if frames_to_write == frames_to_read {
                let target = self.prefetch_target(frames_to_read);
                eof_reached |= self.fill_scratch(target, metrics);
            }

            if frames_to_write == frames_to_read {
                ReadOutcome::Full {
                    frames: frames_to_write,
                }
            } else if eof_reached {
                ReadOutcome::Partial {
                    frames: frames_to_write,
                }
            } else {
                metrics.record_underrun();
                for ch in output.iter_mut() {
                    ch[frames_to_write..frames_to_read].fill(0.0);
                }
                ReadOutcome::Full {
                    frames: frames_to_write,
                }
            }
        } else if eof_reached {
            ReadOutcome::Eof
        } else {
            metrics.record_underrun();
            let range_len = range.len();
            for ch in output.iter_mut() {
                ch[..range_len].fill(0.0);
            }
            ReadOutcome::Full { frames: 0 }
        }
    }

    /// Drop everything buffered ahead of a seek the control thread began. Lock-free: the reader
    /// picks up the epoch itself via `sync_seek`.
    pub fn reset_for_seek(&mut self) {
        self.resource.get_mut().sync_seek();
        self.write_len = 0;
        self.write_pos = 0;
        self.source_spans.clear();
        self.last_source_end = None;
        self.resource.get().clear_render();
        self.eof_seen = false;
        self.failed = false;
    }

    const fn scratch_frames(sample_rate: u32) -> FrameCount {
        FrameCount::new(sample_rate as usize / Self::BUFFER_DURATION_DIVISOR)
    }

    /// Control-plane handle used to begin a seek off the audio thread.
    #[must_use]
    pub fn seek_handle(&self) -> Option<Arc<dyn kithara_audio::SeekBegin>> {
        self.resource.get().seek_handle()
    }

    pub(crate) fn render_reader(&self) -> Option<RenderReader> {
        self.resource.get().render_reader()
    }

    delegate::delegate! {
        to self.resource.get() {
            /// Total duration in seconds. Returns 0.0 if unknown.
            #[must_use]
            #[expr($.map_or(0.0, |d| d.as_secs_f64()))]
            pub fn duration(&self) -> f64;
            /// Set the target sample rate of the audio host.
            pub(crate) fn set_host_sample_rate(&self, sample_rate: NonZeroU32);
            /// Update the scheduling priority hint for the shared worker.
            pub(crate) fn set_service_class(&self, class: ServiceClass);
            pub(crate) fn clear_render(&self);
            pub(crate) fn publish_render(
                &self,
                context: &RenderContext,
                frontier: PresentationFrontier,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_signal::{AudioSpec, SampleCount};
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(44_100, 8_820)]
    #[case(48_000, 9_600)]
    #[case(96_000, 19_200)]
    fn scratch_holds_200ms_of_frames(#[case] sample_rate: u32, #[case] expected: usize) {
        assert_eq!(
            PlayerResource::scratch_frames(sample_rate),
            FrameCount::new(expected)
        );
    }

    #[kithara::test]
    fn an_interleaved_length_is_not_a_frame_count() {
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate is non-zero"));
        let frames = PlayerResource::scratch_frames(48_000);
        assert_eq!(
            spec.sample_count(frames),
            Ok(SampleCount::new(frames.get() * 2))
        );
    }

    #[kithara::test]
    fn partial_scratch_consumption_preserves_the_render_revision() {
        let rate = NonZeroU32::new(48_000).expect("fixture sample rate is non-zero");
        let source = SourceSpan::new(100, 130, rate).map(|span| span.with_render_revision(7));
        let mut span = SourceWindow { frames: 10, source };

        assert_eq!(
            span.take(4),
            SourceSpan::new(100, 112, rate).map(|span| span.with_render_revision(7))
        );
        assert_eq!(span.source.map(|source| source.start()), Some(112));
        assert_eq!(
            span.take(6),
            SourceSpan::new(112, 130, rate).map(|span| span.with_render_revision(7))
        );
    }
}
