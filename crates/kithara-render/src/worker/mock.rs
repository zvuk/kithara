use std::num::NonZeroU32;

use kithara_events::{EventReceiver, EventSet};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, SegmentId, SourceSpan};
use ringbuf::traits::{Consumer, Producer};

use super::{
    PcmPacket, PcmReceiver, packet_mock::PacketRing, reader::PcmProducer, scheduler::StreamWake,
};
use crate::test_pools::{pools, sample_buffer};

pub(crate) struct PcmFixture {
    pub(crate) receiver: Option<PcmReceiver>,
    pub(in crate::worker) producer: Option<PcmProducer>,
}

impl PcmFixture {
    pub(crate) async fn new(capacity: usize, blocking: bool) -> Self {
        Self::with_wake(
            capacity,
            blocking,
            StreamWake::new(kithara_worker::Wake::default()),
        )
        .await
    }

    pub(crate) async fn with_wake(capacity: usize, blocking: bool, wake: StreamWake) -> Self {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let (receiver, producer) = PacketRing::with_wake(
            spec,
            Duration::from_secs(1),
            capacity,
            capacity + 2,
            blocking,
            wake,
        )
        .into_ends();
        Self {
            receiver: Some(receiver),
            producer: Some(producer),
        }
    }

    pub(crate) fn push(&mut self, packet: PcmPacket) -> Result<(), PcmPacket> {
        let producer = self.producer.as_mut().expect("live producer");
        producer.forward.try_push(packet)?;
        producer.signal();
        Ok(())
    }

    pub(crate) fn flush(&mut self) {
        self.producer.as_mut().expect("producer").flush();
    }

    pub(crate) fn events<E: EventSet>(&self) -> EventReceiver<E> {
        self.producer.as_ref().expect("producer").events.subscribe()
    }

    pub(crate) fn returned(&mut self) -> Option<PcmPacket> {
        self.producer
            .as_mut()
            .expect("live producer")
            .reverse
            .try_pop()
    }

    pub(crate) fn close(&mut self) {
        self.producer = None;
    }
}

pub(crate) fn chunk(segment: SegmentId, samples: &[f32]) -> AudioChunk {
    let rate = NonZeroU32::new(48_000).expect("test rate");
    let frames = u32::try_from(samples.len()).expect("test chunk length");
    AudioChunk::new(
        AudioChunkInfo {
            spec: AudioSpec::new(1, rate),
            frames,
            segment,
            source_span: SourceSpan::new(0, u64::from(frames), rate, u64::from(frames)),
            ..AudioChunkInfo::default()
        },
        sample_buffer(&pools(), samples),
    )
}
