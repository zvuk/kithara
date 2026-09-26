use kithara_platform::time::Duration;
use kithara_ui::{
    compile::{CompiledNode, CompiledUi},
    draw::PoolStats,
    expand::ExpandedNode,
};

/// What one frame did, counted rather than timed.
///
/// A frame slow because it draws twice as much is a different defect from one
/// slow because it rebuilds, and these tell the two apart where a duration
/// cannot.
#[derive(Clone, Copy, Default)]
pub(crate) struct Census {
    /// Native declarations the frame produced, retained host only.
    pub(crate) natives: Option<Natives>,
    /// Vello scene encoding sizes. The immediate host has no scene and leaves
    /// this unset rather than reporting a zero it never measured.
    pub(crate) scene: Option<Scene>,
    pub(crate) pool: Pool,
    pub(crate) scheduled: bool,
}

/// One frame's share of the draw pool's counters. Reported apart, never summed
/// into one number: a sum hides reuse behind real allocation.
#[derive(Clone, Copy, Default)]
pub(crate) struct Pool {
    pub(crate) alloc_misses: u64,
    pub(crate) home_hits: u64,
    pub(crate) put_drops: u64,
    pub(crate) steal_hits: u64,
}

impl Pool {
    fn add(&mut self, other: Self) {
        self.alloc_misses += other.alloc_misses;
        self.home_hits += other.home_hits;
        self.put_drops += other.put_drops;
        self.steal_hits += other.steal_hits;
    }

    pub(crate) fn delta(before: &PoolStats, after: &PoolStats) -> Self {
        Self {
            alloc_misses: after.alloc_misses.saturating_sub(before.alloc_misses),
            home_hits: after.home_hits.saturating_sub(before.home_hits),
            put_drops: after.put_drops.saturating_sub(before.put_drops),
            steal_hits: after.steal_hits.saturating_sub(before.steal_hits),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Scene {
    pub(crate) draw_data: usize,
    pub(crate) draw_tags: usize,
    pub(crate) path_data: usize,
    pub(crate) transforms: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Natives {
    pub(crate) shaders: usize,
    pub(crate) vis: usize,
}

/// Everything one run produced, summed where summing is honest and kept apart
/// where it is not.
#[derive(Default)]
pub(crate) struct Tally {
    pub(crate) natives: Option<Natives>,
    /// The last frame's scene. A scrolled page legitimately encodes a different
    /// scene every frame, so this is a snapshot, not a total.
    pub(crate) scene: Option<Scene>,
    pub(crate) pool: Pool,
    durations: Vec<Duration>,
    /// Whether any frame declared a different number of native draws than the
    /// first. One page cannot be measured as two.
    pub(crate) natives_varied: bool,
    pub(crate) frames: usize,
    pub(crate) scheduled: usize,
}

impl Tally {
    /// Total, mean, shortest and longest frame, in microseconds.
    pub(crate) fn micros(&self) -> [u128; 4] {
        let total: u128 = self.durations.iter().map(Duration::as_micros).sum();
        let count = u128::try_from(self.durations.len().max(1)).unwrap_or(1);
        let min = self
            .durations
            .iter()
            .map(Duration::as_micros)
            .min()
            .unwrap_or_default();
        let max = self
            .durations
            .iter()
            .map(Duration::as_micros)
            .max()
            .unwrap_or_default();
        [total, total / count, min, max]
    }

    pub(crate) fn push(&mut self, census: Census, elapsed: Duration) {
        self.frames += 1;
        self.scheduled += usize::from(census.scheduled);
        if let (Some(first), Some(now)) = (self.natives, census.natives) {
            self.natives_varied |= first != now;
        }
        self.natives = self.natives.or(census.natives);
        self.scene = census.scene;
        self.pool.add(census.pool);
        self.durations.push(elapsed);
    }
}

/// How many `Vis` and `Shader` leaves the compiled page names, which is what the
/// retained host's declaration lists have to equal.
pub(crate) fn leaves(ui: &CompiledUi) -> Natives {
    fn walk(node: &ExpandedNode, found: &mut Natives) {
        match node {
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. } => {
                for child in children {
                    walk(child, found);
                }
            }
            ExpandedNode::Optional { child, .. }
            | ExpandedNode::Pressable { child, .. }
            | ExpandedNode::Scroll { child, .. } => walk(child, found),
            ExpandedNode::Popover {
                anchor, content, ..
            } => {
                walk(anchor, found);
                walk(content, found);
            }
            ExpandedNode::Control { spec, .. } => match spec.kind() {
                "Vis" => found.vis += 1,
                "Shader" => found.shaders += 1,
                _ => {}
            },
            _ => {}
        }
    }

    let mut found = Natives::default();
    let mut stack = vec![&ui.root];
    while let Some(node) = stack.pop() {
        match node {
            CompiledNode::Split { children, .. } => {
                stack.extend(children.iter().map(|cell| &cell.node));
            }
            CompiledNode::Optional { child, .. } => stack.push(child),
            CompiledNode::Module { root, .. } => walk(root, &mut found),
            _ => {}
        }
    }
    found
}
