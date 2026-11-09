use audioadapter::{Adapter, AdapterMut};
use audioadapter_buffers::direct::InterleavedSlice;
use bevy_platform::time::Instant;
use firewheel::{backend::BackendProcessInfo, node::StreamStatus, processor::FirewheelProcessor};
use kithara_platform::time::Duration;
use ringbuf::{HeapCons, traits::Consumer};

/// Settles queued schedules at the old geometry before returning the actual processor.
pub(crate) struct SessionProcessor {
    processor: Option<FirewheelProcessor>,
    stop: HeapCons<()>,
    channels: usize,
    last: BackendProcessInfo,
}

impl SessionProcessor {
    pub(crate) fn new(processor: FirewheelProcessor, channels: usize, stop: HeapCons<()>) -> Self {
        Self {
            processor: Some(processor),
            stop,
            channels,
            last: BackendProcessInfo {
                frames: 0,
                process_timestamp: Some(Instant::now()),
                duration_since_stream_start: Duration::ZERO,
                input_stream_status: StreamStatus::empty(),
                output_stream_status: StreamStatus::empty(),
                dropped_frames: 0,
                process_to_playback_delay: None,
            },
        }
    }

    pub(crate) fn render(&mut self, output: &mut [f32], info: BackendProcessInfo) {
        let input = InterleavedSlice::new(&[] as &[f32], 0, 0)
            .expect("zero input frames have zero samples");
        let mut output = InterleavedSlice::new_mut(output, self.channels, info.frames)
            .expect("the admitted output contains complete interleaved frames");
        self.process(&input, &mut output, info);
    }

    pub(crate) fn process(
        &mut self,
        input: &dyn Adapter<f32>,
        output: &mut dyn AdapterMut<f32>,
        info: BackendProcessInfo,
    ) {
        if self.stop.try_pop().is_some() {
            self.settle();
            self.processor.take();
        }
        if let Some(processor) = &mut self.processor {
            self.last = info.clone();
            processor.process(input, output, info);
        } else {
            output.fill_frames_with(0, info.frames, &0.0);
        }
    }

    /// Retains the last real timestamp because process0 republishes the shared clock.
    fn settle(&mut self) {
        let Some(processor) = &mut self.processor else {
            return;
        };
        let input = InterleavedSlice::new(&[] as &[f32], 0, 0).expect("zero-frame input shape");
        let mut output =
            InterleavedSlice::new_mut(&mut [], self.channels, 0).expect("zero-frame output shape");
        let mut stopped = self.last.clone();
        stopped.frames = 0;
        stopped.input_stream_status = StreamStatus::empty();
        stopped.output_stream_status = StreamStatus::empty();
        stopped.dropped_frames = 0;
        processor.process(&input, &mut output, stopped);
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn is_running(&self) -> bool {
        self.processor.is_some()
    }
}

impl Drop for SessionProcessor {
    fn drop(&mut self) {
        self.settle();
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use firewheel::{ActivateInfo, FirewheelConfig, FirewheelContext};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::rt::MasterNode;

    #[kithara::test]
    fn stopping_settles_the_real_queued_schedule_without_moving_its_clock() {
        let mut context = FirewheelContext::new(FirewheelConfig::default());
        let mut shape = ActivateInfo {
            sample_rate: NonZeroU32::new(48_000).expect("rate"),
            max_block_frames: NonZeroU32::new(128).expect("block"),
            num_stream_in_channels: 0,
            num_stream_out_channels: 2,
            input_to_output_latency_seconds: 0.0,
        };
        let processor = context.activate(shape).expect("initial graph");
        let (_stop, stopped) = ringbuf::traits::Split::split(ringbuf::HeapRb::new(1));
        let mut actual = SessionProcessor::new(processor, 2, stopped);
        let mut info = actual.last.clone();
        info.frames = 4;
        actual.render(&mut [0.0; 8], info);
        let clock = context.heartbeat();
        let timestamp = context
            .audio_clock_instant()
            .expect("the running processor publishes its timestamp");
        let node = context
            .add_node(MasterNode, None)
            .expect("real master node");
        context
            .connect(node, context.graph_out_node_id(), &[(0, 0), (1, 1)], true)
            .expect("master route");
        context.update().expect("queued old-geometry schedule");
        drop(actual);
        assert_eq!(context.heartbeat(), clock);
        assert!(!context.is_active());
        assert!(context.audio_clock_instant().is_none());
        shape.max_block_frames = NonZeroU32::new(64).expect("new geometry");
        let processor = context.activate(shape).expect("same context restart");
        assert_eq!(context.heartbeat(), clock);
        assert_eq!(context.audio_clock_instant(), Some(timestamp));
        let (_stop, stopped) = ringbuf::traits::Split::split(ringbuf::HeapRb::new(1));
        let mut actual = SessionProcessor::new(processor, 2, stopped);
        let mut info = actual.last.clone();
        info.frames = 1;
        actual.render(&mut [0.0; 2], info);
        assert_eq!(
            context.heartbeat(),
            clock.checked_add(1).expect("one rendered frame")
        );
    }
}
