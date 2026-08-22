use std::mem::size_of;

use kithara_bufpool::{Reuse, SharedPool};

const SHARDS: usize = 4;

#[derive(Default)]
pub(super) struct GridScratch {
    pub(super) boundaries: Vec<usize>,
    pub(super) gaps: Vec<f64>,
    pub(super) marks: Vec<f64>,
    pub(super) neighbors: Vec<f64>,
    pub(super) outliers: Vec<bool>,
    pub(super) positions: Vec<f64>,
    pub(super) sorted: Vec<f64>,
    pub(super) spans: Vec<(usize, usize, f64, f64)>,
}

impl GridScratch {
    fn reserve(&mut self, beats: usize, downbeats: usize) {
        let numeric = beats.max(downbeats);
        reserve(&mut self.boundaries, downbeats);
        reserve(&mut self.gaps, numeric);
        reserve(&mut self.marks, beats);
        reserve(&mut self.neighbors, downbeats);
        reserve(&mut self.outliers, downbeats);
        reserve(&mut self.positions, downbeats);
        reserve(&mut self.sorted, numeric);
        reserve(&mut self.spans, downbeats);
    }
}

impl Reuse for GridScratch {
    fn byte_size(&self) -> usize {
        byte_size(&self.boundaries)
            .saturating_add(byte_size(&self.gaps))
            .saturating_add(byte_size(&self.marks))
            .saturating_add(byte_size(&self.neighbors))
            .saturating_add(byte_size(&self.outliers))
            .saturating_add(byte_size(&self.positions))
            .saturating_add(byte_size(&self.sorted))
            .saturating_add(byte_size(&self.spans))
    }

    fn reuse(&mut self, trim: usize) -> bool {
        reset(&mut self.boundaries, trim);
        reset(&mut self.gaps, trim);
        reset(&mut self.marks, trim);
        reset(&mut self.neighbors, trim);
        reset(&mut self.outliers, trim);
        reset(&mut self.positions, trim);
        reset(&mut self.sorted, trim);
        reset(&mut self.spans, trim);
        self.byte_size() > 0
    }
}

fn byte_size<T>(values: &Vec<T>) -> usize {
    values.capacity().saturating_mul(size_of::<T>())
}

fn reserve<T>(values: &mut Vec<T>, capacity: usize) {
    if values.capacity() < capacity {
        values.reserve(capacity.saturating_sub(values.len()));
    }
}

fn reset<T>(values: &mut Vec<T>, trim: usize) {
    values.clear();
    if trim > 0 && values.capacity() > trim.saturating_mul(2) {
        values.shrink_to(trim);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GridPool(SharedPool<SHARDS, GridScratch>);

impl GridPool {
    pub(super) fn with<R>(
        &self,
        beats: usize,
        downbeats: usize,
        use_scratch: impl FnOnce(&mut GridScratch) -> R,
    ) -> R {
        let mut scratch = self.0.get_with(|scratch| scratch.reserve(beats, downbeats));
        use_scratch(&mut scratch)
    }
}

impl Default for GridPool {
    fn default() -> Self {
        const MAX_BUFFERS: usize = SHARDS;
        const TRIM_CAPACITY: usize = 65_536;

        Self(SharedPool::new(MAX_BUFFERS, TRIM_CAPACITY))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(debug_assertions)]
    use assert_no_alloc::AllocDisabler;
    use assert_no_alloc::assert_no_alloc;
    use kithara_test_utils::kithara;

    use super::GridPool;

    #[cfg(debug_assertions)]
    #[global_allocator]
    static ALLOCATOR: AllocDisabler = AllocDisabler;

    #[kithara::test(native, flash(false))]
    fn repeated_grid_scratch_reuses_primitive_storage() {
        let pool = GridPool::default();
        let first = pool.with(4_096, 1_024, |scratch| scratch.gaps.as_ptr() as usize);
        let misses = pool.0.stats().alloc_misses;

        let second =
            assert_no_alloc(|| pool.with(4_096, 1_024, |scratch| scratch.gaps.as_ptr() as usize));

        assert_eq!(pool.0.stats().alloc_misses, misses);
        assert_eq!(second, first);
    }
}
