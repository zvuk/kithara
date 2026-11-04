use std::num::NonZeroUsize;

use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};

/// Running minimum of each gain target and the `len - 1` targets before it.
///
/// Targets come from a peak detector and are never `NaN`. Each window yields its oldest
/// smallest target, so ties, including `-0.0` against `0.0`, keep the older target and
/// `len == 1` is the identity. History before the first target and after
/// [`reset`](Self::reset) or [`set_len`](Self::set_len) is unity, so the first `len - 1`
/// outputs are at most 1.0.
///
/// Storage for the older targets of the longest window, its capacity, is taken once from
/// the caller's region by [`new`](Self::new); [`set_len`](Self::set_len) picks any length
/// up to it without touching the region. Targets run in blocks of `len - 1`: the slots
/// ahead of the current block's position hold the previous block's minimums from each slot
/// to its end, and a full block folds once from its end, so a call with `n` targets folds
/// fewer than `n + len` stored ones.
pub struct MinimumWindow {
    /// The current block's targets, then the previous block's minimums to its end, in the
    /// first `span` slots.
    older: SampleBuffer,
    /// Older targets of each window: its length minus one.
    span: usize,
    /// Targets of the current block in `older`.
    filled: usize,
    /// Oldest smallest target of the current block; `+inf` before its first.
    newer: f32,
}

/// Why [`MinimumWindow::set_len`] kept the window as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a window of {len} targets is longer than the capacity of {capacity}")]
pub struct WindowTooLong {
    /// Targets of the refused window.
    pub len: usize,
    /// Targets of the longest window the storage holds.
    pub capacity: usize,
}

impl std::fmt::Debug for MinimumWindow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MinimumWindow")
            .field("older", &self.older.len())
            .field("span", &self.span)
            .field("filled", &self.filled)
            .finish_non_exhaustive()
    }
}

impl MinimumWindow {
    /// Windows of `capacity` targets over unity history, the older targets of the longest
    /// held in storage taken from `pools`.
    ///
    /// # Errors
    /// [`PoolError`] when `capacity - 1` targets do not fit the region.
    pub fn new<S>(capacity: NonZeroUsize, pools: &PoolRegion<S>) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let span = capacity.get().saturating_sub(1);
        let mut window = Self {
            older: pools.get_with_len::<f32>(span)?,
            span,
            filled: 0,
            newer: f32::INFINITY,
        };
        window.reset();
        Ok(window)
    }

    /// Restart from unity history with windows of `len` targets, keeping the storage.
    ///
    /// # Errors
    /// [`WindowTooLong`] when `len` exceeds the capacity given to [`new`](Self::new); the
    /// window keeps its length and its history.
    pub fn set_len(&mut self, len: NonZeroUsize) -> Result<(), WindowTooLong> {
        let span = len.get().saturating_sub(1);
        if span > self.older.len() {
            return Err(WindowTooLong {
                len: len.get(),
                capacity: self.older.len().saturating_add(1),
            });
        }
        self.span = span;
        self.reset();
        Ok(())
    }

    /// Replace targets with their window minimums; return their count.
    pub fn process(&mut self, targets: &mut [f32]) -> usize {
        let count = targets.len();
        let mut rest = targets;
        while !rest.is_empty()
            && let Some(active) = self.older.get_mut(..self.span)
            && let Some((_, ahead)) = active.split_at_mut_checked(self.filled)
            && !ahead.is_empty()
        {
            let take = ahead.len().min(rest.len());
            let (block, tail) = std::mem::take(&mut rest).split_at_mut(take);
            let mut newer = self.newer;
            for (target, slot) in block.iter_mut().zip(ahead.iter_mut()) {
                let older = *slot;
                *slot = *target;
                if *target < newer {
                    newer = *target;
                }
                *target = if newer < older { newer } else { older };
            }
            self.newer = newer;
            self.filled = self.filled.saturating_add(block.len());
            if self.filled == self.span {
                self.fold();
            }
            rest = tail;
        }
        count
    }

    /// Forget every target and restart from unity history, keeping the length and the
    /// storage.
    pub fn reset(&mut self) {
        if let Some(older) = self.older.get_mut(..self.span) {
            older.fill(1.0);
        }
        self.filled = 0;
        self.newer = f32::INFINITY;
    }

    /// Turn the full block into its minimums from each slot to its end and open the next.
    fn fold(&mut self) {
        let mut newer = f32::INFINITY;
        for slot in self.older.iter_mut().take(self.span).rev() {
            if newer < *slot {
                *slot = newer;
            } else {
                newer = *slot;
            }
        }
        self.filled = 0;
        self.newer = f32::INFINITY;
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::{
        Xorshift64,
        bufpool::{pools, pools_with_budget},
        kithara,
    };

    use super::*;

    const VALUES: [f32; 10] = [
        0.0,
        -0.0,
        f32::INFINITY,
        f32::NEG_INFINITY,
        1.0,
        1.5,
        0.5,
        0.25,
        1.0e-40,
        3.0,
    ];

    fn window(len: usize) -> MinimumWindow {
        MinimumWindow::new(NonZeroUsize::new(len).unwrap(), &pools()).unwrap()
    }

    /// The window law spelled out: fold oldest to newest over unity history.
    fn oracle(len: usize, inputs: &[f32]) -> Vec<f32> {
        let history: Vec<f32> = std::iter::repeat_n(1.0, len.checked_sub(1).unwrap())
            .chain(inputs.iter().copied())
            .collect();
        history
            .windows(len)
            .map(|window| {
                window
                    .iter()
                    .copied()
                    .reduce(|minimum, target| if target < minimum { target } else { minimum })
                    .unwrap()
            })
            .collect()
    }

    fn bits(values: &[f32]) -> Vec<u32> {
        values.iter().copied().map(f32::to_bits).collect()
    }

    fn targets(count: usize, seed: u64) -> Vec<f32> {
        let mut random = Xorshift64::new(seed);
        let values = u64::try_from(VALUES.len()).unwrap();
        (0..count)
            .map(|_| {
                let index = usize::try_from(random.range_u64(0, values)).unwrap();
                *VALUES.get(index).unwrap()
            })
            .collect()
    }

    /// Targets on every `u16` level, so the minimum of a long window moves often.
    fn levels(count: usize, seed: u64) -> Vec<f32> {
        let mut random = Xorshift64::new(seed);
        (0..count)
            .map(|_| f32::from(u16::try_from(random.range_u64(0, 65_536)).unwrap()))
            .collect()
    }

    #[kithara::test]
    fn every_partition_and_reset_matches_the_window_law_bit_for_bit() {
        const CALLS: [usize; 12] = [0, 1, 5, 0, 63, 64, 65, 2, 700, 0, 3, 129];
        let mut window = window(1_000);
        for len in [1_usize, 2, 3, 8, 64, 65, 1_000] {
            window.set_len(NonZeroUsize::new(len).unwrap()).unwrap();
            for (seed, reset_at) in [(1_u64, 7_usize), (7, 1), (99, 12)] {
                let (before, after) = CALLS.split_at(reset_at);
                for (sizes, salt) in [(before, 0_u64), (after, 1)] {
                    let input = targets(sizes.iter().sum(), seed.wrapping_add(salt));
                    let mut output = input.clone();
                    let mut rest = output.as_mut_slice();
                    for &size in sizes {
                        let (call, tail) = std::mem::take(&mut rest).split_at_mut(size);
                        assert_eq!(window.process(call), size);
                        rest = tail;
                    }
                    assert_eq!(
                        bits(&output),
                        bits(&oracle(len, &input)),
                        "len {len}, seed {seed}"
                    );
                    window.reset();
                }
            }
        }
    }

    #[kithara::test]
    fn long_blocks_match_the_window_law_on_and_across_their_boundaries() {
        for len in [65_usize, 1_000] {
            let older = len.checked_sub(1).unwrap();
            let short = older.checked_sub(1).unwrap();
            let long = older.checked_mul(3).unwrap().checked_add(1).unwrap();
            let count = older.checked_mul(6).unwrap();
            let zeros: Vec<f32> = [0.0, -0.0].into_iter().cycle().take(count).collect();
            let mut window = window(len);
            for input in [levels(count, 5), levels(count, 6), zeros] {
                let mut output = input.clone();
                let mut rest = output.as_mut_slice();
                for size in [short, 1, older, long, short] {
                    let (call, tail) = std::mem::take(&mut rest).split_at_mut(size);
                    assert_eq!(window.process(call), size);
                    rest = tail;
                }
                assert!(rest.is_empty());
                assert_eq!(bits(&output), bits(&oracle(len, &input)), "len {len}");
                window.reset();
            }
        }
    }

    #[kithara::test]
    fn unity_history_covers_the_first_len_minus_one_outputs_after_new_and_reset() {
        let mut window = window(4);
        for _ in 0..2 {
            let mut history = [2.0, 3.0, 2.5, 2.0, 0.5, 2.0, 2.0, 2.0, 2.0, 0.75];
            assert_eq!(window.process(&mut history), 10);
            assert_eq!(history, [1.0, 1.0, 1.0, 2.0, 0.5, 0.5, 0.5, 0.5, 2.0, 0.75]);
            window.reset();
        }
    }

    #[kithara::test]
    fn calls_shorter_than_the_window_delay_a_ramp_by_len_minus_one() {
        let input: Vec<f32> = std::iter::successors(Some(0.0_f32), |value| Some(value + 1.0))
            .take(2_100)
            .collect();
        let mut window = window(1_000);
        let mut output = input.clone();
        let mut rest = output.as_mut_slice();
        for size in [700, 129, 63, 64, 65, 0, 1, 700, 378] {
            let (call, tail) = std::mem::take(&mut rest).split_at_mut(size);
            assert_eq!(window.process(call), size);
            rest = tail;
        }
        assert!(rest.is_empty());
        assert_eq!(bits(&output), bits(&oracle(1_000, &input)));
    }

    #[kithara::test]
    fn a_length_past_the_capacity_is_refused_and_the_window_runs_on() {
        let mut window = window(4);
        let input = targets(40, 11);
        let mut output = input.clone();
        let (before, after) = output.split_at_mut(17);
        assert_eq!(window.process(before), 17);
        assert_eq!(
            window.set_len(NonZeroUsize::new(5).unwrap()),
            Err(WindowTooLong {
                len: 5,
                capacity: 4
            })
        );
        assert_eq!(window.process(after), 23);
        assert_eq!(bits(&output), bits(&oracle(4, &input)));
        window.set_len(NonZeroUsize::new(2).unwrap()).unwrap();
        let mut shorter = input.clone();
        assert_eq!(window.process(&mut shorter), 40);
        assert_eq!(bits(&shorter), bits(&oracle(2, &input)));
    }

    #[kithara::test]
    fn the_window_takes_exactly_its_older_targets_from_the_region() {
        const OLDER: usize = 64;
        let len = NonZeroUsize::MIN.saturating_add(OLDER);
        let bytes = size_of::<f32>().checked_mul(OLDER).unwrap();
        assert!(matches!(
            MinimumWindow::new(len, &pools_with_budget(bytes.checked_sub(1).unwrap())),
            Err(PoolError::OverallBudgetExceeded { .. })
        ));
        let mut window = MinimumWindow::new(len, &pools_with_budget(bytes)).unwrap();
        let input = targets(300, 3);
        let mut output = input.clone();
        assert_eq!(window.process(&mut output), 300);
        assert_eq!(bits(&output), bits(&oracle(len.get(), &input)));
    }
}
