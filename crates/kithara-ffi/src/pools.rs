use kithara::bufpool::{OverallBudget, Percent, PoolConfig, PoolError, PoolRegion, pool_schema};

struct Consts;

impl Consts {
    const BYTE_MAX_BUFFERS: usize = 32;
    const BYTE_MAX_RETAINED_CAPACITY: usize = 2 * 1024 * 1024;
    const INITIAL_SAMPLE_BUFFERS: usize = 16;
    const INITIAL_SAMPLE_CAPACITY: usize = 9_216;
    const OVERALL_BYTES: usize = 256 * 1024 * 1024;
    const SAMPLE_MAX_BUFFERS: usize = 128;
    const SAMPLE_MAX_RETAINED_CAPACITY: usize = 200_000;
}

pool_schema! {
    /// Buffer pools owned by one FFI engine composition root.
    pub FfiPools {
        bytes: u8,
        samples: f32,
    }
}

/// Concrete buffer-pool facade used by FFI player surfaces.
pub type Pools = PoolRegion<FfiPools>;

pub(crate) type FfiStore = kithara::assets::AssetStore<FfiPools>;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type FfiHost = kithara::host::Host<FfiPools>;
#[cfg(target_arch = "wasm32")]
pub(crate) type FfiHost = kithara::host::Host<FfiPools>;
pub(crate) type FfiWorker = kithara::play::PlayWorker<FfiPools>;
pub(crate) type FfiResourceConfig<B = kithara::play::PlaybackResamplerBackend> =
    kithara::play::ResourceConfig<FfiPools, B>;
pub(crate) type FfiQueue = kithara::queue::Queue<FfiPools>;
pub(crate) type FfiQueueControl = kithara::queue::QueueControl<FfiPools>;
pub(crate) type FfiTrackSource = kithara::queue::TrackSource<FfiPools>;

/// Build one explicitly registered FFI pool region.
///
/// # Errors
/// Returns an error when pool configuration or initial allocation fails.
pub fn build() -> Result<Pools, PoolError> {
    FfiPools::builder(OverallBudget(Consts::OVERALL_BYTES))
        .bytes(
            PoolConfig::builder()
                .initial_buffers(0)
                .max_buffers(Consts::BYTE_MAX_BUFFERS)
                .max_retained_capacity(Consts::BYTE_MAX_RETAINED_CAPACITY)
                .max_share(Percent::FULL)
                .build(),
        )
        .samples(
            PoolConfig::builder()
                .initial_buffers(Consts::INITIAL_SAMPLE_BUFFERS)
                .initial_capacity(Consts::INITIAL_SAMPLE_CAPACITY)
                .max_buffers(Consts::SAMPLE_MAX_BUFFERS)
                .max_retained_capacity(Consts::SAMPLE_MAX_RETAINED_CAPACITY)
                .max_share(Percent::FULL)
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
            let buffers = (0..Consts::INITIAL_SAMPLE_BUFFERS)
                .map(|_| {
                    worker_pools
                        .get_with_len::<f32>(Consts::INITIAL_SAMPLE_CAPACITY)
                        .unwrap_or_else(|error| panic!("initial sample buffer: {error}"))
                })
                .collect::<Vec<_>>();
            let all_ready = buffers
                .iter()
                .all(|buffer| buffer.capacity() >= Consts::INITIAL_SAMPLE_CAPACITY);
            let peak = worker_pools.stats().peak_allocated_bytes;
            drop(buffers);
            (all_ready, peak)
        })
        .join()
        .unwrap_or_else(|_| panic!("sample-pool worker panicked"));

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
