use crossbeam_queue::ArrayQueue;
use kithara_decode::Decoder;
use kithara_platform::sync::Arc;
use kithara_stream::{MediaInfo, SeekObserve};

use crate::pipeline::seek::{SeekContext, SeekRequest};

/// What to do once decoder recreation succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecreateNext {
    /// Continue plain decoding from the new decoder.
    Decode,
    /// Re-run seek resolution on the recreated decoder.
    Seek(SeekRequest),
    /// Finish seek application by seeking the recreated decoder.
    ApplySeek(SeekRequest),
}

/// Decoder recreation task tracked by the FSM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecreateState {
    pub(crate) media_info: MediaInfo,
    pub(crate) cause: RecreateCause,
    pub(crate) next: RecreateNext,
    pub(crate) offset: u64,
}

pub(crate) struct RebuildState {
    pub(crate) completion: Arc<ArrayQueue<DecoderRebuildComplete>>,
    pub(crate) superseded_seek: Option<SeekRequest>,
    pub(crate) recreate: RecreateState,
    pub(crate) started_seek_epoch: u64,
    pub(crate) ticket: u64,
}

impl RebuildState {
    pub(crate) fn record_seek_preempt(&mut self, seek: &dyn SeekObserve, decode_epoch: u64) {
        if !seek.take_preempt() {
            return;
        }
        let epoch = seek.epoch();
        let min_epoch = self
            .superseded_seek
            .map_or(self.started_seek_epoch, |request| request.seek.epoch);
        if epoch <= min_epoch || epoch <= decode_epoch {
            return;
        }
        let Some(target) = seek.target() else {
            return;
        };
        self.superseded_seek = Some(SeekRequest {
            seek: SeekContext {
                target,
                epoch,
                events: (seek.pending_epoch() == Some(epoch)).into(),
            },
            emit_request: false,
        });
    }
}

pub(crate) struct DecoderRebuildComplete {
    pub(crate) result: Result<Box<dyn Decoder>, RecreateOutcome>,
    pub(crate) ticket: u64,
}

/// Outcome of one decoder recreation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecreateOutcome {
    Done,
    SoftFailed,
    NeedsSourceWait,
}

/// Why the decoder needs to be recreated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecreateCause {
    /// Codec boundary detected during playback.
    FormatBoundary,
    /// Host audio route changed the device sample rate.
    RouteChange,
    /// ABR switch changed the codec or variant.
    VariantSwitch,
}
