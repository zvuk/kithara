use std::{collections::VecDeque, num::NonZeroU32, ops::Range};

use kithara_audio::{SourceEnd, SourceSpan};
use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_platform::{maybe_send::WasmSend, sync::Arc};
use kithara_signal::FrameCount;
use kithara_test_macros as kithara;
use kithara_warp::{PresentationFrontier, RenderContext, RenderReader};
use num_traits::cast::AsPrimitive;

#[rustfmt::skip]
use crate::resource::Resource;
use crate::{bridge::RtMetrics, worker::ServiceClass};

/// RT-safe resource wrapper with internal scratch buffers.
///
/// Wraps a [`Resource`] and maintains per-channel scratch plus matching
/// media progress filled from the underlying `AudioReader`. The audio thread
/// reads from these buffers, avoiding direct interaction with the
/// potentially-blocking decoder on every callback.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct PlayerResource {
    #[field(get, deref = false)]
    src: Arc<str>,
    resource: WasmSend<Resource>,
    channel_buffers: [SampleBuffer; Self::STEREO_CHANNELS],
    media_spans: VecDeque<MediaSpan>,
    eof_seen: bool,
    failed: bool,
    write_len: usize,
    write_pos: usize,
    /// Media progress copied by the most recent [`Self::read`].
    #[field(get, vis = "pub(crate)", copy)]
    consumed_media_seconds: f64,
    /// Exact decoded-source boundary copied through the scratch buffer.
    last_source_end: Option<SourceEnd>,
}

#[derive(Clone, Copy)]
struct MediaSpan {
    consumed_frames: usize,
    frames: usize,
    seconds: f64,
    source: Option<SourceSpan>,
}

impl MediaSpan {
    const fn new(frames: usize, seconds: f64, source: Option<SourceSpan>) -> Self {
        Self {
            consumed_frames: 0,
            frames,
            seconds,
            source,
        }
    }

    fn remaining_frames(&self) -> usize {
        self.frames - self.consumed_frames
    }

    fn source_for(&self, frames: usize) -> Option<SourceSpan> {
        let source = self.source?;
        let start = self.consumed_frames;
        let end = start + frames.min(self.remaining_frames());
        let source_start = source_frame_at(source, start, self.frames)?;
        let source_end = source_frame_at(source, end, self.frames)?;
        SourceSpan::new(source_start, source_end, source.sample_rate())
            .map(|span| span.with_render_revision(source.render_revision()))
    }

    fn take(&mut self, frames: usize) -> (f64, Option<SourceSpan>) {
        let consumed_frames = frames.min(self.remaining_frames());
        let consumed_source = self.source_for(consumed_frames);
        self.consumed_frames += consumed_frames;
        let consumed_frames: f64 = AsPrimitive::as_(consumed_frames);
        let span_frames: f64 = AsPrimitive::as_(self.frames);
        let seconds =
            consumed_source.map_or(self.seconds * consumed_frames / span_frames, |source| {
                let source_frames: f64 = AsPrimitive::as_(source.end() - source.start());
                source_frames / f64::from(source.sample_rate().get())
            });
        (seconds, consumed_source)
    }

    #[kithara::probe(
        render_revision = source.render_revision(),
        session_epoch = u64::from(context.session_epoch()),
        output_start = i64::from(context.output_frames().start),
        output_end = i64::from(context.output_frames().end),
        source_start = source.start(),
        source_end = source.end()
    )]
    fn pcm_consumed(
        &mut self,
        frames: usize,
        context: &RenderContext,
        source: SourceSpan,
    ) -> (f64, Option<SourceSpan>) {
        let consumed = self.take(frames);
        debug_assert_eq!(consumed.1, Some(source));
        consumed
    }
}

fn source_frame_at(source: SourceSpan, frames: usize, span_frames: usize) -> Option<u64> {
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
            media_spans: VecDeque::with_capacity(buffer_frames),
            src,
            resource: WasmSend::new(resource),
            write_len: 0,
            write_pos: 0,
            consumed_media_seconds: 0.0,
            last_source_end: None,
            eof_seen: false,
            failed: false,
        })
    }

    pub(crate) fn apply_playback_rate(&self, rate: f32) -> f32 {
        self.resource.get().apply_playback_rate(rate)
    }

    #[kithara::probe(
        requested_frames,
        read_frames,
        scratch_before,
        session_frame,
        eof = self.eof_seen,
        failed = self.failed
    )]
    fn commit_resource_read(
        &mut self,
        requested_frames: usize,
        read_frames: usize,
        media_seconds: f64,
        source: Option<SourceSpan>,
        scratch_before: usize,
        session_frame: i64,
    ) {
        if read_frames > 0 {
            self.media_spans
                .push_back(MediaSpan::new(read_frames, media_seconds, source));
            self.write_len += read_frames;
            self.write_pos += read_frames;
        }
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

    fn fill_scratch(
        &mut self,
        target_frames: usize,
        context: Option<&RenderContext>,
        metrics: &RtMetrics,
    ) -> bool {
        let mut eof_reached = self.eof_seen;
        let session_frame = context.map_or(-1, |context| i64::from(context.output_frames().start));

        while target_frames > self.write_len && !eof_reached {
            let needed = target_frames - self.write_len;
            let avail = (self.channel_buffers[0].len() - self.write_pos).min(needed);
            if avail == 0 {
                break;
            }

            let channel_buffers = &mut self.channel_buffers;
            let (left_buf, right_buf) = channel_buffers.split_at_mut(1);
            let left = &mut left_buf[0][self.write_pos..self.write_pos + avail];
            let right = &mut right_buf[0][self.write_pos..self.write_pos + avail];
            let mut planar: [&mut [f32]; Self::STEREO_CHANNELS] = [left, right];

            let scratch_before = self.write_len;
            let start_position = self.resource.get().position();
            let (n, media_seconds, source) = match self.resource.get_mut().read_planar(&mut planar)
            {
                Ok(kithara_audio::ReadOutcome::Frames {
                    count,
                    position,
                    source_span,
                }) => (
                    count.get(),
                    position.saturating_sub(start_position).as_secs_f64(),
                    source_span,
                ),
                Ok(kithara_audio::ReadOutcome::Pending { .. }) => (0, 0.0, None),
                Ok(kithara_audio::ReadOutcome::Eof { .. }) => {
                    self.eof_seen = true;
                    eof_reached = true;
                    (0, 0.0, None)
                }
                Err(_) => {
                    metrics.record_decode_error();
                    self.failed = true;
                    (0, 0.0, None)
                }
            };
            self.commit_resource_read(
                avail,
                n,
                media_seconds,
                source,
                scratch_before,
                session_frame,
            );
            if n == 0 {
                break;
            }
        }

        eof_reached
    }

    fn prefetch_target(&self, callback_frames: usize) -> usize {
        self.write_len
            .saturating_add(callback_frames)
            .min(self.channel_buffers[0].len())
    }

    fn consume_media(&mut self, mut frames: usize, context: Option<&RenderContext>) -> f64 {
        let mut seconds = 0.0;
        let mut source_end = self.last_source_end;
        let mut output_start = 0usize;
        while frames > 0 {
            let Some(mut span) = self.media_spans.pop_front() else {
                break;
            };
            let consumed = frames.min(span.remaining_frames());
            let output_end = output_start.saturating_add(consumed);
            let presented = context
                .and_then(|context| context.for_output_range(output_start..output_end))
                .zip(span.source_for(consumed));
            let (consumed_seconds, consumed_source) = match presented {
                Some((context, source)) => span.pcm_consumed(consumed, &context, source),
                None => span.take(consumed),
            };
            seconds += consumed_seconds;
            if let Some(consumed_source) = consumed_source {
                source_end = Some(SourceEnd::new(
                    consumed_source.end(),
                    consumed_source.sample_rate(),
                ));
            }
            frames -= consumed;
            output_start = output_start.saturating_add(consumed);
            if span.remaining_frames() > 0 {
                self.media_spans.push_front(span);
            }
        }
        self.last_source_end = source_end;
        seconds
    }

    pub(crate) fn presentation_source_end(&self, sample_rate: NonZeroU32) -> Option<SourceEnd> {
        let source_end = self.last_source_end?;
        (source_end.sample_rate() == sample_rate
            && source_end.sample_rate() == self.resource.get().spec().sample_rate)
            .then_some(source_end)
    }

    /// Remaining buffered frames when the wrapped reader has reached EOF.
    ///
    /// `Some(0)` means the current read drained the last buffered frame exactly;
    /// the next read will return [`ReadOutcome::Eof`].
    #[must_use]
    pub fn frames_until_eof(&self) -> Option<usize> {
        self.eof_seen.then_some(self.write_len)
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

    #[kithara::measure]
    pub(crate) fn read_with_context(
        &mut self,
        context: Option<&RenderContext>,
        output: &mut [&mut [f32]],
        range: Range<usize>,
        metrics: &RtMetrics,
    ) -> ReadOutcome {
        self.consumed_media_seconds = 0.0;
        let frames_to_read = range.end - range.start;
        let mut eof_reached = self.fill_scratch(frames_to_read, context, metrics);

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

            self.consumed_media_seconds = self.consume_media(frames_to_write, context);

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
                eof_reached |= self.fill_scratch(target, context, metrics);
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
        self.media_spans.clear();
        self.consumed_media_seconds = 0.0;
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
    fn partial_scratch_consumption_advances_the_exact_source_span() {
        let rate = NonZeroU32::new(48_000).expect("fixture sample rate is non-zero");
        let mut span = MediaSpan::new(
            10,
            10.0 / f64::from(rate.get()),
            SourceSpan::new(100, 130, rate),
        );

        let (seconds, partial) = span.take(4);
        assert!((seconds - 12.0 / f64::from(rate.get())).abs() <= f64::EPSILON);
        assert_eq!(partial, SourceSpan::new(100, 112, rate));
        assert_eq!(span.remaining_frames(), 6);

        let (_, full) = span.take(6);
        assert_eq!(full, SourceSpan::new(112, 130, rate));
    }

    #[kithara::test]
    fn repeated_partial_consumption_uses_one_cumulative_source_ratio() {
        let rate = NonZeroU32::new(48_000).expect("fixture sample rate is non-zero");
        let mut span = MediaSpan::new(
            6,
            6.0 / f64::from(rate.get()),
            SourceSpan::new(100, 110, rate),
        );

        let (_, first) = span.take(1);
        let (_, second) = span.take(1);
        let (_, remainder) = span.take(4);

        assert_eq!(first, SourceSpan::new(100, 101, rate));
        assert_eq!(second, SourceSpan::new(101, 103, rate));
        assert_eq!(remainder, SourceSpan::new(103, 110, rate));
    }
}
