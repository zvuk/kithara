use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use hls_m3u8::{MasterPlaylist, tags::VariantStream};

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

pub type VariantSelector =
    Arc<dyn Fn(&MasterPlaylist<'_>) -> Option<usize> + Send + Sync + 'static>;

pub fn variants_from_master(master: &MasterPlaylist<'_>) -> Vec<Variant> {
    master
        .variant_streams
        .iter()
        .enumerate()
        .filter_map(|(variant_index, stream)| match stream {
            VariantStream::ExtXStreamInf { .. } => Some(Variant {
                variant_index,
                bandwidth_bps: stream.bandwidth(),
            }),
            VariantStream::ExtXIFrame { .. } => None,
        })
        .collect()
}
