use std::num::NonZeroU32;

use kithara_audio::{PcmControl, PcmRead, PcmSession, ReadOutcome, SeekOutcome};
use kithara_decode::{DecodeError, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::{sync::Arc, time::Duration};

use crate::resource::Resource;

struct EmptyReader {
    bus: EventBus,
    metadata: TrackMetadata,
    spec: PcmSpec,
}

impl Default for EmptyReader {
    fn default() -> Self {
        Self {
            bus: EventBus::default(),
            metadata: TrackMetadata::default(),
            spec: PcmSpec::new(2, NonZeroU32::new(44_100).expect("static sample rate")),
        }
    }
}

impl PcmRead for EmptyReader {
    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Eof {
            position: Duration::ZERO,
        })
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Eof {
            position: Duration::ZERO,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn position(&self) -> Duration {
        Duration::ZERO
    }
}

impl PcmSession for EmptyReader {
    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }
}

impl PcmControl for EmptyReader {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }
}

pub(crate) fn empty_resource(src: &str) -> Resource {
    Resource::from_reader(EmptyReader::default(), Some(Arc::from(src)))
}
