use kithara::bufpool::{
    OverallBudget, Percent, PoolConfig, PoolConfigPatch, PoolError, PoolRegion, pool_schema,
};
use serde::Deserialize;

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
    /// Buffer pools owned by the desktop application composition root.
    pub AppPools {
        bytes: u8,
        samples: f32,
    }
}

/// Concrete buffer-pool facade used by the desktop application.
pub type Pools = PoolRegion<AppPools>;

/// App-owned asset-store shape.
pub type AppStore = kithara::assets::AssetStore<AppPools>;

/// App-owned audio host shape.
pub type AppHost = kithara::host::Host<AppPools>;

/// App-owned playback worker shape.
pub type AppWorker = kithara::play::PlayWorker<AppPools>;

/// App-owned resource configuration shape.
pub type AppResourceConfig<B = kithara::prelude::PlaybackResamplerBackend> =
    kithara::play::ResourceConfig<AppPools, B>;

/// App-owned queue shape.
pub type AppQueue = kithara::queue::Queue<AppPools>;

/// App-owned queue control shape.
pub type AppQueueControl = kithara::queue::QueueControl<AppPools>;

/// App-owned track-source shape.
pub type AppTrackSource = kithara::queue::TrackSource<AppPools>;

/// What a document can say about this application's buffer pools: the region's
/// own byte budget, then one field per pool the `pool_schema!` invocation above
/// declares. There is no shared region type to derive a patch on, because
/// `pool_schema!` generates a region per consumer.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct PoolsSection {
    pub(crate) budget_bytes: Option<usize>,
    pub(crate) bytes: PoolConfigPatch,
    pub(crate) samples: PoolConfigPatch,
}

/// Build the application's single explicitly registered pool region.
///
/// # Errors
/// Returns an error when pool configuration or initial allocation fails.
pub fn build(section: &PoolsSection) -> Result<Pools, PoolError> {
    AppPools::builder(OverallBudget(
        section.budget_bytes.unwrap_or(Consts::OVERALL_BYTES),
    ))
    .bytes(bytes_config(&section.bytes))
    .samples(samples_config(&section.samples))
    .build()
}

fn bytes_config(patch: &PoolConfigPatch) -> PoolConfig {
    let mut config = PoolConfig::builder()
        .initial_buffers(0)
        .max_buffers(Consts::BYTE_MAX_BUFFERS)
        .max_retained_capacity(Consts::BYTE_MAX_RETAINED_CAPACITY)
        .max_share(Percent::FULL)
        .build();
    config.apply(patch.clone());
    config
}

fn samples_config(patch: &PoolConfigPatch) -> PoolConfig {
    let mut config = PoolConfig::builder()
        .initial_buffers(Consts::INITIAL_SAMPLE_BUFFERS)
        .initial_capacity(Consts::INITIAL_SAMPLE_CAPACITY)
        .max_buffers(Consts::SAMPLE_MAX_BUFFERS)
        .max_retained_capacity(Consts::SAMPLE_MAX_RETAINED_CAPACITY)
        .max_share(Percent::FULL)
        .build();
    config.apply(patch.clone());
    config
}

#[cfg(test)]
pub(crate) mod tests {
    use std::thread;

    use kithara_test_utils::kithara;

    use super::*;

    pub(crate) fn build_with(
        overall_budget: OverallBudget,
        bytes: PoolConfig,
        samples: PoolConfig,
    ) -> Result<Pools, PoolError> {
        AppPools::builder(overall_budget)
            .bytes(bytes)
            .samples(samples)
            .build()
    }

    #[kithara::test]
    fn initial_samples_are_ready_on_another_thread() {
        let pools = build(&PoolsSection::default())
            .unwrap_or_else(|error| panic!("app pool region: {error}"));
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

    /// The value itself is pinned in `kithara-bufpool`, which owns `PoolConfig`
    /// and can read its fields; what this pins is the routing -- that a named
    /// pool's settings reach that pool and no other.
    #[kithara::test(native, flash(false))]
    fn a_pools_document_reaches_the_pool_it_names_and_no_other() {
        let section: PoolsSection = serde_yaml_ng::from_str("bytes:\n  max_buffers: 64\n")
            .expect("a valid pools document parses");

        assert_ne!(
            bytes_config(&section.bytes),
            bytes_config(&PoolConfigPatch::default()),
            "the document's value reaches the bytes pool"
        );
        assert_eq!(
            samples_config(&section.samples),
            samples_config(&PoolConfigPatch::default()),
            "a pool the document does not name keeps the policy the builder set"
        );
    }

    #[kithara::test]
    fn a_document_budget_replaces_the_regions_own() {
        let section: PoolsSection = serde_yaml_ng::from_str("budget_bytes: 1048576\n")
            .expect("a valid pools document parses");

        let pools = build(&section).unwrap_or_else(|error| panic!("app pool region: {error}"));

        assert_eq!(pools.stats().max_bytes, 1_048_576);
    }

    #[kithara::test]
    fn returned_capacity_is_bounded() {
        const BYTE_GUARDS: usize = 33;
        const BYTE_COUNT_CAPACITY: usize = 1;
        const BYTE_RETAINED_CAPACITY: usize = 2 * 1024 * 1024;
        const OVERALL_BYTES: usize = 256 * 1024 * 1024;
        const SAMPLE_RETAINED_CAPACITY: usize = 200_000;

        let pools = build(&PoolsSection::default())
            .unwrap_or_else(|error| panic!("app pool region: {error}"));
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

        let pools = build(&PoolsSection::default())
            .unwrap_or_else(|error| panic!("app pool region: {error}"));
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
