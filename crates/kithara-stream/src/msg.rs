#![forbid(unsafe_code)]

use bytes::Bytes;
use tokio_util::sync::CancellationToken;

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

/// Engine parameters for orchestration.
#[derive(Debug, Clone)]
pub struct EngineParams {
    pub cmd_channel_capacity: usize,
    pub out_channel_capacity: usize,
    pub cancel: CancellationToken,
}

impl Default for EngineParams {
    fn default() -> Self {
        Self {
            cmd_channel_capacity: 16,
            out_channel_capacity: 32,
            cancel: CancellationToken::new(),
        }
    }
}
