use kithara_bufpool::{
    OverallBudget, PoolAlias, PoolConfig, PoolError, PoolRegion, StringKey, VecKey, pool_schema,
};

use super::{
    super::{DrawCmd, DrawListBuilder, FillRule, PoolPath, Verb},
    Buffer, PoolText,
};
use crate::source::DrawPoolLimits;

const SHARDS: usize = 1;

enum CommandTag {}
enum PathTag {}
enum TextTag {}

type CommandKey = PoolAlias<CommandTag, VecKey<DrawCmd, SHARDS>>;
type PathKey = PoolAlias<PathTag, VecKey<Verb, SHARDS>>;
type TextKey = PoolAlias<TextTag, StringKey<SHARDS>>;

pool_schema! {
    pub(crate) DrawSchema {
        commands: CommandKey,
        paths: PathKey,
        text: TextKey,
    }
}

/// Aggregate reuse statistics for every draw buffer kind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PoolStats {
    /// Acquisitions that had to create a fresh empty buffer.
    pub alloc_misses: u64,
    /// Acquisitions served by the caller thread's shard.
    pub home_hits: u64,
    /// Returned buffers dropped because their shard was full or rejected them.
    pub put_drops: u64,
    /// Acquisitions served by a different thread shard.
    pub steal_hits: u64,
}

/// Shared reusable storage for retained commands, paths, and text.
#[derive(Clone, Debug)]
pub struct DrawBuffers {
    region: PoolRegion<DrawSchema>,
    limits: DrawPoolLimits,
}

impl DrawBuffers {
    /// Builds the registered draw-buffer family under one shared hard budget.
    ///
    /// # Panics
    ///
    /// Panics if the internally generated schema configuration is invalid. Only
    /// [`DrawPoolLimits::default`] was ever handed to this until a configuration
    /// document could name one; a caller building from document-supplied values
    /// should use [`Self::try_new`] instead, which reports the same failure
    /// rather than aborting.
    #[must_use]
    pub fn new(limits: DrawPoolLimits) -> Self {
        match Self::try_new(limits) {
            Ok(buffers) => buffers,
            Err(error) => panic!("valid draw buffer configuration failed: {error}"),
        }
    }

    /// Builds the registered draw-buffer family under one shared hard budget.
    ///
    /// # Errors
    /// Returns the [`PoolError`] the generated schema configuration failed
    /// with -- reachable once `limits` comes from a configuration document
    /// rather than only ever [`DrawPoolLimits::default`].
    pub fn try_new(limits: DrawPoolLimits) -> Result<Self, PoolError> {
        let max_buffers = limits.max_buffers.max(1);
        let config = |max_retained_capacity| {
            PoolConfig::builder()
                .max_buffers(max_buffers)
                .max_retained_capacity(max_retained_capacity)
                .build()
        };
        let region = DrawSchema::builder(OverallBudget(limits.max_bytes))
            .commands(config(limits.command_capacity))
            .paths(config(limits.path_capacity))
            .text(config(limits.text_capacity))
            .build()?;
        Ok(Self { region, limits })
    }

    /// Starts an empty command list backed by this buffer family.
    #[must_use]
    pub fn list(&self) -> DrawListBuilder {
        DrawListBuilder::pooled(self)
    }

    /// Copies path verbs into a buffer that returns here when unused.
    #[must_use]
    pub fn path<Verbs>(&self, rule: FillRule, verbs: Verbs) -> PoolPath
    where
        Verbs: IntoIterator<Item = Verb>,
    {
        let mut path = PoolPath::pooled(rule, self.region.get::<PathKey>());
        path.extend(verbs);
        path
    }

    /// Copies UTF-8 content into a buffer that returns here when unused.
    #[must_use]
    pub fn text(&self, content: &str) -> PoolText {
        PoolText::pooled(content, self.region.get::<TextKey>())
    }

    #[must_use]
    pub const fn limits(&self) -> DrawPoolLimits {
        self.limits
    }

    #[must_use]
    pub fn stats(&self) -> PoolStats {
        let stats = [
            self.region.pool_stats::<CommandKey>(),
            self.region.pool_stats::<PathKey>(),
            self.region.pool_stats::<TextKey>(),
        ];
        PoolStats {
            alloc_misses: stats.iter().map(|stats| stats.alloc_misses).sum(),
            home_hits: stats.iter().map(|stats| stats.home_hits).sum(),
            put_drops: stats.iter().map(|stats| stats.put_drops).sum(),
            steal_hits: stats.iter().map(|stats| stats.steal_hits).sum(),
        }
    }

    pub(in crate::draw) fn commands(&self) -> Buffer<DrawCmd> {
        Buffer::pooled(self.region.get::<CommandKey>())
    }

    pub(in crate::draw) fn pooled_path(&self, path: PoolPath) -> PoolPath {
        path.into_pooled(|| self.region.get::<PathKey>())
    }
}

impl Default for DrawBuffers {
    fn default() -> Self {
        Self::new(DrawPoolLimits::default())
    }
}

impl PartialEq for DrawBuffers {
    fn eq(&self, other: &Self) -> bool {
        self.limits == other.limits
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::draw::{Pt, Rect, Rgba};

    #[kithara::test]
    fn released_draw_values_are_reused_and_observable() {
        let buffers = DrawBuffers::new(
            DrawPoolLimits::builder()
                .max_buffers(4)
                .command_capacity(8)
                .path_capacity(8)
                .text_capacity(16)
                .build(),
        );
        {
            let path = buffers.path(FillRule::NonZero, [Verb::MoveTo(Pt { x: 0.0, y: 0.0 })]);
            let text = buffers.text("pooled");
            assert_eq!(text.as_str(), "pooled");
            let mut list = buffers.list();
            list.fill_path(
                path,
                Rgba {
                    a: 1.0,
                    b: 0.0,
                    g: 0.0,
                    r: 1.0,
                },
            );
            list.clip(
                Rect {
                    h: 1.0,
                    w: 1.0,
                    x: 0.0,
                    y: 0.0,
                },
                buffers.list().finish(),
            );
            drop(list.finish());
        }
        let cold = buffers.stats().alloc_misses;
        let cold_by_kind = [
            buffers.region.pool_stats::<CommandKey>().alloc_misses,
            buffers.region.pool_stats::<PathKey>().alloc_misses,
            buffers.region.pool_stats::<TextKey>().alloc_misses,
        ];
        {
            let path = buffers.path(FillRule::NonZero, [Verb::Close]);
            let text = buffers.text("again");
            assert_eq!(text.as_str(), "again");
            let mut list = buffers.list();
            list.fill_path(
                path,
                Rgba {
                    a: 1.0,
                    b: 0.0,
                    g: 0.0,
                    r: 1.0,
                },
            );
            drop(list.finish());
        }

        assert!(
            cold >= 4,
            "each empty buffer kind must report its first allocation"
        );
        assert_eq!(
            [
                buffers.region.pool_stats::<CommandKey>().alloc_misses,
                buffers.region.pool_stats::<PathKey>().alloc_misses,
                buffers.region.pool_stats::<TextKey>().alloc_misses,
            ],
            cold_by_kind
        );
        assert_eq!(buffers.stats().alloc_misses, cold);
        assert!(buffers.stats().home_hits >= 3);
    }

    #[kithara::test]
    fn retained_snapshots_do_not_sequester_reusable_buffers() {
        let buffers = DrawBuffers::new(
            DrawPoolLimits::builder()
                .max_buffers(4)
                .command_capacity(8)
                .path_capacity(8)
                .text_capacity(16)
                .build(),
        );
        let text = buffers.text("cached");
        let mut list = buffers.list();
        list.fill_path(
            buffers.path(FillRule::NonZero, [Verb::Close]),
            Rgba {
                a: 1.0,
                b: 1.0,
                g: 1.0,
                r: 1.0,
            },
        );
        let list = list.finish();
        let before = buffers.stats().alloc_misses;

        let snapshot = list.clone();
        let text_snapshot = text.clone();

        assert_eq!(buffers.stats().alloc_misses, before);
        assert_eq!(snapshot, list);
        assert_eq!(text_snapshot, text);
    }

    #[kithara::test]
    fn oversized_buffers_are_dropped_without_truncating_live_values() {
        let buffers = DrawBuffers::new(
            DrawPoolLimits::builder()
                .max_buffers(1)
                .command_capacity(1)
                .path_capacity(2)
                .text_capacity(1)
                .build(),
        );
        let path = buffers.path(
            FillRule::NonZero,
            [
                Verb::MoveTo(Pt { x: 0.0, y: 0.0 }),
                Verb::LineTo(Pt { x: 1.0, y: 0.0 }),
                Verb::Close,
            ],
        );
        assert_eq!(path.verbs().len(), 3);
        drop(path);
        let after_drop = buffers.stats();
        assert_eq!(after_drop.put_drops, 1);

        drop(buffers.path(FillRule::NonZero, [Verb::Close]));
        assert_eq!(buffers.stats().alloc_misses, after_drop.alloc_misses + 1);
    }
}
