use kithara_platform::time::Duration;
use kithara_stream::SourceSeekAnchor;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SeekEvents {
    #[default]
    Publish,
    Suppress,
}

impl SeekEvents {
    pub(crate) const fn should_publish(self) -> bool {
        matches!(self, Self::Publish)
    }
}

impl From<bool> for SeekEvents {
    fn from(application_visible: bool) -> Self {
        if application_visible {
            Self::Publish
        } else {
            Self::Suppress
        }
    }
}

/// Context for a pending seek, carried through multiple states.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SeekContext {
    pub(crate) target: Duration,
    pub(crate) epoch: u64,
    pub(crate) events: SeekEvents,
}

/// Stateful seek request carried across waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeekRequest {
    pub(crate) seek: SeekContext,
    pub(crate) emit_request: bool,
}

impl Default for SeekRequest {
    fn default() -> Self {
        Self {
            seek: SeekContext::default(),
            emit_request: true,
        }
    }
}

/// Seek application mode resolved before touching the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApplySeekState {
    pub(crate) mode: SeekMode,
    pub(crate) request: SeekRequest,
}

/// Resume state after a seek has been applied to the decoder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ResumeState {
    /// Anchor byte offset from the seek, used for readiness checks and demand.
    pub(crate) anchor_offset: Option<u64>,
    /// Variant that owns `anchor_offset`.
    pub(crate) anchor_variant_index: Option<usize>,
    pub(crate) skip: Option<Duration>,
    pub(crate) seek: SeekContext,
}

/// How the seek should be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekMode {
    /// Direct decoder seek. An estimated byte gates readiness when available.
    Direct { target_byte: Option<u64> },
    /// Anchor-based seek with segment alignment.
    Anchor(SourceSeekAnchor),
}
