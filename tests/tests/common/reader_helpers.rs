//! Cross-test reader helpers for driving `Audio<Stream<_>>` to EOF or to a
//! minimum sample count.
//!
//! These replace per-file copies in `multi_instance/*` and future stress
//! suites — the logic is identical up to the underlying `StreamType`, so we
//! generalize instead of duplicating.

use kithara::{
    audio::{Audio, ReadOutcome},
    stream::{Stream, StreamType},
};
#[cfg(target_arch = "wasm32")]
use kithara_platform::{thread, time::Duration};

/// Default audio read buffer; large enough to keep syscall overhead low but
/// small enough to exercise the pipeline in short bursts.
const READ_BUF_SAMPLES: usize = 4096;

/// Read any `Audio<Stream<S>>` to natural EOF, asserting every sample is finite.
///
/// Panics if a decode error surfaces or the reader never returns
/// `ReadOutcome::Eof`. Returns total samples read.
pub(crate) fn read_to_eof<S: StreamType>(audio: &mut Audio<Stream<S>>) -> u64 {
    let mut buf = vec![0.0f32; READ_BUF_SAMPLES];
    let mut total = 0u64;
    loop {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Pending { .. }) => {
                // Alive but no data this tick — on native this shouldn't
                // stall indefinitely since the underlying source is
                // fully buffered. Break to avoid a tight loop.
                break;
            }
            Ok(ReadOutcome::Frames { count, .. }) => {
                let n = count.get();
                for &s in &buf[..n] {
                    assert!(s.is_finite(), "non-finite sample at offset {total}");
                }
                total += n as u64;
            }
            Ok(ReadOutcome::Eof { .. }) => return total,
            Err(e) => panic!("decode error at offset {total}: {e}"),
        }
    }
    panic!("reader stalled at {total} samples without reaching EOF");
}

/// Bounds for cooperative wasm reads: cap the zero-read retries and
/// require a minimum sample count to have flowed through the pipeline.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReadLimit {
    /// Number of consecutive zero-byte reads to tolerate before giving up.
    pub max_zero_reads: usize,
    /// Minimum samples we require before declaring the reader healthy.
    pub min_samples: u64,
}

impl ReadLimit {
    /// Default wasm concurrency-check limits — conservative enough to
    /// survive cooperative scheduler starvation in the browser runner.
    pub(crate) const fn wasm_default() -> Self {
        Self {
            max_zero_reads: 200,
            min_samples: 8192,
        }
    }
}

/// Read until `limit.min_samples` is reached or we stall.
///
/// Native targets collapse to [`read_to_eof`]; wasm targets use the limit
/// to survive pipeline starvation.
#[cfg(target_arch = "wasm32")]
pub(crate) fn read_for_concurrency_check<S: StreamType>(
    audio: &mut Audio<Stream<S>>,
    limit: ReadLimit,
) -> u64 {
    let mut buf = vec![0.0f32; READ_BUF_SAMPLES];
    let mut total = 0u64;
    let mut zero_reads = 0usize;

    while total < limit.min_samples && zero_reads < limit.max_zero_reads {
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

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_for_concurrency_check<S: StreamType>(
    audio: &mut Audio<Stream<S>>,
    _limit: ReadLimit,
) -> u64 {
    read_to_eof(audio)
}
