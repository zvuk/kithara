use std::sync::atomic::AtomicU64;

use kithara_platform::sync::Arc;
use kithara_stream::{Activity, PlayheadWrite, SeekControl, SeekObserve, StreamType};

use crate::pipeline::{
    decode::{
        core::{DecodeCore, DecodeParts},
        gate::ReadinessGate,
        resume::ResumeCursor,
    },
    rebuild::port::{RebuildPort, RebuildRuntime},
    seek::SeekEngine,
    stream::shared::SharedStream,
};

pub(crate) struct SourceParts<T: StreamType> {
    pub(crate) activity: Arc<dyn Activity>,
    pub(crate) decode: DecodeCore,
    pub(crate) playhead: Arc<dyn PlayheadWrite>,
    pub(crate) readiness: ReadinessGate,
    pub(crate) rebuild: RebuildPort<T>,
    pub(crate) resume: ResumeCursor,
    pub(crate) seek: Arc<dyn SeekControl>,
    pub(crate) seek_engine: SeekEngine,
    pub(crate) seek_obs: Arc<dyn SeekObserve>,
}

impl<T: StreamType> SourceParts<T> {
    pub(crate) fn new(
        stream: &SharedStream<T>,
        decode: DecodeParts<T>,
        epoch: Arc<AtomicU64>,
        rebuild: RebuildRuntime,
    ) -> Self {
        let DecodeParts {
            core,
            factory,
            host_sample_rate,
            recreate_on_host_rate_change,
            decoder_host_sample_rate,
        } = decode;
        Self {
            activity: stream.activity(),
            decode: core,
            playhead: stream.playhead_write(),
            readiness: ReadinessGate::new(stream.peer_wake()),
            rebuild: RebuildPort::new(factory, rebuild),
            resume: ResumeCursor::new(
                host_sample_rate,
                recreate_on_host_rate_change,
                decoder_host_sample_rate,
            ),
            seek: stream.seek_control(),
            seek_engine: SeekEngine::new(epoch),
            seek_obs: stream.seek_observe(),
        }
    }
}
