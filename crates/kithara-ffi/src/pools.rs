use kithara::{
    assets::AssetStore,
    bufpool::{OverallBudget, Percent, PoolConfig, PoolError, PoolRegion, pool_schema},
    play::{PlayWorker, PlaybackResamplerBackend, ResourceConfig},
    queue::{Queue, QueueControl, TrackSource},
};

use crate::consts;

pool_schema! {
    /// Buffer pools owned by one FFI engine composition root.
    pub FfiPools {
        bytes: u8,
        samples: f32,
    }
}

/// Concrete buffer-pool facade used by FFI player surfaces.
pub type Pools = PoolRegion<FfiPools>;

pub(crate) type FfiStore = AssetStore<FfiPools>;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type FfiHost = kithara::host::Host<FfiPools>;
#[cfg(target_arch = "wasm32")]
pub(crate) type FfiHost = kithara::host::Host<FfiPools>;
pub(crate) type FfiWorker = PlayWorker<FfiPools>;
pub(crate) type FfiResourceConfig<B = PlaybackResamplerBackend> = ResourceConfig<FfiPools, B>;
pub(crate) type FfiQueue = Queue<FfiPools>;
pub(crate) type FfiQueueControl = QueueControl<FfiPools>;
pub(crate) type FfiTrackSource = TrackSource<FfiPools>;

/// Build one explicitly registered FFI pool region.
///
/// # Errors
/// Returns an error when pool configuration or initial allocation fails.
pub fn build() -> Result<Pools, PoolError> {
    FfiPools::builder(OverallBudget(consts::OVERALL_BYTES))
        .bytes(
            PoolConfig::builder()
                .initial_buffers(0)
                .max_buffers(consts::BYTE_MAX_BUFFERS)
                .max_retained_capacity(consts::BYTE_MAX_RETAINED_CAPACITY)
                .max_share(Percent::MAX)
                .build(),
        )
        .samples(
            PoolConfig::builder()
                .initial_buffers(consts::INITIAL_SAMPLE_BUFFERS)
                .initial_capacity(consts::INITIAL_SAMPLE_CAPACITY)
                .max_buffers(consts::SAMPLE_MAX_BUFFERS)
                .max_retained_capacity(consts::SAMPLE_MAX_RETAINED_CAPACITY)
                .max_share(Percent::MAX)
                .build(),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use std::thread;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn initial_samples_are_ready_on_another_thread() {
        let pools = build().unwrap_or_else(|error| panic!("FFI pool region: {error}"));
        let initial_peak = pools.stats().peak_allocated_bytes;
        let worker_pools = pools.clone();

        let (all_ready, peak) = thread::spawn(move || {
            let buffers = (0..consts::INITIAL_SAMPLE_BUFFERS)
                .map(|_| {
                    worker_pools
                        .get_with_len::<f32>(consts::INITIAL_SAMPLE_CAPACITY)
                        .unwrap_or_else(|error| panic!("initial sample buffer: {error}"))
                })
                .collect::<Vec<_>>();
            let all_ready = buffers
                .iter()
                .all(|buffer| buffer.capacity() >= consts::INITIAL_SAMPLE_CAPACITY);
            let peak = worker_pools.stats().peak_allocated_bytes;
            drop(buffers);
            (all_ready, peak)
        })
        .join()
        .expect("sample-pool worker panicked");

        assert!(all_ready);
        assert_eq!(peak, initial_peak);
    }

    #[kithara::test]
    fn returned_capacity_is_bounded() {
        const BYTE_GUARDS: usize = 33;
        const BYTE_COUNT_CAPACITY: usize = 1;
        const BYTE_RETAINED_CAPACITY: usize = 2 * 1024 * 1024;
        const OVERALL_BYTES: usize = 256 * 1024 * 1024;
        const SAMPLE_RETAINED_CAPACITY: usize = 200_000;

        let pools = build().unwrap_or_else(|error| panic!("FFI pool region: {error}"));
        let baseline = pools.stats().allocated_bytes;
        assert_eq!(pools.stats().max_bytes, OVERALL_BYTES);

        let buffers = (0..BYTE_GUARDS)
            .map(|_| {
                pools
                    .get_with_len::<u8>(BYTE_COUNT_CAPACITY)
                    .unwrap_or_else(|error| panic!("byte buffer: {error}"))
            })
            .collect::<Vec<_>>();
        drop(buffers);
        assert!(
            pools.stats().allocated_bytes <= baseline + BYTE_COUNT_CAPACITY,
            "same-thread byte returns exceed one retained buffer"
        );

        let pools = build().unwrap_or_else(|error| panic!("FFI pool region: {error}"));
        let baseline = pools.stats().allocated_bytes;
        let bytes = pools
            .get_with_len::<u8>(BYTE_RETAINED_CAPACITY + 1)
            .unwrap_or_else(|error| panic!("oversized byte buffer: {error}"));
        drop(bytes);
        assert_eq!(pools.stats().allocated_bytes, baseline);

        let samples = pools
            .get_with_len::<f32>(SAMPLE_RETAINED_CAPACITY + 1)
            .unwrap_or_else(|error| panic!("oversized sample buffer: {error}"));
        drop(samples);
        assert!(
            pools.stats().allocated_bytes <= baseline,
            "oversized sample return exceeds baseline"
        );
    }
}
