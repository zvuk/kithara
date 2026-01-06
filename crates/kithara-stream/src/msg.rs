#![forbid(unsafe_code)]

use bytes::Bytes;

/// Legacy data/control multiplexing used by the legacy `Stream` orchestrator.
///
/// This is kept for transitional compatibility while higher-level crates migrate to the new
/// `StreamMsg<C, E>` model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message<M> {
    Data(Bytes),
    Control(M),
}

/// Generic "data + control + events" message model.
///
/// - `C`: in-band control messages that the consumer must observe (e.g. HLS variant change,
///   discontinuity, init boundary).
/// - `E`: observability/debug events (e.g. fetch progress, cache hits, wait stalls).
///
/// Both `C` and `E` are defined by higher-level crates (`kithara-file`, `kithara-hls`) and are
/// fully generic here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamMsg<C, E> {
    Data(Bytes),
    Control(C),
    Event(E),
}

/// Shared parameters for stream creation.
///
/// `offline_mode` is included because it is a shared concern for file and HLS flows.
/// Enforcement is delegated to the concrete source implementation (e.g. cache-only, no network).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParams {
    pub offline_mode: bool,
}

impl Default for StreamParams {
    fn default() -> Self {
        Self {
            offline_mode: false,
        }
    }
}
