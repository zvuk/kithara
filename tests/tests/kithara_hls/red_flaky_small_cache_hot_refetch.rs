#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use kithara::{
    assets::{StorageBackend, StoreOptions},
    audio::{Audio, AudioConfig, ChunkOutcome, PcmRead},
    hls::{Hls, HlsConfig},
    platform::{time::Duration, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::{TestServerHelper, auto, flash_pace::virtual_pace};
use tracing::info;

struct Consts;
impl Consts {
    const WARMUP_CHUNKS: usize = 4;
    const READER_SLEEP_MS: u64 = 30;
    const MIN_PROGRESS_CHUNKS: usize = 100;
    /// Paced-drain budget counted in chunks (~12 s at `READER_SLEEP_MS`
    /// pacing) — a clock-agnostic stand-in for the old wall-clock playback
    /// budget. EOF is not reachable inside the budget and is not required;
    /// a hot-refetch livelock parks the reader forever and trips the test
    /// timeout instead.
    const DRAIN_CHUNKS: usize = 400;
}

#[kithara::test(
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info,suite_stress=info")
)]
async fn red_flaky_small_cache_hot_refetch_behind_reader() {
    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");

    let store = StoreOptions::builder()
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(1).expect("nonzero"))
        .build();

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .initial_abr_mode(auto(0))
        .build();

    // Offline pull: park on ring underrun instead of spinning on Pending,
    // so the loops need no wall-clock deadlines — a hot-refetch livelock
    // becomes a permanent park caught by the hang watchdog / timeout.
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .block_on_underrun(true)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("audio creation");

    // The blocking read phase must NOT run on the test runtime thread: with
    // block_on_underrun the read parks the thread, and on the current-thread
    // runtime that starves the HLS drive/fetch tasks that feed it. preload()
    // primes through the same parking recv, so it belongs here too.
    let drained = spawn_blocking(move || {
        let _ = audio.preload();
        info!("warmup: reading {} chunks", Consts::WARMUP_CHUNKS);
        let mut chunks_read = 0usize;
        while chunks_read < Consts::WARMUP_CHUNKS {
            match PcmRead::next_chunk(&mut audio) {
                Ok(ChunkOutcome::Chunk(_)) => chunks_read += 1,
                Ok(ChunkOutcome::Eof { .. }) => break,
                Ok(ChunkOutcome::Pending { .. }) => {
                    virtual_pace(Duration::from_micros(100));
                }
                Err(e) => panic!("warmup decode error: {e}"),
            }
        }
        info!(chunks_read, "warmup done");

        let mut drained = 0usize;
        let mut reached_eof = false;
        while drained < Consts::DRAIN_CHUNKS && !reached_eof {
            match PcmRead::next_chunk(&mut audio) {
                Ok(ChunkOutcome::Chunk(_)) => {
                    drained += 1;
                    // Load-bearing pacing: the reader must lag the network so
                    // the cap=1 LRU evicts segments behind/under the reader.
                    virtual_pace(Duration::from_millis(Consts::READER_SLEEP_MS));
                }
                Ok(ChunkOutcome::Eof { .. }) => {
                    reached_eof = true;
                }
                Ok(ChunkOutcome::Pending { .. }) => {
                    virtual_pace(Duration::from_micros(100));
                }
                Err(e) => panic!("drain decode error: {e}"),
            }
        }

        info!(drained, reached_eof, "drain done");

        drop(audio);

        drained
    })
    .await
    .expect("read phase join");

    assert!(
        drained >= Consts::MIN_PROGRESS_CHUNKS,
        "RED: full drain produced only {} chunks (expected >= {}). \
         With cache_capacity=1 on an ephemeral store the scheduler \
         re-fetches every segment multiple times, and each re-fetch \
         evicts the reader's live window before it can be decoded. \
         The production flake `live_ephemeral_small_cache_playback_hls` hits \
         the same pattern at cap=4 under CPU contention.",
        drained,
        Consts::MIN_PROGRESS_CHUNKS
    );
}
