#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ChunkOutcome, PcmReader},
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{TestServerHelper, TestTempDir, auto, temp_dir};
use kithara_platform::time::{self, Duration, Instant};
use tracing::info;

struct Consts;
impl Consts {
    const PLAYBACK_BUDGET_SECS: u64 = 12;
    const WARMUP_CHUNKS: usize = 4;
    const NEXT_CHUNK_TIMEOUT: Duration = Duration::from_millis(3_000);
    const READER_SLEEP_MS: u64 = 30;
    const MIN_PROGRESS_CHUNKS: usize = 100;
}

#[kithara::test(
    flash(false),
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3"),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info,suite_stress=info")
)]
async fn red_flaky_small_cache_hot_refetch_behind_reader(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");

    let store = StoreOptions::builder()
        .cache_dir(temp_dir.path().into())
        .is_ephemeral(true)
        .cache_capacity(NonZeroUsize::new(1).expect("nonzero"))
        .build();

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .initial_abr_mode(auto(0))
        .build();

    let mut audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::new(hls_config))
        .await
        .expect("audio creation");
    let _ = audio.preload();

    info!("warmup: reading {} chunks", Consts::WARMUP_CHUNKS);
    let mut chunks_read = 0usize;
    let warmup_deadline = Instant::now() + Duration::from_secs(10);
    while chunks_read < Consts::WARMUP_CHUNKS {
        if Instant::now() > warmup_deadline {
            panic!(
                "RED: warmup stalled before reader could advance past seg 0 — \
                 only {chunks_read}/{} chunks drained in 10 s. \
                 This matches the hot-refetch loop: the LRU evicts seg 0 \
                 before the reader reaches byte 0 of it, and the scheduler \
                 keeps re-fetching seg 0 forever.",
                Consts::WARMUP_CHUNKS
            );
        }
        match PcmReader::next_chunk(&mut audio) {
            Ok(ChunkOutcome::Chunk(_)) => {
                chunks_read += 1;
                continue;
            }
            Ok(ChunkOutcome::Eof { .. }) => break,
            Ok(ChunkOutcome::Pending { .. }) => {}
            Err(e) => panic!("warmup decode error: {e}"),
        }
        time::sleep(Duration::from_micros(100)).await;
    }
    info!(chunks_read, "warmup done");

    let deadline = Instant::now() + Duration::from_secs(Consts::PLAYBACK_BUDGET_SECS);
    let mut drained = 0usize;
    let mut stall_at: Option<Duration> = None;
    let started = Instant::now();
    let mut reached_eof = false;
    while Instant::now() < deadline {
        let chunk_deadline = Instant::now() + Consts::NEXT_CHUNK_TIMEOUT;
        let mut got_chunk = false;
        let mut inner_eof = false;
        while Instant::now() < chunk_deadline {
            match PcmReader::next_chunk(&mut audio) {
                Ok(ChunkOutcome::Chunk(_)) => {
                    drained += 1;
                    got_chunk = true;
                    time::sleep(Duration::from_millis(Consts::READER_SLEEP_MS)).await;
                    break;
                }
                Ok(ChunkOutcome::Eof { .. }) => {
                    inner_eof = true;
                    break;
                }
                Ok(ChunkOutcome::Pending { .. }) => {}
                Err(e) => panic!("drain decode error: {e}"),
            }
            time::sleep(Duration::from_micros(100)).await;
        }
        if inner_eof {
            reached_eof = true;
            break;
        }
        if !got_chunk {
            stall_at = Some(started.elapsed());
            break;
        }
    }
    let _ = reached_eof;

    let elapsed = started.elapsed();
    info!(drained, ?elapsed, ?stall_at, "drain done");

    drop(audio);

    if let Some(elapsed) = stall_at {
        panic!(
            "RED: reader stalled after {elapsed:?} at chunk {drained}. \
             Hot-refetch loop on cap=1 LRU: scheduler keeps re-fetching \
             segments behind the reader; wait_range budget exceeded."
        );
    }

    assert!(
        drained >= Consts::MIN_PROGRESS_CHUNKS,
        "RED: reader crawled through only {} chunks in {}s \
         (expected >= {}). With cache_capacity=1 on an ephemeral \
         store the scheduler re-fetches every segment multiple times, and each \
         re-fetch evicts the reader's live window before it can be decoded. \
         The production flake `live_ephemeral_small_cache_playback_hls` hits \
         the same pattern at cap=4 under CPU contention.",
        drained,
        Consts::PLAYBACK_BUDGET_SECS,
        Consts::MIN_PROGRESS_CHUNKS
    );
}
