use kithara::{
    assets::AssetStore,
    bufpool::{OverallBudget, PoolConfig, PoolConfigPatch, PoolError, PoolRegion, pool_schema},
    host::{Host, HostConfig, HostCore, HostSettings},
    link::{
        LinkedConfig, LinkedFactory, LinkedHost, LinkedHostConfig, LinkedHostDeck, TempoStep,
        TempoTrajectory,
    },
    play::{PlayError, PlayWorker, PlayerFactory, ResourceConfig},
    prelude::PlaybackResamplerBackend,
    queue::{Queue, QueueControl, TrackSource},
    signal::SessionFrame,
    sync::SessionBeat,
};
use serde::Deserialize;

mod consts {
    use std::num::NonZeroU16;

    pub(super) const BEATS_PER_BAR: NonZeroU16 = match NonZeroU16::new(4) {
        Some(value) => value,
        None => unreachable!(),
    };
    pub(super) const BYTE_MAX_BUFFERS: usize = 32;
    pub(super) const BYTE_MAX_RETAINED_CAPACITY: usize = 2 * 1024 * 1024;
    pub(super) const INITIAL_SAMPLE_BUFFERS: usize = 16;
    pub(super) const INITIAL_SAMPLE_CAPACITY: usize = 9_216;
    pub(super) const OVERALL_BYTES: usize = 256 * 1024 * 1024;
    pub(super) const SAMPLE_MAX_BUFFERS: usize = 128;
    pub(super) const SAMPLE_MAX_RETAINED_CAPACITY: usize = 200_000;
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
pub type AppStore = AssetStore<AppPools>;

/// App-owned session owner: the host core with deck sync on top.
pub type AppHostOwner = LinkedHost<AppPools, HostCore<AppPools, LinkedHostDeck<AppPools>>>;

/// App-owned audio host shape.
pub type AppHost = Host<AppPools, AppHostOwner>;

/// App-owned playback worker shape.
pub type AppWorker = PlayWorker<AppPools>;

/// App-owned resource configuration shape.
pub type AppResourceConfig<B = PlaybackResamplerBackend> = ResourceConfig<AppPools, B>;

/// App-owned queue shape: every track can follow the host tempo.
pub type AppQueue = Queue<AppPools, LinkedFactory<PlayerFactory>>;

/// App-owned queue control shape.
pub type AppQueueControl = QueueControl<AppPools>;

/// App-owned track-source shape.
pub type AppTrackSource = TrackSource<AppPools>;

/// Builds the application's audio host.
///
/// # Errors
/// Returns [`PlayError`] when the host rejects `config` or cannot start.
pub fn host(config: HostConfig<AppPools>) -> Result<AppHost, PlayError> {
    let trajectory = trajectory(config.settings());
    AppHost::layered(config, move |core| {
        LinkedHost::new(
            LinkedHostConfig::builder()
                .inner(core)
                .trajectory(trajectory)
                .build(),
        )
    })
}

/// The track factory of a deck queue on a host running `settings`.
pub(crate) fn factory(settings: HostSettings) -> LinkedFactory<PlayerFactory> {
    LinkedFactory::new(
        LinkedConfig::builder()
            .inner(PlayerFactory)
            .trajectory(trajectory(settings))
            .build(),
    )
}

/// The host tempo plan before the session starts: beat zero on frame zero at
/// the configured tempo, four beats to the bar.
fn trajectory(settings: HostSettings) -> TempoTrajectory {
    TempoTrajectory::new(
        TempoStep {
            frame: SessionFrame::new(0),
            beat: SessionBeat::default(),
            tempo: settings.tempo(),
        },
        consts::BEATS_PER_BAR,
        settings.sample_rate(),
    )
}

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
        section.budget_bytes.unwrap_or(consts::OVERALL_BYTES),
    ))
    .bytes(bytes_config(&section.bytes))
    .samples(samples_config(&section.samples))
    .build()
}

fn bytes_config(patch: &PoolConfigPatch) -> PoolConfig {
    let mut config = PoolConfig::builder()
        .initial_buffers(0)
        .max_buffers(consts::BYTE_MAX_BUFFERS)
        .max_retained_capacity(consts::BYTE_MAX_RETAINED_CAPACITY)
        .build();
    config.apply(patch.clone());
    config
}

fn samples_config(patch: &PoolConfigPatch) -> PoolConfig {
    let mut config = PoolConfig::builder()
        .initial_buffers(consts::INITIAL_SAMPLE_BUFFERS)
        .initial_capacity(consts::INITIAL_SAMPLE_CAPACITY)
        .max_buffers(consts::SAMPLE_MAX_BUFFERS)
        .max_retained_capacity(consts::SAMPLE_MAX_RETAINED_CAPACITY)
        .build();
    config.apply(patch.clone());
    config
}

#[cfg(test)]
pub(crate) mod tests {
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
}
