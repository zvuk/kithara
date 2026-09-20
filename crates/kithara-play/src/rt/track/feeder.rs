use std::{
    collections::VecDeque,
    num::{NonZeroU32, NonZeroUsize},
    ops::Range,
};

use kithara_audio::{RevisionFloorStatus, SourceEnd, SourceSpan};
use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_events::TrackId;
use kithara_platform::{maybe_send::WasmSend, sync::Arc};
use kithara_signal::FrameCount;
use kithara_test_macros as kithara;
use kithara_warp::{PresentationFrontier, RenderContext, RenderReader};

use super::feeder_read::ScheduledSeekPresentation;

#[path = "buffer.rs"]
mod buffer;
#[path = "output.rs"]
mod output;
#[path = "seek.rs"]
mod seek;

#[rustfmt::skip]
use crate::resource::Resource;
use crate::{
    bridge::{RtMetrics, ScheduledSeekDisposition, ScheduledSeekEpoch},
    resource::RenderActivation,
    worker::ServiceClass,
};

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
    pub(super) src: Arc<str>,
    pub(super) last_source_end: Option<SourceEnd>,
    pub(super) last_warp_map_revision: u64,
    pub(super) source_spans: VecDeque<SourceWindow>,
    pub(super) resource: WasmSend<Resource>,
    pub(super) channel_buffers: [SampleBuffer; Self::STEREO_CHANNELS],
    pub(super) activation_tail: Option<[SampleBuffer; Self::STEREO_CHANNELS]>,
    pub(super) activation_blend_frames: usize,
    pub(super) activation_blend_pos: usize,
    pub(super) eof_seen: bool,
    pub(super) failed: bool,
    pub(super) render_revision_floor: u64,
    pub(super) scheduled_seek: Option<ScheduledSeekRecord>,
    pub(super) latest_scheduled_epoch: Option<ScheduledSeekEpoch>,
    pub(super) write_len: usize,
    pub(super) write_pos: usize,
}

#[derive(Clone, Copy)]
pub(super) struct ScheduledSeekRecord {
    pub(super) scheduled_epoch: ScheduledSeekEpoch,
    pub(super) decoder_epoch: u64,
    pub(super) disposition: ScheduledSeekDisposition,
    pub(super) armed: bool,
}

#[derive(Clone, Copy)]
pub(super) struct SourceWindow {
    pub(super) source: Option<SourceSpan>,
    pub(super) frames: usize,
    pub(super) media_frames: u64,
    pub(super) consumed_frames: usize,
}

impl SourceWindow {
    pub(super) fn remaining(&self) -> usize {
        self.frames - self.consumed_frames
    }

    pub(super) fn source_for(&self, frames: usize) -> Option<SourceSpan> {
        let source = self.source?;
        let start = partial_source_end(source, self.consumed_frames, self.frames)?;
        let end = partial_source_end(
            source,
            self.consumed_frames + frames.min(self.remaining()),
            self.frames,
        )?;
        SourceSpan::new(start, end, source.sample_rate())
            .map(|span| span.with_render_revision(source.render_revision()))
    }

    pub(super) fn take(&mut self, frames: usize) -> (Option<SourceSpan>, u64) {
        let consumed = frames.min(self.remaining());
        let taken = self.source_for(consumed);
        let before = partial_frames(self.media_frames, self.consumed_frames, self.frames);
        self.consumed_frames += consumed;
        let after = partial_frames(self.media_frames, self.consumed_frames, self.frames);
        (taken, after - before)
    }
}

pub(super) fn partial_frames(total: u64, frames: usize, span_frames: usize) -> u64 {
    let numerator = u128::from(total) * u128::try_from(frames).unwrap_or(u128::MAX);
    let denominator = u128::try_from(span_frames).unwrap_or(u128::MAX);
    u64::try_from(numerator / denominator).unwrap_or(total)
}

pub(super) fn partial_source_end(
    source: SourceSpan,
    frames: usize,
    span_frames: usize,
) -> Option<u64> {
    let source_frames = source.end().checked_sub(source.start())?;
    let numerator = u128::from(source_frames).checked_mul(u128::try_from(frames).ok()?)?;
    let denominator = u128::try_from(span_frames).ok()?;
    let consumed = u64::try_from(numerator.checked_div(denominator)?).ok()?;
    source.start().checked_add(consumed)
}

pub(super) fn activation_prefix(
    context: &RenderContext,
    activation: RenderActivation,
) -> Option<usize> {
    let start = i64::from(context.output_frames().start);
    let end = i64::from(context.output_frames().end);
    let output = i64::from(activation.output);
    if output >= end {
        return None;
    }
    if output <= start {
        return Some(0);
    }
    usize::try_from(output - start).ok()
}

pub(super) fn combine_reads(
    prefix_frames: usize,
    prefix_source_frames: u64,
    suffix: ReadOutcome,
    suffix_source_frames: u64,
) -> (ReadOutcome, u64) {
    let Some(source_frames) = prefix_source_frames.checked_add(suffix_source_frames) else {
        return (ReadOutcome::Failed, 0);
    };
    let outcome = match suffix {
        ReadOutcome::Full { frames } => ReadOutcome::Full {
            frames: prefix_frames.saturating_add(frames),
        },
        ReadOutcome::Partial { frames } => ReadOutcome::Partial {
            frames: prefix_frames.saturating_add(frames),
        },
        ReadOutcome::Eof => ReadOutcome::Partial {
            frames: prefix_frames,
        },
        ReadOutcome::Failed => ReadOutcome::Failed,
    };
    (outcome, source_frames)
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
    pub(super) const BUFFER_DURATION_DIVISOR: usize = 5;

    /// Number of stereo output channels.
    pub(super) const STEREO_CHANNELS: usize = 2;

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
        let activation_blend_frames = resource
            .activation_blend_frames()
            .map_or(0, NonZeroUsize::get);
        let activation_tail = if activation_blend_frames == 0 {
            None
        } else {
            Some([
                pools.get_with_len::<f32>(activation_blend_frames)?,
                pools.get_with_len::<f32>(activation_blend_frames)?,
            ])
        };

        Ok(Self {
            src,
            channel_buffers: [left, right],
            activation_tail,
            activation_blend_frames,
            activation_blend_pos: activation_blend_frames,
            source_spans: VecDeque::with_capacity(buffer_frames),
            resource: WasmSend::new(resource),
            write_len: 0,
            write_pos: 0,
            last_source_end: None,
            last_warp_map_revision: 0,
            eof_seen: false,
            failed: false,
            render_revision_floor: 0,
            scheduled_seek: None,
            latest_scheduled_epoch: None,
        })
    }

    delegate::delegate! {
        to self.resource.get() {
            pub(crate) fn apply_playback_rate(&self, rate: f32) -> f32;
            pub(crate) fn playback_rate(&self) -> f32;
        }
    }

    /// Cached span in seconds: how much of the source is on disk and needs no
    /// further network.
    #[must_use]
    pub fn cached_span(&self) -> f64 {
        self.resource.get().cached_span().as_secs_f64()
    }
}
#[cfg(test)]
mod tests {
    use kithara_signal::{AudioSpec, FrameCount, SampleCount};
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
    fn partial_scratch_source_position_is_independent_of_read_partition() {
        let rate = NonZeroU32::new(48_000).expect("fixture sample rate is non-zero");
        let source = SourceSpan::new(100, 107, rate);
        let mut span = SourceWindow {
            source,
            frames: 10,
            media_frames: 7,
            consumed_frames: 0,
        };
        let mut media_frames = 0;
        for output_frame in 1..=10 {
            let (taken, consumed) = span.take(1);
            media_frames += consumed;
            let expected = 7 * output_frame / 10;
            assert_eq!(
                taken.map(|source| source.end()),
                Some(100 + expected),
                "source frontier after {output_frame} output frames"
            );
            assert_eq!(media_frames, expected);
        }
    }

    #[kithara::test]
    #[case(100, Some(0))]
    #[case(105, Some(5))]
    #[case(110, None)]
    fn activation_partition_uses_the_exact_host_frame(
        #[case] activation_output: i64,
        #[case] expected_prefix: Option<usize>,
    ) {
        let sample_rate = NonZeroU32::new(48_000).expect("fixture sample rate is non-zero");
        let context = RenderContext::new(
            kithara_warp::SessionFrame::new(100)..kithara_warp::SessionFrame::new(110),
            sample_rate,
            None,
            kithara_warp::SessionEpoch::new(1),
            None,
        )
        .expect("fixture context is valid");
        let activation = RenderActivation {
            output: kithara_warp::SessionFrame::new(activation_output),
            revision: 7,
        };

        assert_eq!(activation_prefix(&context, activation), expected_prefix);
    }

    #[kithara::test]
    fn partial_scratch_consumption_preserves_render_provenance() {
        let rate = NonZeroU32::new(48_000).expect("fixture sample rate is non-zero");
        let revision = kithara_signal::pack_render_revision(7, 11)
            .expect("fixture revisions fit the provenance word");
        let source =
            SourceSpan::new(100, 130, rate).map(|span| span.with_render_revision(revision));
        let mut span = SourceWindow {
            source,
            frames: 10,
            media_frames: 30,
            consumed_frames: 0,
        };

        assert_eq!(
            span.take(4),
            (
                SourceSpan::new(100, 112, rate).map(|span| span.with_render_revision(revision)),
                12
            )
        );
        assert_eq!(span.source_for(6).map(|source| source.start()), Some(112));
        assert_eq!(
            span.take(6),
            (
                SourceSpan::new(112, 130, rate).map(|span| span.with_render_revision(revision)),
                18
            )
        );
    }
}
