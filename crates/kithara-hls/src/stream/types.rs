//! Stream types and state management.

use std::time::Duration;

use thiserror::Error;
use url::Url;

use crate::{HlsError, abr::AbrReason, playlist::SegmentKey};

/// Segment metadata (data is on disk, not in memory).
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    pub variant: usize,
    pub segment_index: usize,
    pub sequence: u64,
    pub url: Url,
    pub duration: Option<Duration>,
    pub key: Option<SegmentKey>,
    /// Segment size in bytes.
    pub len: u64,
}

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("hls error: {0}")]
    Hls(#[from] HlsError),

    #[error("pipeline aborted")]
    Aborted,
}

pub type PipelineResult<T> = Result<T, PipelineError>;

/// Commands for stream control.
#[derive(Debug)]
pub enum StreamCommand {
    Seek { segment_index: usize },
    ForceVariant { variant_index: usize, from: usize },
}

/// Variant switch state: tracks current variant and switch decisions.
#[derive(Clone)]
pub struct VariantSwitch {
    pub from: usize,
    pub to: usize,
    pub start_segment: usize,
    pub reason: AbrReason,
}

impl VariantSwitch {
    pub fn new(variant: usize) -> Self {
        Self {
            from: variant,
            to: variant,
            start_segment: 0,
            reason: AbrReason::Initial,
        }
    }

    pub fn with_seek(current_to: usize, segment_index: usize) -> Self {
        Self {
            from: current_to,
            to: current_to,
            start_segment: segment_index,
            reason: AbrReason::ManualOverride,
        }
    }

    pub fn with_force_variant(variant_index: usize, from: usize) -> Self {
        Self {
            from,
            to: variant_index,
            start_segment: 0,
            reason: AbrReason::ManualOverride,
        }
    }

    pub fn with_abr_switch(
        from: usize,
        to: usize,
        start_segment: usize,
        reason: AbrReason,
    ) -> Self {
        Self {
            from,
            to,
            start_segment,
            reason,
        }
    }

    pub fn apply_seek(&mut self, segment_index: usize) {
        self.start_segment = segment_index;
    }

    pub fn apply_force_variant(&mut self, variant_index: usize, from: usize) {
        self.from = from;
        self.to = variant_index;
        self.start_segment = 0;
        self.reason = AbrReason::ManualOverride;
    }
}
