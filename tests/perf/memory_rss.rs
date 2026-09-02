#![expect(
    clippy::cast_precision_loss,
    reason = "RSS values in MB, f64 precision is sufficient"
)]

use hotpath::HotpathGuardBuilder;
use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioRead, DecodeError, ReadOutcome},
    hls::{Hls, HlsConfig},
    platform::{
        time::{Duration, Instant},
        tokio::task::spawn_blocking,
    },
    play::{PlayWorker, PlayWorkerConfig},
};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir, auto,
    bufpool_ext::{TestPools, pools},
    temp_dir,
};
#[cfg(not(target_os = "linux"))]
use memory_stats::memory_stats;
use tracing::info;
use url::Url;

struct Consts;
impl Consts {
    const MB: usize = 1024 * 1024;
    const BUDGET_RUNS: usize = 3;
    const READ_FRAMES: usize = 4096;
    /// Upper bound on one drain. Nothing paces the reader, so a healthy drain
    /// ends far below this; it is here so a stalled stream fails the test
    /// instead of hanging it.
    const DRAIN_LIMIT: Duration = Duration::from_secs(20);
    /// Share of a drain that counts as warmup. Measured 2026-08-31: RSS climbs
    /// from 43.7 MB to 47.0 MB and then stays flat, so a quarter of the drain
    /// clears the ramp as long as the ladder below is long enough.
    const WARMUP_SHARE: usize = 4;
    /// Reads it took RSS to reach its plateau, measured 2026-08-31: 257, or
    /// about 12 s of audio. The ramp is a fixed startup cost - pools, decoder,
    /// first prefetch - and does not grow with the stream, so the warmup share
    /// has to clear it or warmup gets read off the ramp and every drain looks
    /// like a leak.
    const SETTLE_READS: usize = 257;
    const RSS_BUDGET_MB: usize = 30;
    const LEAK_TOLERANCE_MB: usize = 5;
}

#[cfg(target_os = "linux")]
#[hotpath::measure]
fn physical_memory() -> Option<usize> {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("Rss:"))?
        .split_ascii_whitespace()
        .next()?
        .parse::<usize>()
        .ok()?
        .checked_mul(1024)
}

#[cfg(not(target_os = "linux"))]
#[hotpath::measure]
fn physical_memory() -> Option<usize> {
    memory_stats().map(|stats| stats.physical_mem)
}

/// Why a drain stopped.
///
/// Only [`Self::Eof`] leaves a complete measurement behind. The other two
/// truncate it, and a truncated drain cannot say whether RSS settled.
enum DrainEnd {
    Eof,
    Failed(DecodeError),
    Deadline,
}

/// RSS along one read of a stream to its end, and how that read finished.
struct Drain {
    samples: Vec<usize>,
    end: DrainEnd,
    elapsed: Duration,
}

impl Drain {
    /// RSS samples of a drain that reached the end of the stream.
    ///
    /// Panics on any other ending. A drain that stops early still produces
    /// numbers, and those numbers agree with any budget, so scoring one would
    /// leave the assertions below unable to fail.
    fn complete_samples(&self) -> &[usize] {
        match &self.end {
            DrainEnd::Eof => {}
            DrainEnd::Failed(error) => panic!(
                "drain failed after {:?} and {} reads: {error}",
                self.elapsed,
                self.samples.len(),
            ),
            DrainEnd::Deadline => panic!(
                "drain reached its deadline after {:?} and {} reads",
                self.elapsed,
                self.samples.len(),
            ),
        }
        assert!(
            self.samples.len() / Consts::WARMUP_SHARE > Consts::SETTLE_READS,
            "drain produced {} reads, so its warmup share is {} and RSS needs {} \
             to settle - warmup would be read off the ramp",
            self.samples.len(),
            self.samples.len() / Consts::WARMUP_SHARE,
            Consts::SETTLE_READS,
        );
        &self.samples
    }
}

/// Serves the build-cached production ladder and hands back its master URL.
///
/// The shape is the production one - three AAC-LC variants under a FLAC one -
/// so `auto` is offered the same codec boundary to cross that it is offered in
/// production, and a codec switch reallocates decoder state. Only the length is
/// ours.
#[hotpath::measure]
fn ladder_url(server: &TestServerHelper) -> Url {
    server.asset("hls-rss/master.m3u8")
}

/// Reads `audio` to the end of the stream, sampling RSS after every read.
///
/// The reader is not paced against a clock, so the measurement window is the
/// drain itself rather than any wall-clock span: the whole track comes out in
/// a few seconds. That is why the samples are indexed by read below and not by
/// elapsed time.
#[hotpath::measure]
fn drain_sampling_rss<A: AudioRead>(audio: &mut A) -> Drain {
    let mut buf = vec![0f32; Consts::READ_FRAMES];
    let mut samples = Vec::new();
    let start = Instant::now();

    let end = loop {
        if start.elapsed() >= Consts::DRAIN_LIMIT {
            break DrainEnd::Deadline;
        }
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Eof { .. }) => break DrainEnd::Eof,
            Ok(_) => {}
            Err(error) => break DrainEnd::Failed(error),
        }
        if let Some(rss) = physical_memory() {
            samples.push(rss);
        }
    };

    Drain {
        samples,
        end,
        elapsed: start.elapsed(),
    }
}

/// Multi-run RSS measurement: peak RSS delta must stay within budget.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(90)),
    hang_timeout_secs(5)
)]
async fn test_hls_playback_rss_within_budget(temp_dir: TestTempDir) {
    let _guard = HotpathGuardBuilder::new("rss_budget").build();
    let mut run_deltas = Vec::with_capacity(Consts::BUDGET_RUNS);
    let server = TestServerHelper::new().await;
    let url = ladder_url(&server);

    for run in 0..Consts::BUDGET_RUNS {
        let baseline_rss = physical_memory().expect("RSS measurement unsupported");

        let pools = pools();
        let store = AssetStore::builder(pools.clone())
            .backend(StorageBackend::Disk {
                root: temp_dir.path().into(),
            })
            .build();
        let hls_config = HlsConfig::for_url(url.clone())
            .store(store)
            .pools(pools.clone())
            .initial_abr_mode(auto(0))
            .build();
        let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config).build();
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
        let mut audio = worker.open(config).await.expect("audio creation");

        let drain = spawn_blocking(move || drain_sampling_rss(&mut audio))
            .await
            .expect("spawn_blocking");

        let samples = drain.complete_samples();
        let peak_rss = samples
            .iter()
            .copied()
            .max()
            .expect("a complete drain has samples");
        let delta = peak_rss.saturating_sub(baseline_rss);
        run_deltas.push(delta);

        info!(
            "Run {run}: baseline={:.1}MB peak={:.1}MB delta={:.1}MB reads={} elapsed={:?}",
            baseline_rss as f64 / Consts::MB as f64,
            peak_rss as f64 / Consts::MB as f64,
            delta as f64 / Consts::MB as f64,
            samples.len(),
            drain.elapsed,
        );
    }

    let min_delta = run_deltas.iter().copied().min().unwrap_or(0);
    let max_delta = run_deltas.iter().copied().max().unwrap_or(0);
    let mean_delta = run_deltas.iter().sum::<usize>() / run_deltas.len();

    info!(
        "RSS deltas: min={:.1}MB mean={:.1}MB max={:.1}MB budget={}MB",
        min_delta as f64 / Consts::MB as f64,
        mean_delta as f64 / Consts::MB as f64,
        max_delta as f64 / Consts::MB as f64,
        Consts::RSS_BUDGET_MB
    );

    assert!(
        max_delta < Consts::RSS_BUDGET_MB * Consts::MB,
        "RSS exceeded budget: max delta {:.1}MB > {}MB",
        max_delta as f64 / Consts::MB as f64,
        Consts::RSS_BUDGET_MB
    );
}

/// RSS should stabilize after warmup — no sustained growth.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(5)
)]
async fn test_hls_playback_no_rss_leak(temp_dir: TestTempDir) {
    let _guard = HotpathGuardBuilder::new("rss_leak").build();
    let server = TestServerHelper::new().await;
    let url = ladder_url(&server);

    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().into(),
        })
        .build();
    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools.clone())
        .initial_abr_mode(auto(0))
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config).build();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    let mut audio = worker.open(config).await.expect("audio creation");

    let drain = spawn_blocking(move || drain_sampling_rss(&mut audio))
        .await
        .expect("spawn_blocking");

    let samples = drain.complete_samples();
    let warmup_reads = samples.len() / Consts::WARMUP_SHARE;
    let warmup_rss = samples[..warmup_reads]
        .iter()
        .copied()
        .max()
        .expect("a complete drain has a warmup share");
    let final_rss = *samples.last().expect("a complete drain has samples");
    let growth = final_rss.saturating_sub(warmup_rss);

    info!(
        "Leak test: warmup={:.1}MB final={:.1}MB growth={:.1}MB tolerance={}MB \
         reads={} warmup_reads={warmup_reads}",
        warmup_rss as f64 / Consts::MB as f64,
        final_rss as f64 / Consts::MB as f64,
        growth as f64 / Consts::MB as f64,
        Consts::LEAK_TOLERANCE_MB,
        samples.len(),
    );

    assert!(
        growth < Consts::LEAK_TOLERANCE_MB * Consts::MB,
        "RSS grew after warmup: {:.1}MB > {}MB (warmup={:.1}MB final={:.1}MB)",
        growth as f64 / Consts::MB as f64,
        Consts::LEAK_TOLERANCE_MB,
        warmup_rss as f64 / Consts::MB as f64,
        final_rss as f64 / Consts::MB as f64,
    );
}
