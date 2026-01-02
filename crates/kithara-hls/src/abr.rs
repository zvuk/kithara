use hls_m3u8::MasterPlaylist;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

use crate::{HlsError, HlsResult};

#[derive(Debug, Error)]
pub enum AbrError {
    #[error("No suitable variant found")]
    NoSuitableVariant,

    #[error("Invalid variant index: {0}")]
    InvalidVariantIndex(usize),

    #[error("Insufficient data for ABR decision")]
    InsufficientData,
}

#[derive(Clone, Debug)]
pub struct AbrConfig {
    pub min_buffer_for_up_switch: f32,
    pub down_switch_buffer: f32,
    pub throughput_safety_factor: f32,
    pub up_hysteresis_ratio: f32,
    pub down_hysteresis_ratio: f32,
    pub min_switch_interval: Duration,
    pub initial_variant_index: Option<usize>,
}

impl Default for AbrConfig {
    fn default() -> Self {
        Self {
            min_buffer_for_up_switch: 10.0,
            down_switch_buffer: 5.0,
            throughput_safety_factor: 1.5,
            up_hysteresis_ratio: 1.3,
            down_hysteresis_ratio: 0.8,
            min_switch_interval: Duration::from_secs(30),
            initial_variant_index: Some(0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ThroughputSample {
    pub bytes_transferred: u64,
    pub duration: Duration,
    pub timestamp: Instant,
}

#[derive(Clone, Debug)]
pub struct AbrState {
    current_variant: usize,
    last_switch: Option<Instant>,
    samples: Vec<ThroughputSample>,
    buffer_level: f32,
}

impl AbrState {
    pub fn new(initial_variant: usize) -> Self {
        Self {
            current_variant: initial_variant,
            last_switch: None,
            samples: Vec::new(),
            buffer_level: 0.0,
        }
    }

    pub fn add_throughput_sample(&mut self, sample: ThroughputSample) {
        self.samples.push(sample);

        // Keep only recent samples (last 30 seconds)
        let cutoff = Instant::now() - Duration::from_secs(30);
        self.samples.retain(|s| s.timestamp > cutoff);
    }

    pub fn update_buffer_level(&mut self, level: f32) {
        self.buffer_level = level;
    }

    pub fn current_variant(&self) -> usize {
        self.current_variant
    }

    fn estimated_throughput(&self) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }

        let total_bytes: u64 = self.samples.iter().map(|s| s.bytes_transferred).sum();
        let total_duration: Duration = self
            .samples
            .iter()
            .map(|s| s.duration)
            .reduce(|acc, dur| acc + dur)
            .unwrap_or(Duration::ZERO);

        if total_duration == Duration::ZERO {
            return None;
        }

        Some(total_bytes as f64 / total_duration.as_secs_f64())
    }

    fn can_switch_now(&self) -> bool {
        match self.last_switch {
            None => true,
            Some(last) => Instant::now().duration_since(last) >= Duration::from_secs(30),
        }
    }
}

pub struct AbrController {
    config: AbrConfig,
    variant_selector: Option<Arc<dyn Fn(&MasterPlaylist) -> Option<usize> + Send + Sync>>,
    state: AbrState,
}

impl AbrController {
    pub fn new(
        config: AbrConfig,
        variant_selector: Option<Arc<dyn Fn(&MasterPlaylist) -> Option<usize> + Send + Sync>>,
        initial_variant: usize,
    ) -> Self {
        Self {
            config,
            variant_selector,
            state: AbrState::new(initial_variant),
        }
    }

    pub fn select_variant(&mut self, master_playlist: &MasterPlaylist) -> HlsResult<usize> {
        // Manual override takes precedence
        if let Some(ref selector) = self.variant_selector {
            if let Some(manual_variant) = selector(master_playlist) {
                return Ok(manual_variant);
            }
        }

        // Auto ABR logic
        self.auto_select_variant(master_playlist)
    }

    pub fn add_throughput_sample(&mut self, sample: ThroughputSample) {
        self.state.add_throughput_sample(sample);
    }

    pub fn update_buffer_level(&mut self, level: f32) {
        self.state.update_buffer_level(level);
    }

    pub fn current_variant(&self) -> usize {
        self.state.current_variant()
    }

    pub fn set_manual_variant(&mut self, variant: usize) -> HlsResult<()> {
        // TODO: Implement proper validation when master playlist is available
        self.state.current_variant = variant;
        self.state.last_switch = Some(Instant::now());
        Ok(())
    }

    pub fn clear_manual_override(&mut self) {
        self.variant_selector = None;
    }

    fn auto_select_variant(&mut self, _master_playlist: &MasterPlaylist) -> HlsResult<usize> {
        let current = self.state.current_variant();
        let _throughput = self
            .state
            .estimated_throughput()
            .ok_or_else(|| HlsError::Abr("Insufficient throughput data".to_string()))?;

        // TODO: Implement proper ABR logic when master playlist is available
        // For now, return current variant
        Ok(current)
    }

    // TODO: Implement ABR helper methods when master playlist is available
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn test_throughput_estimation() {
        let mut state = AbrState::new(0);

        let sample1 = ThroughputSample {
            bytes_transferred: 1000,
            duration: Duration::from_millis(100),
            timestamp: Instant::now(),
        };

        state.add_throughput_sample(sample1);

        let throughput = state.estimated_throughput();
        assert!(throughput.is_some());
        assert_eq!(throughput.unwrap(), 10000.0); // 1000 bytes / 0.1 sec
    }

    #[test]
    fn test_buffer_level_updates() {
        let mut state = AbrState::new(0);

        assert_eq!(state.buffer_level, 0.0);

        state.update_buffer_level(15.0);
        assert_eq!(state.buffer_level, 15.0);
    }

    #[test]
    fn test_can_switch_now_timing() {
        let mut state = AbrState::new(0);

        // Initially can switch
        assert!(state.can_switch_now());

        // After switching, need to wait
        state.last_switch = Some(Instant::now());
        assert!(!state.can_switch_now());
    }

    #[test]
    fn test_sample_retention() {
        let mut state = AbrState::new(0);

        // Add an old sample
        let old_sample = ThroughputSample {
            bytes_transferred: 1000,
            duration: Duration::from_millis(100),
            timestamp: Instant::now() - Duration::from_secs(60),
        };

        // Add a recent sample
        let recent_sample = ThroughputSample {
            bytes_transferred: 2000,
            duration: Duration::from_millis(200),
            timestamp: Instant::now(),
        };

        state.add_throughput_sample(old_sample);
        state.add_throughput_sample(recent_sample);

        // Should only keep recent sample
        assert_eq!(state.samples.len(), 1);
        assert_eq!(state.samples[0].bytes_transferred, 2000);
    }
}
