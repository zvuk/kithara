use core::{
    num::{NonZeroU32, NonZeroUsize},
    sync::atomic::Ordering,
};

use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    event::ProcEvents,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_platform::sync::Mutex;
use kithara_test_utils::kithara;
use ringbuf::traits::{Observer, Producer};

use crate::bridge::MixTapWriter;

pub(crate) struct TapNode {
    writer: Mutex<Option<MixTapWriter>>,
}

impl TapNode {
    pub(crate) fn new(writer: MixTapWriter) -> Self {
        Self {
            writer: Mutex::new(Some(writer)),
        }
    }
}

impl AudioNode for TapNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        TapProcessor::new(self.writer.lock().take(), cx.stream_info.sample_rate)
    }

    fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name("session_mix_tap")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::STEREO,
                num_outputs: ChannelCount::ZERO,
            })
    }
}

struct TapProcessor {
    writer: Option<MixTapWriter>,
    sample_rate: NonZeroU32,
}

impl TapProcessor {
    const STEREO: NonZeroUsize = NonZeroUsize::new(2).expect("2 is non-zero");

    fn new(writer: Option<MixTapWriter>, sample_rate: NonZeroU32) -> Self {
        Self {
            writer,
            sample_rate,
        }
    }

    fn adopt_rate(&mut self, sample_rate: NonZeroU32) {
        if sample_rate != self.sample_rate {
            self.writer = None;
            self.sample_rate = sample_rate;
        }
    }
}

impl AudioNodeProcessor for TapProcessor {
    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.adopt_rate(stream_info.sample_rate);
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        _events: &mut ProcEvents,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let Some(ref mut writer) = self.writer else {
            return ProcessStatus::ClearAllOutputs;
        };
        let stereo = Self::STEREO.get();
        let [left, right, ..] = buffers.inputs else {
            writer
                .drops
                .fetch_add(dropped_samples(info.frames, 0, stereo), Ordering::Relaxed);
            return ProcessStatus::ClearAllOutputs;
        };
        // Frame-aligned pushes keep the consumer's channel order intact under
        // overflow: a lost half-frame would swap L and R for good.
        let frames = info
            .frames
            .min(writer.pcm.vacant_len() / stereo)
            .min(left.len())
            .min(right.len());
        let pushed = writer
            .pcm
            .push_iter((0..frames).flat_map(|frame| [left[frame], right[frame]]));
        let dropped = dropped_samples(info.frames, pushed, stereo);
        if dropped > 0 {
            writer.drops.fetch_add(dropped, Ordering::Relaxed);
        }

        ProcessStatus::ClearAllOutputs
    }
}

fn dropped_samples(frames: usize, pushed: usize, stereo: usize) -> u64 {
    u64::try_from(frames * stereo - pushed).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicU64;

    use kithara_platform::sync::Arc;
    use ringbuf::{
        HeapRb,
        traits::{Observer, Split},
    };

    use super::*;

    fn rate(hz: u32) -> NonZeroU32 {
        NonZeroU32::new(hz).expect("test rate is non-zero")
    }

    #[kithara::test]
    fn a_changed_device_rate_ends_the_feed() {
        const CAPACITY: usize = 64;

        let (pcm, cons) = HeapRb::<f32>::new(CAPACITY).split();
        let writer = MixTapWriter::new(pcm, Arc::new(AtomicU64::new(0)));
        let mut processor = TapProcessor::new(Some(writer), rate(44_100));

        processor.adopt_rate(rate(44_100));
        assert!(cons.write_is_held(), "an equal rate keeps the feed running");

        processor.adopt_rate(rate(48_000));
        assert!(
            !cons.write_is_held(),
            "a rate change must end the feed rather than relabel the samples"
        );
    }
}
