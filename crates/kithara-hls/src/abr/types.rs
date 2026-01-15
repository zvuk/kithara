use std::time::{Duration, Instant};

use crate::playlist::MasterPlaylist;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThroughputSampleSource {
    Network,
    Cache,
}

#[derive(Clone, Debug)]
pub struct ThroughputSample {
    pub bytes: u64,
    pub duration: Duration,
    pub at: Instant,
    pub source: ThroughputSampleSource,
}

#[derive(Clone, Debug)]
pub struct AbrConfig {
    pub min_buffer_for_up_switch_secs: f64,
    pub down_switch_buffer_secs: f64,
    pub throughput_safety_factor: f64,
    pub up_hysteresis_ratio: f64,
    pub down_hysteresis_ratio: f64,
    pub min_switch_interval: Duration,
    pub initial_variant_index: Option<usize>,
    pub sample_window: Duration,
}

impl Default for AbrConfig {
    fn default() -> Self {
        Self {
            min_buffer_for_up_switch_secs: 10.0,
            down_switch_buffer_secs: 5.0,
            throughput_safety_factor: 1.5,
            up_hysteresis_ratio: 1.3,
            down_hysteresis_ratio: 0.8,
            min_switch_interval: Duration::from_secs(30),
            initial_variant_index: Some(0),
            sample_window: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Variant {
    pub variant_index: usize,
    pub bandwidth_bps: u64,
}

pub fn variants_from_master(master: &MasterPlaylist) -> Vec<Variant> {
    master
        .variants
        .iter()
        .map(|v| Variant {
            variant_index: v.id.0,
            bandwidth_bps: v.bandwidth.unwrap_or(0),
        })
        .collect()
}
