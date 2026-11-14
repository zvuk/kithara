use std::fmt;

use kithara_abr::AbrHandle;
use kithara_decode::TrackMetadata;

use super::PcmReceiver;

/// Off-RT load result; only the receiver transfers to the audio thread.
pub struct OpenedPcm {
    pub receiver: PcmReceiver,
    pub metadata: TrackMetadata,
    pub abr: Option<AbrHandle>,
}

impl fmt::Debug for OpenedPcm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenedPcm")
            .field("receiver", &self.receiver)
            .finish_non_exhaustive()
    }
}
