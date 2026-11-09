use std::{num::NonZeroU32, time::Duration};

use bevy_platform::time::Instant;
use firewheel::{backend::BackendProcessInfo, node::StreamStatus};
use js_sys::{Array, Float32Array};
use triple_buffer::Output;
use wasm_bindgen::{JsCast, prelude::*};

use crate::session::processor::SessionProcessor;

#[derive(Clone)]
pub(super) struct ClockStamp {
    pub(super) main: Instant,
    pub(super) audio: f64,
}

/// Owns the same framework processor in the AudioWorklet realm.
#[wasm_bindgen]
pub struct HostOutput {
    processor: SessionProcessor,
    clock: Output<ClockStamp>,
    rate: NonZeroU32,
    interleaved: [f32; crate::consts::WEB_OUTPUT_SAMPLES],
    left: [f32; crate::consts::WEB_RENDER_QUANTUM],
    right: [f32; crate::consts::WEB_RENDER_QUANTUM],
}

impl HostOutput {
    pub(super) fn new(
        processor: SessionProcessor,
        clock: Output<ClockStamp>,
        rate: NonZeroU32,
    ) -> Self {
        Self {
            processor,
            clock,
            rate,
            interleaved: [0.0; crate::consts::WEB_OUTPUT_SAMPLES],
            left: [0.0; crate::consts::WEB_RENDER_QUANTUM],
            right: [0.0; crate::consts::WEB_RENDER_QUANTUM],
        }
    }
}

#[wasm_bindgen]
impl HostOutput {
    pub fn process(&mut self, channels: Array, audio_time: f64) -> Result<bool, JsValue> {
        let left = channels.get(0).dyn_into::<Float32Array>()?;
        let right = channels.get(1).dyn_into::<Float32Array>()?;
        if left.length() != right.length() {
            return Err(JsValue::from_str("web channel frame counts differ"));
        }
        let frames = usize::try_from(left.length())
            .map_err(|_| JsValue::from_str("web frame count overflow"))?;
        let mut offset = 0usize;
        while offset < frames {
            let count = frames
                .checked_sub(offset)
                .ok_or_else(|| JsValue::from_str("web cursor overflow"))?
                .min(crate::consts::WEB_RENDER_QUANTUM);
            let samples = count
                .checked_mul(2)
                .ok_or_else(|| JsValue::from_str("web sample count overflow"))?;
            let cursor =
                u32::try_from(offset).map_err(|_| JsValue::from_str("web cursor overflow"))?;
            let now = audio_time + f64::from(cursor) / f64::from(self.rate.get());
            let stamp = self.clock.read();
            let delta = now - stamp.audio;
            let distance = Duration::try_from_secs_f64(delta.abs())
                .map_err(|_| JsValue::from_str("invalid audio clock offset"))?;
            let timestamp = if delta < 0.0 {
                stamp.main.checked_sub(distance)
            } else {
                stamp.main.checked_add(distance)
            }
            .ok_or_else(|| JsValue::from_str("audio clock offset overflow"))?;
            let output = self
                .interleaved
                .get_mut(..samples)
                .ok_or_else(|| JsValue::from_str("unprepared web output shape"))?;
            self.processor.render(
                output,
                BackendProcessInfo {
                    frames: count,
                    process_timestamp: Some(timestamp),
                    duration_since_stream_start: Duration::try_from_secs_f64(now)
                        .map_err(|_| JsValue::from_str("invalid audio context time"))?,
                    input_stream_status: StreamStatus::empty(),
                    output_stream_status: StreamStatus::empty(),
                    dropped_frames: 0,
                    process_to_playback_delay: None,
                },
            );
            for ((left, right), sample) in self
                .left
                .iter_mut()
                .zip(&mut self.right)
                .zip(output.chunks_exact(2))
            {
                *left = *sample.first().expect("a stereo chunk has its left sample");
                *right = *sample.get(1).expect("a stereo chunk has its right sample");
            }
            let end = cursor
                .checked_add(
                    u32::try_from(count).map_err(|_| JsValue::from_str("web chunk overflow"))?,
                )
                .ok_or_else(|| JsValue::from_str("web chunk overflow"))?;
            let left_prefix = self
                .left
                .get(..count)
                .ok_or_else(|| JsValue::from_str("web left prefix overflow"))?;
            let right_prefix = self
                .right
                .get(..count)
                .ok_or_else(|| JsValue::from_str("web right prefix overflow"))?;
            if cursor == 0 && end == left.length() {
                left.copy_from(left_prefix);
                right.copy_from(right_prefix);
            } else {
                left.subarray(cursor, end).copy_from(left_prefix);
                right.subarray(cursor, end).copy_from(right_prefix);
            }
            offset = usize::try_from(end).map_err(|_| JsValue::from_str("web cursor overflow"))?;
        }
        Ok(self.processor.is_running())
    }
}
