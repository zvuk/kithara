use std::collections::VecDeque;

use kithara_platform::time::Instant;
use kithara_ui::{
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    render::{ReadValue, StereoLevels, WaveBucket, WaveformView},
};
use num_traits::cast::AsPrimitive;

use crate::demo::DemoRegistry;

mod consts {
    pub(super) const FRAME_WINDOW: usize = 300;
    pub(super) const WAVE_BUCKETS: u16 = 8_192;
}

pub(crate) struct StressState {
    last_tick: Option<Instant>,
    fps: String,
    frame_ms_avg: String,
    frame_ms_p99: String,
    frame_ms_ordered: Vec<f64>,
    frame_ms: VecDeque<f64>,
    levels: [StereoLevels; 8],
    waveforms: [Vec<WaveBucket>; 4],
    phase: f32,
    fader: f64,
    /// What the application calls the buckets it is handing over. A host holds
    /// the last copy it took and reads this name to tell a waveform it already
    /// has from one it has to take again, so buckets that moved under a name
    /// that did not are buckets the page never redraws.
    revision: u64,
}

impl Default for StressState {
    fn default() -> Self {
        Self::new(consts::WAVE_BUCKETS)
    }
}

impl StressState {
    /// The stress page carrying waveforms of `buckets` samples each.
    ///
    /// The count is a parameter because it is the page's own weight: a
    /// measurement that sweeps it separates a frame slow from drawing from a
    /// frame slow from folding buckets into the same columns.
    pub(crate) fn new(buckets: u16) -> Self {
        Self {
            fader: 0.7,
            frame_ms: VecDeque::with_capacity(consts::FRAME_WINDOW),
            frame_ms_avg: "--".to_owned(),
            frame_ms_ordered: Vec::with_capacity(consts::FRAME_WINDOW),
            frame_ms_p99: "--".to_owned(),
            fps: "--".to_owned(),
            last_tick: None,
            levels: [StereoLevels::default(); 8],
            phase: 0.0,
            revision: 0,
            waveforms: std::array::from_fn(|index| stress_waveform(index, buckets)),
        }
    }

    pub(crate) fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let value = match endpoint {
            "bench.fps" => ReadValue::Text(&self.fps),
            "bench.frame_ms_avg" => ReadValue::Text(&self.frame_ms_avg),
            "bench.frame_ms_p99" => ReadValue::Text(&self.frame_ms_p99),
            "bench.fader" => ReadValue::Scalar(self.fader),
            "bench.wave.0" => waveform_value(&self.waveforms[0], self.revision),
            "bench.wave.1" => waveform_value(&self.waveforms[1], self.revision),
            "bench.wave.2" => waveform_value(&self.waveforms[2], self.revision),
            "bench.wave.3" => waveform_value(&self.waveforms[3], self.revision),
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

    fn push_data(&mut self) {
        self.phase += 0.037;
        self.revision = self.revision.wrapping_add(1);
        for (index, waveform) in self.waveforms.iter_mut().enumerate() {
            waveform.rotate_left(1);
            let offset = u16::try_from(index).map_or(0.0, f32::from);
            if let Some(bucket) = waveform.last_mut() {
                *bucket = stress_bucket(self.phase + offset * 0.71);
            }
        }
        for (index, levels) in self.levels.iter_mut().enumerate() {
            let offset = u16::try_from(index).map_or(0.0, f32::from);
            let carrier = self.phase.mul_add(2.3, offset * 0.47).sin();
            let noise = self.phase.mul_add(31.7, offset * 7.13).sin();
            levels.l = (carrier.mul_add(0.32, noise * 0.08 + 0.54)).clamp(0.0, 1.0);
            levels.r = ((carrier + 0.63).sin().mul_add(0.3, noise * 0.09 + 0.5)).clamp(0.0, 1.0);
            levels.volume = self.fader.as_();
        }
    }

    fn record_frame(&mut self, frame_ms: f64) {
        if self.frame_ms.len() == consts::FRAME_WINDOW {
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

    pub(crate) const fn reset_clock(&mut self) {
        self.last_tick = None;
    }

    pub(crate) fn set_scalar(&mut self, path: &str, value: f64) -> bool {
        if path != "stress/master" {
            return false;
        }
        self.fader = value.clamp(0.0, 1.0);
        true
    }

    pub(crate) fn tick(&mut self) {
        let now = Instant::now();
        if let Some(previous) = self.last_tick.replace(now) {
            self.record_frame(now.duration_since(previous).as_secs_f64() * 1_000.0);
        }
        self.push_data();
    }
}

pub(crate) fn insert_endpoints(registry: &mut DemoRegistry) {
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

fn stress_waveform(index: usize, buckets: u16) -> Vec<WaveBucket> {
    let offset = u16::try_from(index).map_or(0.0, f32::from);
    (0..buckets)
        .map(|bucket| stress_bucket(f32::from(bucket).mul_add(0.013, offset)))
        .collect()
}

const fn waveform_value(waveform: &[WaveBucket], revision: u64) -> ReadValue<'_> {
    ReadValue::Waveform(WaveformView {
        revision,
        buckets: waveform,
        beats: &[],
        downbeats: &[],
        unready: &[],
        bpm: None,
        r#loop: None,
        cues: &[],
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::render::ReadValue;

    use super::StressState;

    fn revision(state: &StressState) -> u64 {
        match state.get("bench.wave.0") {
            Some(ReadValue::Waveform(view)) => view.revision,
            other => panic!("the stress page must hand over a waveform, not {other:?}"),
        }
    }

    #[kithara::test]
    fn moving_the_buckets_gives_them_a_name_they_did_not_have() {
        let mut stress = StressState::default();
        let first = revision(&stress);

        stress.tick();

        assert_ne!(
            revision(&stress),
            first,
            "the buckets moved under the name they already had, so a host holding the last copy \
             keeps drawing it"
        );
    }
}
