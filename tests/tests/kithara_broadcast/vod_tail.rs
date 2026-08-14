use std::num::NonZeroUsize;

use kithara::{
    self,
    assets::{AssetStore, StorageBackend},
    audio::{Audio, AudioConfig, ReadOutcome},
    decode::DecoderBackend,
    hls::{Hls, HlsConfig},
    platform::{CancelToken, time::Duration},
    stream::Stream,
};
use url::Url;

use super::origin::{Origin, SAMPLE_RATE, TONE_HZ, assert_carries_the_tone};

const SEGMENTS: u64 = 4;
const PRIMING_SKIP_FRAMES: usize = 4_800;
const READ_FRAMES: usize = 44_100;
const READ_BUF_SAMPLES: usize = 4_096;

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(60)))]
async fn the_production_client_plays_the_stopped_broadcast() {
    let origin = Origin::start();
    origin.advance_to(SEGMENTS).await;
    origin.handle.stop();

    let store = AssetStore::builder()
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(32).expect("nonzero"))
        .build();
    let master = Url::parse(origin.handle.url()).expect("the handle reports a URL");
    let hls_config = HlsConfig::for_url(master)
        .store(store)
        .cancel(CancelToken::never())
        .build();
    let audio_config = AudioConfig::<Hls>::for_stream(hls_config)
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(DecoderBackend::Symphonia)
                .build(),
        )
        .build();

    let mut audio = Audio::<Stream<Hls>>::new(audio_config)
        .await
        .expect("open the stopped broadcast as HLS");
    audio.preload().expect("preload the VOD tail");

    let channels = usize::from(audio.spec().channels);
    let left = read_left_channel(&mut audio, (PRIMING_SKIP_FRAMES + READ_FRAMES) * channels);

    assert!(
        left.len() >= PRIMING_SKIP_FRAMES + READ_FRAMES,
        "the client must play past the encoder's priming, got {} frames",
        left.len()
    );
    assert_carries_the_tone(
        &left[PRIMING_SKIP_FRAMES..],
        TONE_HZ,
        SAMPLE_RATE,
        "the client's PCM",
    );
}

fn read_left_channel(audio: &mut Audio<Stream<Hls>>, samples: usize) -> Vec<f32> {
    let channels = usize::from(audio.spec().channels);
    let mut buf = vec![0.0f32; READ_BUF_SAMPLES];
    let mut left = Vec::new();
    while left.len() * channels < samples {
        match audio.read(&mut buf).expect("the client decodes the tail") {
            ReadOutcome::Pending { .. } => continue,
            ReadOutcome::Frames { count, .. } => {
                left.extend(buf[..count.get()].iter().step_by(channels));
            }
            ReadOutcome::Eof { .. } => break,
        }
    }
    left
}
