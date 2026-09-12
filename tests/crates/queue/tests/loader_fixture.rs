#![cfg(not(target_arch = "wasm32"))]

use std::{io::Write, num::NonZeroU32};

use kithara::{
    events::{EventReceiver, TrackId},
    platform::time::{self, Duration},
    queue::{QueueControl, QueueEvent, TrackStatus},
};
use kithara_integration_tests::{event::TestEvent, offline::OfflinePlayerHarness};

use crate::bufpool_ext::TestPools;

/// A real local WAV file kept alive while Queue owns its loader and any replay.
pub(crate) struct LocalWav {
    file: tempfile::NamedTempFile,
}

impl LocalWav {
    pub(crate) fn constant(
        label: &str,
        sample_rate: u32,
        channels: u16,
        seconds: f64,
        samples: &[u8],
    ) -> Self {
        assert!(
            samples.len().is_multiple_of(size_of::<f32>()),
            "prepared PCM must contain whole samples"
        );
        let frames = (f64::from(sample_rate) * seconds) as usize;
        assert!(
            frames <= samples.len() / size_of::<f32>(),
            "prepared PCM is shorter than requested WAV fixture"
        );
        let sample_rate = NonZeroU32::new(sample_rate).expect("test sample rate is non-zero");
        let channels = usize::from(channels);
        let data_bytes = frames
            .checked_mul(channels)
            .and_then(|count| count.checked_mul(size_of::<i16>()))
            .expect("test WAV size fits usize");
        let mut file = tempfile::Builder::new()
            .prefix(label)
            .suffix(".wav")
            .tempfile()
            .expect("create local WAV fixture");
        write_wav_header(&mut file, sample_rate.get(), channels, data_bytes);
        for frame in 0..frames {
            let start = frame * size_of::<f32>();
            let sample = f32::from_le_bytes(
                samples[start..start + size_of::<f32>()]
                    .try_into()
                    .expect("prepared PCM sample"),
            );
            let pcm = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
            for _ in 0..channels {
                file.write_all(&pcm.to_le_bytes())
                    .expect("write WAV sample");
            }
        }
        Self { file }
    }

    pub(crate) fn source(&self) -> String {
        self.file.path().to_string_lossy().into_owned()
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        std::fs::read(self.file.path()).expect("read local WAV fixture")
    }
}

pub(crate) async fn append_loaded(
    harness: &OfflinePlayerHarness,
    queue: &QueueControl<TestPools>,
    source: &LocalWav,
) -> TrackId {
    append_source_loaded(harness, queue, source.source()).await
}

pub(crate) async fn append_source_loaded(
    harness: &OfflinePlayerHarness,
    queue: &QueueControl<TestPools>,
    source: String,
) -> TrackId {
    let mut events: EventReceiver<TestEvent> = queue.subscribe();
    let id = harness
        .run(queue, move |q| q.append(source))
        .await
        .expect("append local WAV through Queue loader");
    wait_loaded(&mut events, id).await;
    id
}

pub(crate) async fn wait_loaded(events: &mut EventReceiver<TestEvent>, id: TrackId) {
    let loaded = time::timeout(Duration::from_secs(20), async {
        while let Ok(envelope) = events.recv().await {
            if matches!(
                envelope.event,
                TestEvent::Queue(QueueEvent::TrackStatusChanged {
                    id: seen,
                    status: TrackStatus::Loaded,
                }) if seen == id
            ) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(loaded, "local WAV fixture {id:?} must load through Queue");
}

fn write_wav_header(
    file: &mut tempfile::NamedTempFile,
    sample_rate: u32,
    channels: usize,
    data_bytes: usize,
) {
    let channels = u16::try_from(channels).expect("test channel count fits u16");
    let data_bytes = u32::try_from(data_bytes).expect("test WAV body fits u32");
    let bytes_per_second = sample_rate
        .checked_mul(u32::from(channels))
        .and_then(|value| value.checked_mul(2))
        .expect("test WAV rate fits u32");
    file.write_all(b"RIFF").expect("write WAV RIFF marker");
    file.write_all(&(36_u32 + data_bytes).to_le_bytes())
        .expect("write WAV size");
    file.write_all(b"WAVEfmt ")
        .expect("write WAV format marker");
    file.write_all(&16_u32.to_le_bytes())
        .expect("write WAV fmt size");
    file.write_all(&1_u16.to_le_bytes())
        .expect("write WAV PCM format");
    file.write_all(&channels.to_le_bytes())
        .expect("write WAV channels");
    file.write_all(&sample_rate.to_le_bytes())
        .expect("write WAV rate");
    file.write_all(&bytes_per_second.to_le_bytes())
        .expect("write WAV byte rate");
    file.write_all(&(channels * 2).to_le_bytes())
        .expect("write WAV block align");
    file.write_all(&16_u16.to_le_bytes())
        .expect("write WAV bit depth");
    file.write_all(b"data").expect("write WAV data marker");
    file.write_all(&data_bytes.to_le_bytes())
        .expect("write WAV data size");
}
