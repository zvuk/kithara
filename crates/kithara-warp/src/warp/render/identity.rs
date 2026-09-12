use std::{marker::PhantomData, num::NonZeroU32};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, FrameCount};
use kithara_test_macros as kithara;

use crate::{RenderReader, RenderSnapshot, WarpConfig};

/// Identity renderer for targets without elastic DSP.
/// It preserves decoded samples exactly and keeps playback-rate capability disabled.
#[non_exhaustive]
pub struct WarpRenderer<S> {
    committed: Option<RenderSnapshot>,
    prepared: Option<usize>,
    rendered_source_end: Option<(u64, NonZeroU32)>,
    schema: PhantomData<fn() -> S>,
    context: RenderReader,
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(crate) fn new(
        _config: &WarpConfig,
        context: RenderReader,
        _spec: AudioSpec,
        _pools: PoolRegion<S>,
    ) -> Self {
        Self {
            context,
            committed: None,
            prepared: None,
            rendered_source_end: None,
            schema: PhantomData,
        }
    }

    /// Whether the renderer can accept another source chunk.
    #[must_use]
    pub const fn accepts_input(&self) -> bool {
        true
    }

    /// Drain one buffered output chunk after source EOF or a transition.
    pub const fn flush(&mut self) -> Option<AudioChunk> {
        None
    }

    /// Prepare deferred renderer state for the current source format.
    pub const fn prepare(&mut self, _spec: AudioSpec) {}

    /// Select the next source span that fits the output quantum.
    pub fn prepare_quantum(
        &mut self,
        _meta: AudioChunkInfo,
        remaining: usize,
    ) -> Option<FrameCount> {
        self.prepared = (remaining > 0).then_some(remaining);
        self.prepared.map(FrameCount::new)
    }

    /// Shrink a prepared source span at true EOF.
    pub fn prepare_terminal_quantum(
        &mut self,
        _meta: AudioChunkInfo,
        frames: usize,
    ) -> Option<FrameCount> {
        let prepared = self.prepared.take()?;
        if frames == 0 || frames > prepared {
            return None;
        }
        self.prepared = Some(frames);
        Some(FrameCount::new(frames))
    }

    /// Render one complete decoded source chunk.
    pub fn render(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        self.prepared = None;
        self.render_prepared(chunk)
    }

    fn render_prepared(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        let snapshot = self.context.load();
        self.rendered_source_end = Some((
            chunk
                .meta
                .frame_offset
                .saturating_add(u64::from(chunk.meta.frames)),
            chunk.meta.spec.sample_rate,
        ));
        if let Some(snapshot) = snapshot
            && self.context.is_current(&snapshot)
            && let Some((source, _)) = self.rendered_source_end
        {
            let source_start = snapshot.frontier().source();
            let output_start = i64::from(snapshot.frontier().output());
            if let Some(committed) =
                snapshot.advance(self.committed.as_ref(), source, chunk.frames())
            {
                kithara::probe_event!(
                    render_committed,
                    session_epoch = u64::from(committed.context().session_epoch()),
                    transport_revision = committed
                        .context()
                        .transport_revision()
                        .map_or(0, u64::from),
                    output_start,
                    source_start,
                    source_end = committed.frontier().source()
                );
                self.committed = Some(committed);
            }
        }
        Some(chunk)
    }

    /// Render the source span selected by [`Self::prepare_quantum`].
    pub fn render_quantum(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        let frames = self.prepared.take()?;
        (chunk.frames() == frames).then(|| self.render_prepared(chunk))?
    }

    /// Exact decoded-source boundary represented by the latest emitted samples.
    #[must_use]
    pub const fn rendered_source_end(&self) -> Option<(u64, NonZeroU32)> {
        self.rendered_source_end
    }

    /// Whether rendering needs worker-owned staging buffers.
    #[must_use]
    pub const fn requires_staging(&self) -> bool {
        false
    }

    /// Discard renderer state after a source discontinuity.
    pub const fn reset(&mut self) {
        self.committed = None;
        self.prepared = None;
        self.rendered_source_end = None;
    }

    /// Whether a live transition still owns buffered samples.
    #[must_use]
    pub const fn transition_pending(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_signal::AudioChunkInfo;
    use kithara_test_fixtures::unit_fixtures::warp_pair;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{pools, sample_buffer};

    #[kithara::test]
    fn renderer_preserves_samples_exactly(warp_pair: Vec<f32>) {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test sample rate"));
        let mut meta = AudioChunkInfo::default();
        meta.spec = spec;
        meta.frames = 1;
        meta.frame_offset = 41;
        let input = AudioChunk::new(meta, sample_buffer(&pools, &warp_pair));
        let input_ptr = input.samples.as_ptr();
        let config = WarpConfig::builder()
            .stretch(StretchControls::new(1.5))
            .build();
        let mut renderer = WarpRenderer::new(
            &config,
            crate::RenderPublisher::default().reader(),
            spec,
            pools,
        );

        assert_eq!(renderer.rendered_source_end(), None);
        let output = renderer.render(input).expect("identity output");

        assert_eq!(output.samples.as_ptr(), input_ptr);
        assert_eq!(output.samples.as_ref(), &[0.25, -0.5]);
        assert_eq!(renderer.rendered_source_end(), Some((42, spec.sample_rate)));
        renderer.reset();
        assert_eq!(renderer.rendered_source_end(), None);
        assert!(renderer.flush().is_none());
        assert!(!crate::supports_playback_rate());
    }
}
