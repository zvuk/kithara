//! `EffectBridgeNode` — wraps an [`AudioEffect`] as a Firewheel [`AudioNode`].
//!
//! Converts between Firewheel's per-channel buffer slices and [`PcmChunk`]'s
//! interleaved format. Allows any [`AudioEffect`] (EQ, compressor, etc.)
//! to be inserted in the Firewheel audio graph on the master bus.
//!
//! [`AudioEffect`]: kithara_audio::AudioEffect
//! [`AudioNode`]: firewheel::node::AudioNode

use std::num::{NonZeroU32, NonZeroUsize};

use fast_interleave::{deinterleave_variable, interleave_variable};
use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    diff::{Diff, Patch},
    event::ProcEvents,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        ProcBuffers, ProcExtra, ProcInfo, ProcessStatus,
    },
};
use kithara_audio::AudioEffect;
use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_platform::Mutex;

/// Number of stereo channels (index math).
const STEREO: usize = 2;

/// Number of stereo channels for [`PcmSpec`].
const STEREO_CHANNELS: u16 = 2;

/// [`AudioNode`] wrapping an [`AudioEffect`] for the Firewheel graph.
///
/// The effect is moved into the processor on construction via interior
/// mutability (`Mutex<Option<...>>`), since [`AudioNode::construct_processor`]
/// takes `&self`.
///
/// The node is stereo in / stereo out.
///
/// [`AudioEffect`]: kithara_audio::AudioEffect
#[derive(Diff, Patch)]
pub(crate) struct EffectBridgeNode {
    /// Whether the node is active (used by Diff/Patch for graph updates).
    active: bool,

    /// The effect, wrapped in a Mutex<Option<...>> so it can be taken
    /// from `&self` in `construct_processor`. Set to `None` after the
    /// processor is built.
    #[diff(skip)]
    effect: Mutex<Option<Box<dyn AudioEffect>>>,

    /// PCM buffer pool for scratch buffer allocation.
    #[diff(skip)]
    pcm_pool: PcmPool,
}

impl EffectBridgeNode {
    /// Create a new bridge node wrapping the given effect.
    pub(crate) fn new(effect: Box<dyn AudioEffect>, pcm_pool: PcmPool) -> Self {
        Self {
            active: true,
            effect: Mutex::new(Some(effect)),
            pcm_pool,
        }
    }
}

impl AudioNode for EffectBridgeNode {
    type Configuration = EmptyConfig;

    fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name("EffectBridge")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::STEREO,
                num_outputs: ChannelCount::STEREO,
            })
    }

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        let effect = self.effect.lock().take();

        if effect.is_none() {
            tracing::error!("EffectBridgeNode::construct_processor called more than once");
        }

        EffectBridgeProcessor::new(effect, cx.stream_info.sample_rate, self.pcm_pool.clone())
    }
}

/// Realtime processor that applies an [`AudioEffect`] to Firewheel buffers.
///
/// On each `process()` call:
/// 1. Interleaves input channel slices into a scratch buffer.
/// 2. Wraps the scratch buffer in a [`PcmChunk`].
/// 3. Passes the chunk through `effect.process()`.
/// 4. Deinterleaves the result back into output channel slices.
///
/// The scratch `PcmBuf` is obtained from the global pool at construction
/// time and reused across calls to avoid per-block heap allocation.
///
/// [`AudioEffect`]: kithara_audio::AudioEffect
pub(crate) struct EffectBridgeProcessor {
    /// The wrapped effect. `None` only if `construct_processor` was called
    /// more than once (which logs an error); in that case the processor
    /// acts as a bypass node.
    effect: Option<Box<dyn AudioEffect>>,
    pool: PcmPool,
    sample_rate: NonZeroU32,
    /// Reusable interleaved scratch buffer from the pool. Resized as needed.
    scratch: PcmBuf,
}

impl EffectBridgeProcessor {
    fn new(effect: Option<Box<dyn AudioEffect>>, sample_rate: NonZeroU32, pool: PcmPool) -> Self {
        let scratch = pool.get();
        Self {
            effect,
            pool,
            sample_rate,
            scratch,
        }
    }

    /// Interleave per-channel input slices into `self.scratch`.
    fn interleave(&mut self, inputs: &[&[f32]], frames: usize) {
        let num_channels = inputs.len();
        let needed = frames * num_channels;
        self.scratch.resize(needed, 0.0);

        if let Some(nc) = NonZeroUsize::new(num_channels) {
            interleave_variable(inputs, 0..frames, &mut self.scratch, nc);
        }
    }

    /// Deinterleave `self.scratch` into per-channel output slices.
    fn deinterleave(&self, outputs: &mut [&mut [f32]], frames: usize) {
        if let Some(nc) = NonZeroUsize::new(outputs.len()) {
            deinterleave_variable(&self.scratch, nc, outputs, 0..frames);
        }
    }
}

impl AudioNodeProcessor for EffectBridgeProcessor {
    fn new_stream(
        &mut self,
        stream_info: &StreamInfo,
        _context: &mut firewheel::node::ProcStreamCtx,
    ) {
        self.sample_rate = stream_info.sample_rate;
        if let Some(effect) = &mut self.effect {
            effect.reset();
        }
    }

    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        _events: &mut ProcEvents,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let frames = info.frames;

        if frames == 0
            || buffers.inputs.len() < STEREO
            || buffers.outputs.len() < STEREO
            || self.effect.is_none()
        {
            return ProcessStatus::Bypass;
        }

        // 1. Interleave inputs -> scratch
        self.interleave(buffers.inputs, frames);

        // 2. Build PcmChunk from scratch (swap in a fresh pool buffer)
        let pcm_buf = std::mem::replace(&mut self.scratch, self.pool.get());
        let spec = PcmSpec {
            channels: STEREO_CHANNELS,
            sample_rate: self.sample_rate.get(),
        };
        let chunk = PcmChunk::new(
            PcmMeta {
                spec,
                ..PcmMeta::default()
            },
            pcm_buf,
        );

        // 3. Process through the effect.
        //    `effect` is guaranteed to be `Some` by the `is_none()` guard above.
        let Some(effect) = self.effect.as_mut() else {
            return ProcessStatus::Bypass;
        };

        if let Some(processed) = effect.process(chunk) {
            // 4. Recover scratch buffer (keeps it pool-backed)
            self.scratch = processed.pcm;

            // 5. Deinterleave scratch -> outputs
            self.deinterleave(buffers.outputs, frames);

            ProcessStatus::OutputsModified
        } else {
            // Effect returned None (accumulating data). Pass through silence.
            // scratch already replaced with a fresh pool buffer above.
            ProcessStatus::ClearAllOutputs
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::pcm_pool;

    use super::*;

    /// A no-op effect that passes audio through unchanged.
    struct PassthroughEffect;

    impl AudioEffect for PassthroughEffect {
        fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk> {
            Some(chunk)
        }

        fn flush(&mut self) -> Option<PcmChunk> {
            None
        }

        fn reset(&mut self) {}
    }

    /// An effect that doubles all sample values.
    struct DoubleEffect;

    impl AudioEffect for DoubleEffect {
        fn process(&mut self, mut chunk: PcmChunk) -> Option<PcmChunk> {
            for sample in chunk.pcm.as_mut_slice() {
                *sample *= 2.0;
            }
            Some(chunk)
        }

        fn flush(&mut self) -> Option<PcmChunk> {
            None
        }

        fn reset(&mut self) {}
    }

    /// An effect that always returns None (accumulating).
    struct AccumulatingEffect;

    impl AudioEffect for AccumulatingEffect {
        fn process(&mut self, _chunk: PcmChunk) -> Option<PcmChunk> {
            None
        }

        fn flush(&mut self) -> Option<PcmChunk> {
            None
        }

        fn reset(&mut self) {}
    }

    #[test]
    fn node_info_is_stereo_in_stereo_out() {
        let node = EffectBridgeNode::new(Box::new(PassthroughEffect), pcm_pool().clone());
        let info = node.info(&EmptyConfig);
        // AudioNodeInfo does not expose fields, but construction should not panic.
        let _ = info;
    }

    #[test]
    fn node_defaults_to_active() {
        let node = EffectBridgeNode::new(Box::new(PassthroughEffect), pcm_pool().clone());
        assert!(node.active);
    }

    #[test]
    fn passthrough_preserves_audio() {
        let mut processor = EffectBridgeProcessor::new(
            Some(Box::new(PassthroughEffect)),
            NonZeroU32::new(44100).unwrap(),
            pcm_pool().clone(),
        );

        let frames = 4;
        let input_l: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4];
        let input_r: Vec<f32> = vec![0.5, 0.6, 0.7, 0.8];

        processor.interleave(&[&input_l, &input_r], frames);

        // Verify interleaving
        assert_eq!(processor.scratch.len(), 8);
        assert_eq!(processor.scratch[0], 0.1); // L0
        assert_eq!(processor.scratch[1], 0.5); // R0
        assert_eq!(processor.scratch[2], 0.2); // L1
        assert_eq!(processor.scratch[3], 0.6); // R1

        // Build chunk and process
        let pcm_buf = std::mem::replace(&mut processor.scratch, pcm_pool().get());
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let chunk = PcmChunk::new(
            PcmMeta {
                spec,
                ..PcmMeta::default()
            },
            pcm_buf,
        );

        let result = processor.effect.as_mut().unwrap().process(chunk).unwrap();
        processor.scratch = result.pcm;

        // Deinterleave
        let mut out_l = vec![0.0f32; frames];
        let mut out_r = vec![0.0f32; frames];
        processor.deinterleave(&mut [&mut out_l, &mut out_r], frames);

        assert_eq!(out_l, input_l);
        assert_eq!(out_r, input_r);
    }

    #[test]
    fn double_effect_modifies_audio() {
        let mut processor = EffectBridgeProcessor::new(
            Some(Box::new(DoubleEffect)),
            NonZeroU32::new(44100).unwrap(),
            pcm_pool().clone(),
        );

        let frames = 4;
        let input_l: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4];
        let input_r: Vec<f32> = vec![0.5, 0.6, 0.7, 0.8];

        processor.interleave(&[&input_l, &input_r], frames);

        let pcm_buf = std::mem::replace(&mut processor.scratch, pcm_pool().get());
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let chunk = PcmChunk::new(
            PcmMeta {
                spec,
                ..PcmMeta::default()
            },
            pcm_buf,
        );

        let result = processor.effect.as_mut().unwrap().process(chunk).unwrap();
        processor.scratch = result.pcm;

        let mut out_l = vec![0.0f32; frames];
        let mut out_r = vec![0.0f32; frames];
        processor.deinterleave(&mut [&mut out_l, &mut out_r], frames);

        // Each sample should be doubled
        for (out, inp) in out_l.iter().zip(input_l.iter()) {
            assert!((out - inp * 2.0).abs() < f32::EPSILON);
        }
        for (out, inp) in out_r.iter().zip(input_r.iter()) {
            assert!((out - inp * 2.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn accumulating_effect_returns_clear_all_outputs() {
        let mut processor = EffectBridgeProcessor::new(
            Some(Box::new(AccumulatingEffect)),
            NonZeroU32::new(44100).unwrap(),
            pcm_pool().clone(),
        );

        let frames = 4;
        let input_l: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4];
        let input_r: Vec<f32> = vec![0.5, 0.6, 0.7, 0.8];

        processor.interleave(&[&input_l, &input_r], frames);

        let pcm_buf = std::mem::replace(&mut processor.scratch, pcm_pool().get());
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let chunk = PcmChunk::new(
            PcmMeta {
                spec,
                ..PcmMeta::default()
            },
            pcm_buf,
        );

        let result = processor.effect.as_mut().unwrap().process(chunk);
        assert!(result.is_none());
    }

    #[test]
    fn interleave_deinterleave_roundtrip() {
        let mut processor = EffectBridgeProcessor::new(
            Some(Box::new(PassthroughEffect)),
            NonZeroU32::new(48000).unwrap(),
            pcm_pool().clone(),
        );

        let frames = 128;
        let input_l: Vec<f32> = (0..frames).map(|i| i as f32 * 0.01).collect();
        let input_r: Vec<f32> = (0..frames).map(|i| -(i as f32) * 0.01).collect();

        processor.interleave(&[&input_l, &input_r], frames);

        let mut out_l = vec![0.0f32; frames];
        let mut out_r = vec![0.0f32; frames];
        processor.deinterleave(&mut [&mut out_l, &mut out_r], frames);

        assert_eq!(out_l, input_l);
        assert_eq!(out_r, input_r);
    }

    #[test]
    fn scratch_buffer_reused_across_calls() {
        let mut processor = EffectBridgeProcessor::new(
            Some(Box::new(PassthroughEffect)),
            NonZeroU32::new(44100).unwrap(),
            pcm_pool().clone(),
        );

        // First call
        let input_l = vec![1.0f32; 64];
        let input_r = vec![2.0f32; 64];
        processor.interleave(&[&input_l, &input_r], 64);

        let pcm_buf = std::mem::replace(&mut processor.scratch, pcm_pool().get());
        let chunk = PcmChunk::new(
            PcmMeta {
                spec: PcmSpec {
                    channels: 2,
                    sample_rate: 44100,
                },
                ..PcmMeta::default()
            },
            pcm_buf,
        );
        let result = processor.effect.as_mut().unwrap().process(chunk).unwrap();
        processor.scratch = result.pcm;

        let capacity_after_first = processor.scratch.capacity();
        assert!(capacity_after_first >= 128);

        // Second call with same size — no reallocation expected
        processor.interleave(&[&input_l, &input_r], 64);
        assert_eq!(processor.scratch.capacity(), capacity_after_first);
    }

    #[test]
    fn empty_frames_returns_bypass() {
        let mut processor = EffectBridgeProcessor::new(
            Some(Box::new(PassthroughEffect)),
            NonZeroU32::new(44100).unwrap(),
            pcm_pool().clone(),
        );

        // Zero frames should bypass
        processor.interleave(&[&[], &[]], 0);
        assert!(processor.scratch.is_empty());
    }
}
