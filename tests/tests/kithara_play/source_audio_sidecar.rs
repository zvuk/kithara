#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    audio::{
        Audio, AudioConfig, PcmSession, ReadOutcome, SourceAudioReadOutcome, SourceFrameRange,
    },
    bufpool::{BytePool, PcmPool},
    platform::time::{self, Duration},
    stream::Stream,
};
use kithara_integration_tests::{
    create_test_wav, kithara,
    memory_source::{MemStream, MemStreamConfig, MemorySource},
};

const BLOCK_FRAMES: usize = 512;
const CHANNELS: u16 = 2;
const SAMPLE_RATE: u32 = 44_100;

fn audio_config() -> AudioConfig<MemStream> {
    let source = MemorySource::new(create_test_wav(SAMPLE_RATE as usize, SAMPLE_RATE, CHANNELS));
    AudioConfig::<MemStream>::for_stream(MemStreamConfig {
        source: Some(source),
        event_bus: None,
    })
    .byte_pool(BytePool::default())
    .pcm_pool(PcmPool::default())
    .hint("wav".to_string())
    .build()
}

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(20)))]
async fn source_sidecar_matches_the_unity_audible_path_after_seek() {
    let mut audio = Audio::<Stream<MemStream>>::new(audio_config())
        .await
        .expect("audio construction");
    let mut source_audio = audio.take_source_audio().expect("stream source sidecar");
    source_audio
        .activate(audio.spec())
        .expect("activate source capture");
    audio.seek(Duration::ZERO).expect("restart decoder");
    let range = SourceFrameRange::new(0, BLOCK_FRAMES as u64).expect("valid source range");
    let demand = source_audio
        .request(range, BLOCK_FRAMES as u64, audio.preload_epoch())
        .expect("source demand");

    let sample_count = BLOCK_FRAMES * usize::from(CHANNELS);
    let mut audible = vec![0.0; sample_count];
    let mut captured = vec![0.0; sample_count];
    let mut audible_ready = false;
    let mut source_ready = false;
    for _ in 0..1_000 {
        source_audio.poll();
        if !audible_ready
            && let ReadOutcome::Frames { count, .. } =
                audio.read(&mut audible).expect("read audible output")
        {
            audible_ready = count.get() == sample_count;
        }
        if !source_ready {
            source_ready = matches!(
                source_audio
                    .read_into(&demand, &mut captured)
                    .expect("read captured source"),
                SourceAudioReadOutcome::Ready {
                    frames: BLOCK_FRAMES
                }
            );
        }
        if audible_ready && source_ready {
            break;
        }
        time::sleep(Duration::from_millis(1)).await;
    }

    assert!(audible_ready, "audible path did not produce a full block");
    assert!(
        source_ready,
        "source sidecar did not produce the requested range"
    );
    assert_eq!(captured, audible);
}
