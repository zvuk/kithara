pub(super) use kithara_signal::{AudioChunkInfo, SourceSpan};

use super::*;
pub(super) use crate::test_pools::{pools, sample_buffer};

pub(crate) struct PacketRing {
    pub(crate) receiver: Option<PcmReceiver>,
    producer: PcmProducer,
}

impl PacketRing {
    pub(crate) fn new(spec: AudioSpec, duration: Duration, capacity: usize) -> Self {
        let (forward_tx, forward_rx) = HeapRb::new(capacity).split();
        let (reverse_tx, reverse_rx) = HeapRb::new(capacity).split();
        let (playing_tx, playing_rx) = triple_buffer(&false);
        Self {
            receiver: Some(PcmReceiver {
                forward: forward_rx,
                reverse: reverse_tx,
                playing: playing_tx,
                ready: None,
                wake: StreamWake::new(kithara_worker::Wake::default()),
                spec,
                duration: Some(duration),
                position: Duration::ZERO,
                frontier: Duration::ZERO,
                metadata: TrackMetadata::default(),
                abr: None,
            }),
            producer: PcmProducer {
                forward: forward_tx,
                reverse: reverse_rx,
                playing: playing_rx,
                ready: FinalWake(None),
            },
        }
    }

    pub(in crate::worker) fn into_ends(mut self) -> (PcmReceiver, PcmProducer) {
        (self.receiver.take().expect("receiver"), self.producer)
    }

    pub(crate) fn playing(&mut self) -> bool {
        *self.producer.playing.read()
    }

    pub(crate) fn push(&mut self, packet: PcmPacket) {
        self.producer
            .forward
            .try_push(packet)
            .expect("packet ring space");
    }

    pub(crate) fn returned(&mut self) -> Option<PcmPacket> {
        self.producer.reverse.try_pop()
    }
}

pub(crate) fn chunk(
    spec: AudioSpec,
    segment: SegmentId,
    lane_frame: u64,
    source_frame: u64,
    samples: &[f32],
) -> AudioChunk {
    let frames = u64::try_from(samples.len() / usize::from(spec.channels)).expect("frames");
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frames: u32::try_from(frames).expect("chunk frames"),
            segment,
            lane_frame,
            frame_offset: source_frame,
            timestamp: spec.duration_for(source_frame).expect("timestamp"),
            end_timestamp: spec
                .duration_for(source_frame + frames)
                .expect("end timestamp"),
            source_span: if frames == 0 {
                SourceSpan::new(source_frame, source_frame + 1, spec.sample_rate, 1)
                    .and_then(|span| span.for_output_range(0..0))
            } else {
                SourceSpan::new(
                    source_frame,
                    source_frame + frames,
                    spec.sample_rate,
                    frames,
                )
            },
            ..AudioChunkInfo::default()
        },
        sample_buffer(&pools(), samples),
    )
}

pub(in crate::worker) fn packet_fixture(
    blocking: bool,
    spec: AudioSpec,
) -> (PcmReceiver, PcmProducer) {
    let (forward, received) = HeapRb::new(8).split();
    let (returned, reverse) = HeapRb::new(8).split();
    let (playing, activity) = triple_buffer(&false);
    let ready = blocking.then(|| Arc::new(ThreadGate::default()));
    let worker = kithara_worker::Worker::new(kithara_worker::WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        kithara_worker::DispatcherConfig::builder()
            .name("terminal-fixture")
            .build(),
    );
    (
        PcmReceiver {
            forward: received,
            reverse: returned,
            playing,
            ready: ready.clone(),
            wake: StreamWake::new(dispatcher.wake_handle()),
            spec,
            duration: None,
            position: Duration::ZERO,
            frontier: Duration::ZERO,
            metadata: TrackMetadata::default(),
            abr: None,
        },
        PcmProducer {
            forward,
            reverse,
            playing: activity,
            ready: FinalWake(ready),
        },
    )
}
