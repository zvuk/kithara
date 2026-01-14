use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use thiserror::Error;
use tokio::sync::broadcast;
use url::Url;

use crate::{HlsError, abr::AbrReason, playlist::SegmentKey};

/// Events emitted by pipeline layers.
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// Variant switch applied (base layer started emitting new variant).
    VariantApplied {
        from: usize,
        to: usize,
        reason: AbrReason,
    },
    /// Segment ready to be yielded from current layer.
    SegmentReady {
        variant: usize,
        segment_index: usize,
    },
    /// Segment successfully decrypted.
    Decrypted {
        variant: usize,
        segment_index: usize,
    },
    /// Segment placed in prefetch buffer.
    Prefetched {
        variant: usize,
        segment_index: usize,
    },
}

/// Segment metadata.
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    pub variant: usize,
    pub segment_index: usize,
    pub sequence: u64,
    pub url: Url,
    pub duration: Option<Duration>,
    pub key: Option<SegmentKey>,
}

/// Payload passed between pipeline layers.
#[derive(Debug, Clone)]
pub struct SegmentPayload {
    pub meta: SegmentMeta,
    pub bytes: Bytes,
}

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("hls error: {0}")]
    Hls(#[from] HlsError),

    #[error("pipeline aborted")]
    Aborted,
}

pub type PipelineResult<T> = Result<T, PipelineError>;

/// Trait for pipeline layer: segment stream with access to event channel.
pub trait PipelineStream: Stream<Item = PipelineResult<SegmentPayload>> + Send + 'static {
    fn event_sender(&self) -> broadcast::Sender<PipelineEvent>;
}
