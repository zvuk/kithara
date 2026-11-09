use std::num::NonZeroU32;

use bevy_platform::time::Instant;
use firewheel::{backend::BackendProcessInfo, cpal::cpal, node::StreamStatus};
use ringbuf::{HeapCons, traits::Consumer};

use crate::session::processor::SessionProcessor;

pub(super) struct OutputCallback {
    processor: Option<SessionProcessor>,
    incoming: HeapCons<SessionProcessor>,
    channels: usize,
    rate: NonZeroU32,
    started: Instant,
    previous: Option<Instant>,
    predicted: std::time::Duration,
}

impl OutputCallback {
    pub(super) fn new(
        incoming: HeapCons<SessionProcessor>,
        channels: usize,
        rate: NonZeroU32,
    ) -> Self {
        Self {
            processor: None,
            incoming,
            channels,
            rate,
            started: Instant::now(),
            previous: None,
            predicted: std::time::Duration::ZERO,
        }
    }

    /// The acquired paused stream receives its processor before its only play call.
    pub(super) fn process(&mut self, output: &mut [f32], callback: &cpal::OutputCallbackInfo) {
        let now = Instant::now();
        let frames = output.len() / self.channels;
        let elapsed = self.previous.map(|previous| now.duration_since(previous));
        let late = elapsed.is_some_and(|elapsed| elapsed > self.predicted);
        self.previous = Some(now);
        let rate = f64::from(self.rate.get());
        let frame_count = u32::try_from(frames).expect("CPAL callback frame count fits u32");
        self.predicted = std::time::Duration::from_secs_f64(f64::from(frame_count) / rate * 1.5);
        let timestamp = callback.timestamp();
        if self.processor.is_none() {
            self.processor = self.incoming.try_pop();
        }
        self.processor
            .as_mut()
            .expect("the acquired stream starts only after processor installation")
            .render(
                output,
                BackendProcessInfo {
                    frames,
                    process_timestamp: Some(now),
                    duration_since_stream_start: now.duration_since(self.started),
                    input_stream_status: StreamStatus::empty(),
                    output_stream_status: if late {
                        StreamStatus::OUTPUT_UNDERFLOW
                    } else {
                        StreamStatus::empty()
                    },
                    dropped_frames: if late {
                        (elapsed
                            .expect("late callback has a predecessor")
                            .as_secs_f64()
                            * rate)
                            .round() as u32
                    } else {
                        0
                    },
                    process_to_playback_delay: Some(
                        timestamp.playback.duration_since(timestamp.callback),
                    ),
                },
            );
    }
}
