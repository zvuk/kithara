#![cfg(not(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
)))]

use std::{marker::PhantomData, num::NonZeroU32};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, FrameCount};

use crate::{RenderReader, RenderSnapshot, WarpConfig};

/// Identity renderer for targets without elastic DSP.
/// It preserves decoded samples exactly and keeps playback-rate capability disabled.
#[non_exhaustive]
pub struct WarpRenderer<S> {
    context: RenderReader,
    committed: Option<RenderSnapshot>,
    prepared: Option<(usize, Option<RenderSnapshot>)>,
    rendered_source_end: Option<(u64, NonZeroU32)>,
    schema: PhantomData<fn() -> S>,
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

    pub(crate) fn new_quantum(
        config: &WarpConfig,
        context: RenderReader,
        spec: AudioSpec,
        pools: PoolRegion<S>,
    ) -> Self {
        Self::new(config, context, spec, pools)
    }

    #[doc(hidden)]
    pub const fn accepts_input(&self) -> bool {
        true
    }

    #[doc(hidden)]
    pub const fn prepare_terminal(&mut self) {}

    #[doc(hidden)]
    pub const fn flush(&mut self) -> Option<AudioChunk> {
        None
    }

    #[doc(hidden)]
    pub const fn prepare(&mut self, _spec: AudioSpec) {}

    #[doc(hidden)]
    pub fn prepare_quantum(
        &mut self,
        _meta: AudioChunkInfo,
        remaining: usize,
    ) -> Option<FrameCount> {
        self.prepared = None;
        (remaining > 0).then(|| {
            self.prepared = Some((remaining, self.context.load()));
            FrameCount::new(remaining)
        })
    }

    #[doc(hidden)]
    pub fn prepare_terminal_quantum(
        &mut self,
        _meta: AudioChunkInfo,
        frames: usize,
    ) -> Option<FrameCount> {
        let (_, context) = self.prepared.take()?;
        (frames > 0).then(|| {
            self.prepared = Some((frames, context));
            FrameCount::new(frames)
        })
    }

    #[doc(hidden)]
    pub fn render(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        self.rendered_source_end = Some((
            chunk
                .meta
                .frame_offset
                .saturating_add(u64::from(chunk.meta.frames)),
            chunk.meta.spec.sample_rate,
        ));
        Some(chunk)
    }

    #[doc(hidden)]
    pub fn render_quantum(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        let (frames, context) = self.prepared.take()?;
        if chunk.frames() != frames
            || context
                .as_ref()
                .is_some_and(|snapshot| !self.context.is_current(snapshot))
        {
            return None;
        }
        let output = self.render(chunk)?;
        if let Some(snapshot) = context
            && let Some((source, _)) = self.rendered_source_end
            && let Some(committed) =
                snapshot.advance(self.committed.as_ref(), source, output.frames())
        {
            self.committed = Some(committed);
        }
        Some(output)
    }

    /// Last context and frontier committed by a successful worker quantum.
    #[doc(hidden)]
    #[must_use]
    pub fn render_snapshot(&self) -> Option<&RenderSnapshot> {
        self.committed.as_ref()
    }

    #[doc(hidden)]
    pub const fn rendered_source_end(&self) -> Option<(u64, NonZeroU32)> {
        self.rendered_source_end
    }

    #[doc(hidden)]
    pub const fn reset(&mut self) {
        self.committed = None;
        self.prepared = None;
        self.rendered_source_end = None;
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_signal::AudioChunkInfo;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{pools, sample_buffer};

    #[kithara::test]
    fn renderer_preserves_samples_exactly() {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test sample rate"));
        let mut meta = AudioChunkInfo::default();
        meta.spec = spec;
        meta.frames = 1;
        meta.frame_offset = 41;
        let input = AudioChunk::new(meta, sample_buffer(&pools, &[0.25, -0.5]));
        let input_ptr = input.samples.as_ptr();
        let renderer_config = WarpConfig::builder().build();
        let warp = crate::Warp::new((), &renderer_config);
        let mut renderer =
            WarpRenderer::new(&renderer_config, warp.publisher().reader(), spec, pools);

        assert_eq!(renderer.rendered_source_end(), None);
        let output = renderer.render(input).expect("identity output");

        assert_eq!(output.samples.as_ptr(), input_ptr);
        assert_eq!(output.samples.as_ref(), &[0.25, -0.5]);
        assert_eq!(renderer.rendered_source_end(), Some((42, spec.sample_rate)));
        renderer.reset();
        assert_eq!(renderer.rendered_source_end(), None);
        assert!(renderer.flush().is_none());
    }
}
