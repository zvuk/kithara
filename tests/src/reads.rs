//! Canonical audio read pumps for the integration suite.
//!
//! One definition each, shared via `kithara_integration_tests::reads`. Every
//! test that drains an `Audio<Stream<S>>` uses these instead of hand-rolling a
//! read loop, so the read contract (finiteness checks, `Pending => yield`)
//! lives in exactly one place.
#[cfg(target_arch = "wasm32")]
use kithara::platform::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use kithara::platform::tokio::task::spawn_blocking;
use kithara::{
    audio::{AudioRead, ReadOutcome},
    platform::thread,
};
use kithara_test_utils::kithara as watchdog;

/// Default audio read buffer; large enough to keep syscall overhead low but
/// small enough to exercise the pipeline in short bursts.
const READ_BUF_SAMPLES: usize = 4096;

/// Run one synchronous audio phase on the blocking pool, returning both the
/// audio owner and the operation result to the async caller.
#[cfg(not(target_arch = "wasm32"))]
pub async fn blocking_audio<A, R>(
    mut audio: A,
    operation: impl FnOnce(&mut A) -> R + Send + 'static,
) -> (A, R)
where
    A: Send + 'static,
    R: Send + 'static,
{
    spawn_blocking(move || {
        let result = operation(&mut audio);
        (audio, result)
    })
    .await
    .expect("blocking audio operation must join")
}

/// Read `audio` to natural EOF, asserting every sample is finite; returns total
/// samples read.
///
/// `ReadOutcome::Pending` means no PCM is ready yet. The pump answers it with
/// [`thread::yield_now`], which hands a flash participant's credit to the
/// quiescence engine so the virtual clock can reach the producer's timers; a
/// bare `continue` keeps that credit and freezes the clock the producer waits
/// on. The pump terminates on the real `ReadOutcome::Eof` rather than guessing
/// EOF from elapsed time; a watchdog catches a wedged pipeline.
pub fn read_to_eof<R: AudioRead + ?Sized>(audio: &mut R) -> u64 {
    read_samples(audio, None)
}

#[watchdog::hang_watchdog]
fn read_samples<R: AudioRead + ?Sized>(audio: &mut R, target: Option<u64>) -> u64 {
    let mut buf = vec![0.0f32; READ_BUF_SAMPLES];
    let mut total = 0u64;
    loop {
        hang_tick!();
        if target.is_some_and(|target| total >= target) {
            return total;
        }
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Pending { .. }) => thread::yield_now(),
            Ok(ReadOutcome::Frames { count, .. }) => {
                let n = count.get();
                for &s in &buf[..n] {
                    assert!(s.is_finite(), "non-finite sample at offset {total}");
                }
                total += n as u64;
                hang_reset!();
            }
            Ok(ReadOutcome::Eof { .. }) => return total,
            Err(e) => panic!("decode error at offset {total}: {e}"),
        }
    }
}

/// Read until at least `target` samples have been produced, or natural EOF,
/// whichever comes first; returns total samples read.
///
/// State-driven warmup: gates on decoded-sample COUNT (real produced state),
/// never on elapsed (virtual-under-flash) time. Same `Pending => yield` model
/// as [`read_to_eof`].
pub fn read_until_samples<R: AudioRead + ?Sized>(audio: &mut R, target: u64) -> u64 {
    read_samples(audio, Some(target))
}

/// Bounds for cooperative wasm reads: cap the zero-read retries and require a
/// minimum sample count to have flowed through the pipeline.
#[derive(Debug, Clone, Copy)]
pub struct ReadLimit {
    /// Number of consecutive zero-byte reads to tolerate before giving up.
    pub max_zero_reads: usize,
    /// Minimum samples we require before declaring the reader healthy.
    pub min_samples: u64,
}

impl ReadLimit {
    /// Default wasm concurrency-check limits — conservative enough to survive
    /// cooperative scheduler starvation in the browser runner.
    #[must_use]
    pub const fn wasm_default() -> Self {
        Self {
            max_zero_reads: 200,
            min_samples: 8192,
        }
    }
}

/// Read until `limit.min_samples` is reached or we stall.
///
/// Native targets collapse to [`read_to_eof`]; wasm targets use the limit to
/// survive cooperative pipeline starvation in the browser runner.
#[cfg(target_arch = "wasm32")]
#[watchdog::hang_watchdog]
pub fn read_for_concurrency_check<R: AudioRead + ?Sized>(audio: &mut R, limit: ReadLimit) -> u64 {
    let mut buf = vec![0.0f32; READ_BUF_SAMPLES];
    let mut total = 0u64;
    let mut zero_reads = 0usize;

    while total < limit.min_samples && zero_reads < limit.max_zero_reads {
        hang_tick!();
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Pending { .. }) => {
                zero_reads += 1;
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Ok(ReadOutcome::Frames { count, .. }) => {
                zero_reads = 0;
                let n = count.get();
                for &sample in &buf[..n] {
                    assert!(sample.is_finite(), "non-finite sample at offset {total}");
                }
                total += n as u64;
                hang_reset!();
            }
            Ok(ReadOutcome::Eof { .. }) => break,
            Err(e) => panic!("decode error at offset {total}: {e}"),
        }
    }

    assert!(
        total >= limit.min_samples,
        "expected at least {} samples, got {total}",
        limit.min_samples,
    );
    total
}

/// Native variant: drain to natural EOF (the limit is a wasm-only starvation
/// guard).
#[cfg(not(target_arch = "wasm32"))]
pub fn read_for_concurrency_check<R: AudioRead + ?Sized>(audio: &mut R, _limit: ReadLimit) -> u64 {
    read_to_eof(audio)
}
