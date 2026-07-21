#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    audio::{
        Audio, AudioConfig, PcmControl, PcmRead, ReadOutcome, SourceRange, SourceRangeReadOutcome,
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
async fn canonical_source_range_matches_the_unity_linear_path() {
    let mut linear = Audio::<Stream<MemStream>>::new(audio_config())
        .await
        .expect("linear audio construction");
    let mut bounded = Audio::<Stream<MemStream>>::new(audio_config())
        .await
        .expect("bounded audio construction");
    linear.preload().expect("linear preload");
    bounded.preload().expect("bounded preload");

    let end = u64::try_from(BLOCK_FRAMES).expect("block size fits source axis");
    let range = SourceRange::try_from(0..end).expect("valid source range");
    let request = bounded
        .request_source_range(range)
        .expect("canonical range request");
    let sample_count = BLOCK_FRAMES * usize::from(CHANNELS);
    let mut audible = vec![0.0; sample_count];
    let mut captured = vec![0.0; sample_count];
    let mut audible_ready = false;
    let mut source_ready = false;

    for _ in 0..1_000 {
        if !audible_ready
            && let ReadOutcome::Frames { count, .. } =
                linear.read(&mut audible).expect("read linear output")
        {
            audible_ready = count.get() == sample_count;
        }
        if !source_ready {
            source_ready = matches!(
                bounded
                    .read_source_range(request, &mut captured)
                    .expect("read canonical source range"),
                SourceRangeReadOutcome::Ready {
                    frames: BLOCK_FRAMES
                }
            );
        }
        if audible_ready && source_ready {
            break;
        }
        time::sleep(Duration::from_millis(1)).await;
    }

    assert!(audible_ready, "linear path did not produce a full block");
    assert!(source_ready, "canonical range did not produce a full block");
    assert_eq!(captured, audible);
}
