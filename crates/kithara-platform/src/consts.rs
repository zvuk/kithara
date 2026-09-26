#[cfg(all(target_arch = "wasm32", test))]
use core::time::Duration;
#[cfg(all(feature = "no-block", not(target_arch = "wasm32"), test))]
use std::time::Duration;

#[cfg(target_arch = "wasm32")]
#[cfg(test)]
pub(crate) const TICK: Duration = Duration::from_millis(10);

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
#[cfg(test)]
pub(crate) const MSGS: usize = 100;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
#[cfg(test)]
pub(crate) const SUBS: usize = 4;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
#[cfg(test)]
pub(crate) const PER_PRODUCER: usize = 200;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
#[cfg(test)]
pub(crate) const PRODUCERS: usize = 8;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
#[cfg(test)]
pub(crate) const ROUNDS: usize = 256;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
#[cfg(test)]
pub(crate) const PERMITS: usize = 3;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
#[cfg(test)]
pub(crate) const TASKS: usize = 16;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
#[cfg(test)]
pub(crate) const NANOS_PER_SEC: u64 = 1_000_000_000;

#[cfg(feature = "no-block")]
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
#[cfg(test)]
pub(crate) const NO_BLOCK_ENGINE_WAIT_MS: u64 = 30;

#[cfg(all(not(target_arch = "wasm32"), feature = "no-block"))]
#[cfg(test)]
pub(crate) const WALL: Duration = Duration::from_millis(50);

#[cfg(all(not(target_arch = "wasm32"), feature = "no-block"))]
#[cfg(test)]
pub(crate) const FIRST_LOG_FILE_ID: usize = 0;

#[cfg(all(not(target_arch = "wasm32"), feature = "no-block"))]
#[cfg(test)]
pub(crate) const BLANKET_TEST_BUDGET_MS: u64 = 10;

#[cfg(all(not(target_arch = "wasm32"), feature = "no-block"))]
#[cfg(test)]
pub(crate) const BLANKET_TEST_SPIN_MS: u64 = 50;

#[cfg(all(not(target_arch = "wasm32"), feature = "no-block"))]
#[cfg(test)]
pub(crate) const CENSUS_LOG_BUDGET_MS: u64 = 10_000;

#[cfg(all(not(target_arch = "wasm32"), feature = "no-block"))]
#[cfg(test)]
pub(crate) const CENSUS_LOG_SLEEP_MS: u64 = 1;

#[cfg(all(not(target_arch = "wasm32"), feature = "no-block"))]
#[cfg(test)]
pub(crate) const WORK_TEST_BUDGET_MS: u64 = 10;

#[cfg(all(not(target_arch = "wasm32"), feature = "no-block"))]
#[cfg(test)]
pub(crate) const WORK_TEST_SPIN_CPU_MS: u64 = 50;
