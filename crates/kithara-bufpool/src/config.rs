use bon::Builder;
use kithara_macros::Patch;

use crate::Percent;

/// Policy for one physical buffer pool in a region.
#[derive(Builder, Clone, Copy, Debug, PartialEq, Eq, Patch)]
pub struct PoolConfig {
    /// Number of reusable payloads allocated during region construction.
    #[builder(default)]
    pub(crate) initial_buffers: usize,
    /// Element capacity of each initially allocated payload.
    #[builder(default)]
    pub(crate) initial_capacity: usize,
    /// Maximum number of retained buffers across all shards.
    pub(crate) max_buffers: usize,
    /// Drop returned buffers above this capacity. Zero disables the ceiling.
    #[builder(default)]
    pub(crate) max_retained_capacity: usize,
    /// Maximum share of the region budget this pool may hold.
    #[builder(default = Percent::FULL)]
    pub(crate) max_share: Percent,
    /// Capacity retained when an oversized buffer returns to the pool.
    #[builder(default)]
    pub(crate) trim_capacity: usize,
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Percent, PoolConfig, PoolConfigPatch};

    #[kithara::test(native, flash(false))]
    fn a_patch_writes_only_the_field_it_names() {
        let mut config = PoolConfig::builder()
            .max_buffers(8)
            .trim_capacity(4_096)
            .build();

        let patch: PoolConfigPatch =
            serde_yaml_ng::from_str("max_buffers: 32\n").expect("a valid patch document parses");
        config.apply(patch);

        assert_eq!(config.max_buffers, 32, "the named field is written");
        assert_eq!(
            config.trim_capacity, 4_096,
            "a field the document does not name keeps the value it already had"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_share_at_the_ceiling_is_accepted() {
        let patch: PoolConfigPatch = serde_yaml_ng::from_str("max_share: 100\n")
            .expect("100 percent is inside the invariant");

        assert_eq!(patch.max_share, Some(Percent::FULL));
    }

    #[kithara::test(native, flash(false))]
    fn a_percent_above_one_hundred_is_refused() {
        let error = serde_yaml_ng::from_str::<PoolConfigPatch>("max_share: 140\n")
            .expect_err("140 percent violates the Percent invariant");

        assert!(
            error.to_string().contains("140"),
            "the refusal names the offending value: {error}"
        );
    }
}
