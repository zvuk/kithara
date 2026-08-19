use kithara_bufpool::SharedPool;

use super::{
    DrawCmd, DrawListBuilder, FillRule, PoolPath, PoolText, Verb,
    buffer::{Buffer, VecPool},
    text::TextPool,
};
use crate::source::DrawPoolLimits;

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
pub struct DrawPools {
    commands: VecPool<DrawCmd>,
    paths: VecPool<Verb>,
    text: TextPool,
    limits: DrawPoolLimits,
}

impl DrawPools {
    #[must_use]
    pub fn new(limits: DrawPoolLimits) -> Self {
        let max_buffers = limits.max_buffers.max(1);
        Self {
            commands: SharedPool::new(max_buffers, limits.command_capacity),
            paths: SharedPool::new(max_buffers, limits.path_capacity),
            text: SharedPool::new(max_buffers, limits.text_capacity),
            limits,
        }
    }

    /// Starts an empty command list backed by this pool family.
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
        let mut path = PoolPath::pooled(rule, &self.paths);
        path.extend(verbs);
        path
    }

    /// Copies UTF-8 content into a buffer that returns here when unused.
    #[must_use]
    pub fn text(&self, content: &str) -> PoolText {
        PoolText::pooled(content, &self.text)
    }

    #[must_use]
    pub const fn limits(&self) -> DrawPoolLimits {
        self.limits
    }

    #[must_use]
    pub fn stats(&self) -> PoolStats {
        let stats = [self.commands.stats(), self.paths.stats(), self.text.stats()];
        PoolStats {
            alloc_misses: stats.iter().map(|stats| stats.alloc_misses).sum(),
            home_hits: stats.iter().map(|stats| stats.home_hits).sum(),
            put_drops: stats.iter().map(|stats| stats.put_drops).sum(),
            steal_hits: stats.iter().map(|stats| stats.steal_hits).sum(),
        }
    }

    pub(super) fn commands(&self) -> Buffer<DrawCmd> {
        Buffer::pooled(&self.commands)
    }

    pub(super) fn pooled_path(&self, path: PoolPath) -> PoolPath {
        path.into_pooled(&self.paths)
    }
}

impl Default for DrawPools {
    fn default() -> Self {
        Self::new(DrawPoolLimits::default())
    }
}

impl PartialEq for DrawPools {
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
        let pools = DrawPools::new(
            DrawPoolLimits::builder()
                .max_buffers(4)
                .command_capacity(8)
                .path_capacity(8)
                .text_capacity(16)
                .build(),
        );
        {
            let path = pools.path(FillRule::NonZero, [Verb::MoveTo(Pt { x: 0.0, y: 0.0 })]);
            let text = pools.text("pooled");
            assert_eq!(text.as_str(), "pooled");
            let mut list = pools.list();
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
                pools.list().finish(),
            );
            drop(list.finish());
        }
        let cold = pools.stats().alloc_misses;
        {
            let path = pools.path(FillRule::NonZero, [Verb::Close]);
            let text = pools.text("again");
            assert_eq!(text.as_str(), "again");
            let mut list = pools.list();
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
            "each empty pool must report its first allocation"
        );
        assert_eq!(pools.stats().alloc_misses, cold);
        assert!(pools.stats().home_hits >= 3);
    }

    #[kithara::test]
    fn retained_snapshots_do_not_sequester_reusable_buffers() {
        let pools = DrawPools::new(
            DrawPoolLimits::builder()
                .max_buffers(4)
                .command_capacity(8)
                .path_capacity(8)
                .text_capacity(16)
                .build(),
        );
        let text = pools.text("cached");
        let mut list = pools.list();
        list.fill_path(
            pools.path(FillRule::NonZero, [Verb::Close]),
            Rgba {
                a: 1.0,
                b: 1.0,
                g: 1.0,
                r: 1.0,
            },
        );
        let list = list.finish();
        let before = pools.stats().alloc_misses;

        let snapshot = list.clone();
        let text_snapshot = text.clone();

        assert_eq!(pools.stats().alloc_misses, before);
        assert_eq!(snapshot, list);
        assert_eq!(text_snapshot, text);
    }

    #[kithara::test]
    fn oversized_buffers_are_dropped_without_truncating_live_values() {
        let pools = DrawPools::new(
            DrawPoolLimits::builder()
                .max_buffers(1)
                .command_capacity(1)
                .path_capacity(2)
                .text_capacity(1)
                .build(),
        );
        let path = pools.path(
            FillRule::NonZero,
            [
                Verb::MoveTo(Pt { x: 0.0, y: 0.0 }),
                Verb::LineTo(Pt { x: 1.0, y: 0.0 }),
                Verb::Close,
            ],
        );
        assert_eq!(path.verbs().len(), 3);
        drop(path);
        let after_drop = pools.stats();
        assert_eq!(after_drop.put_drops, 1);

        drop(pools.path(FillRule::NonZero, [Verb::Close]));
        assert_eq!(pools.stats().alloc_misses, after_drop.alloc_misses + 1);
    }
}
