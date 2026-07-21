use std::collections::VecDeque;

use kithara_platform::time::Instant;
use kithara_ui::{
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    render::{ReadValue, StereoLevels, WaveBucket, WaveformView},
};
use num_traits::cast::AsPrimitive;

use crate::mock::MockRegistry;

struct Consts;

impl Consts {
    const FRAME_WINDOW: usize = 300;
    const WAVE_BUCKETS: u16 = 8_192;
}

pub(super) struct StressState {
    fader: f64,
    frame_ms: VecDeque<f64>,
    frame_ms_avg: String,
    frame_ms_ordered: Vec<f64>,
    frame_ms_p99: String,
    fps: String,
    last_tick: Option<Instant>,
    levels: [StereoLevels; 8],
    phase: f32,
    waveforms: [Vec<WaveBucket>; 4],
}

impl Default for StressState {
    fn default() -> Self {
        Self {
            fader: 0.7,
            frame_ms: VecDeque::with_capacity(Consts::FRAME_WINDOW),
            frame_ms_avg: "--".to_owned(),
            frame_ms_ordered: Vec::with_capacity(Consts::FRAME_WINDOW),
            frame_ms_p99: "--".to_owned(),
            fps: "--".to_owned(),
            last_tick: None,
            levels: [StereoLevels::default(); 8],
            phase: 0.0,
            waveforms: std::array::from_fn(stress_waveform),
        }
    }
}

impl StressState {
    pub(super) fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let value = match endpoint {
            "bench.fps" => ReadValue::Text(&self.fps),
            "bench.frame_ms_avg" => ReadValue::Text(&self.frame_ms_avg),
            "bench.frame_ms_p99" => ReadValue::Text(&self.frame_ms_p99),
            "bench.fader" => ReadValue::Scalar(self.fader),
            "bench.wave.0" => waveform_value(&self.waveforms[0]),
            "bench.wave.1" => waveform_value(&self.waveforms[1]),
            "bench.wave.2" => waveform_value(&self.waveforms[2]),
            "bench.wave.3" => waveform_value(&self.waveforms[3]),
            "bench.level.0" => ReadValue::Stereo(self.levels[0]),
            "bench.level.1" => ReadValue::Stereo(self.levels[1]),
            "bench.level.2" => ReadValue::Stereo(self.levels[2]),
            "bench.level.3" => ReadValue::Stereo(self.levels[3]),
            "bench.level.4" => ReadValue::Stereo(self.levels[4]),
            "bench.level.5" => ReadValue::Stereo(self.levels[5]),
            "bench.level.6" => ReadValue::Stereo(self.levels[6]),
            "bench.level.7" => ReadValue::Stereo(self.levels[7]),
            _ => return None,
        };
        Some(value)
    }

    pub(super) fn reset_clock(&mut self) {
        self.last_tick = None;
    }

    pub(super) fn set_scalar(&mut self, path: &str, value: f64) -> bool {
        if path != "stress/master" {
            return false;
        }
        self.fader = value.clamp(0.0, 1.0);
        true
    }

    pub(super) fn tick(&mut self) {
        let now = Instant::now();
        if let Some(previous) = self.last_tick.replace(now) {
            self.record_frame(now.duration_since(previous).as_secs_f64() * 1_000.0);
        }
        self.push_data();
    }

    fn push_data(&mut self) {
        self.phase += 0.037;
        for (index, waveform) in self.waveforms.iter_mut().enumerate() {
            waveform.rotate_left(1);
            let offset = u16::try_from(index).map_or(0.0, f32::from);
            if let Some(bucket) = waveform.last_mut() {
                *bucket = stress_bucket(self.phase + offset * 0.71);
            }
        }
        for (index, levels) in self.levels.iter_mut().enumerate() {
            let offset = u16::try_from(index).map_or(0.0, f32::from);
            let carrier = (self.phase * 2.3 + offset * 0.47).sin();
            let noise = (self.phase * 31.7 + offset * 7.13).sin();
            levels.l = (carrier.mul_add(0.32, noise * 0.08 + 0.54)).clamp(0.0, 1.0);
            levels.r = ((carrier + 0.63).sin().mul_add(0.3, noise * 0.09 + 0.5)).clamp(0.0, 1.0);
            levels.volume = self.fader.as_();
        }
    }

    fn record_frame(&mut self, frame_ms: f64) {
        if self.frame_ms.len() == Consts::FRAME_WINDOW {
            self.frame_ms.pop_front();
        }
        self.frame_ms.push_back(frame_ms);
        let count = u32::try_from(self.frame_ms.len()).map_or(1.0, f64::from);
        let average = self.frame_ms.iter().sum::<f64>() / count;
        self.frame_ms_ordered.clear();
        self.frame_ms_ordered.extend(self.frame_ms.iter().copied());
        self.frame_ms_ordered.sort_by(f64::total_cmp);
        let percentile = self
            .frame_ms_ordered
            .len()
            .saturating_mul(99)
            .div_ceil(100)
            .saturating_sub(1);
        let Some(p99) = self.frame_ms_ordered.get(percentile).copied() else {
            return;
        };
        self.fps = format!("{:.1}", 1_000.0 / average);
        self.frame_ms_avg = format!("{average:.2}");
        self.frame_ms_p99 = format!("{p99:.2}");
    }
}

pub(super) fn insert_endpoints(registry: &mut MockRegistry) {
    for id in ["bench.fps", "bench.frame_ms_avg", "bench.frame_ms_p99"] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Text),
        );
    }
    registry.insert(
        EndpointCategory::Model,
        "bench.fader",
        EndpointDesc::new(ValueKind::Scalar),
    );
    for index in 0..4 {
        registry.insert(
            EndpointCategory::Model,
            &format!("bench.wave.{index}"),
            EndpointDesc::new(ValueKind::Waveform),
        );
    }
    for index in 0..8 {
        registry.insert(
            EndpointCategory::Model,
            &format!("bench.level.{index}"),
            EndpointDesc::new(ValueKind::Stereo),
        );
    }
}

fn stress_bucket(phase: f32) -> WaveBucket {
    WaveBucket {
        low: phase.sin().mul_add(0.34, 0.52).clamp(0.0, 1.0),
        mid: (phase * 1.73).sin().mul_add(0.29, 0.45).clamp(0.0, 1.0),
        high: (phase * 3.11).sin().mul_add(0.2, 0.34).clamp(0.0, 1.0),
    }
}

fn stress_waveform(index: usize) -> Vec<WaveBucket> {
    let offset = u16::try_from(index).map_or(0.0, f32::from);
    (0..Consts::WAVE_BUCKETS)
        .map(|bucket| stress_bucket(f32::from(bucket).mul_add(0.013, offset)))
        .collect()
}

fn waveform_value(waveform: &[WaveBucket]) -> ReadValue<'_> {
    ReadValue::Waveform(WaveformView {
        buckets: waveform,
        beats: &[],
        downbeats: &[],
        bpm: None,
        r#loop: None,
        cues: &[],
    })
}
