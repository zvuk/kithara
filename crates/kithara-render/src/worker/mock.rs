#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

use kithara_assets::{AssetStore, StorageBackend};
use kithara_audio::{Audio, AudioConfig, NoResamplerBackend};
use kithara_file::{File, FileConfig, FileSrc};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, SegmentId, SourceSpan};
use kithara_stream::mock::NoopWorkerWake;
use kithara_test_fixtures::assets;
use ringbuf::traits::{Consumer, Producer};

use super::{PcmPacket, PcmReceiver, reader::PcmProducer, scheduler::StreamWake};
use crate::test_pools::{TestPools, pools, sample_buffer};

pub(crate) struct PcmFixture {
    pub(crate) receiver: Option<PcmReceiver>,
    pub(in crate::worker) producer: Option<PcmProducer>,
    pub(crate) audio: Option<Audio<kithara_stream::Stream<File<TestPools>>>>,
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
        let pools = pools();
        let path = assets::audio_wav_frames_44100()
            .path()
            .expect("native WAV fixture");
        let file = FileConfig::for_src(FileSrc::Local(path.to_owned()))
            .store(
                AssetStore::builder(pools.clone())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .pools(pools.clone())
            .build();
        let config = AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file).build();
        let audio = Audio::prepare(config, Arc::new(NoopWorkerWake), pools)
            .await
            .expect("prepare test source");
        let (receiver, producer) = PcmReceiver::new(
            NonZeroUsize::new(capacity).expect("nonzero ring capacity"),
            blocking,
            wake,
            &audio,
            Duration::ZERO,
        );
        Self {
            receiver: Some(receiver),
            producer: Some(producer),
            audio: Some(audio),
        }
    }

    pub(crate) fn push(&mut self, packet: PcmPacket) -> Result<(), PcmPacket> {
        let producer = self.producer.as_mut().expect("live producer");
        producer.forward.try_push(packet)?;
        producer.signal();
        Ok(())
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
