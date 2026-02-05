//! Symphonia-based audio decoder backend.
//!
//! This module provides the [`SymphoniaConfig`] for configuring Symphonia decoders.

// Types are not yet exported but will be used in later tasks
#![allow(dead_code)]

use std::sync::{Arc, atomic::AtomicU64};

/// Configuration for Symphonia-based decoders.
#[derive(Debug, Clone)]
pub struct SymphoniaConfig {
    /// Enable data verification (slower but safer).
    pub verify: bool,
    /// Enable gapless playback.
    pub gapless: bool,
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
}

impl Default for SymphoniaConfig {
    fn default() -> Self {
        Self {
            verify: false,
            gapless: true,
            byte_len_handle: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symphonia_config_default() {
        let config = SymphoniaConfig::default();
        assert!(!config.verify);
        assert!(config.gapless);
        assert!(config.byte_len_handle.is_none());
    }
}
