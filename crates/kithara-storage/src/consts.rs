use kithara_platform::time::Duration;

/// Capacity of the retire queue. It buys time, not a bound: no capacity can
/// span an unbounded read:write ratio, so raising this number only moves the
/// overflow threshold.
pub(crate) const RETIRE_CAPACITY: usize = 256;

/// Watchdog timeout for the network-bound `wait_range_inner`: sized well
/// above the `kithara-net` `inactivity_timeout` (plus retry backoff) so a
/// stalled upstream is failed by the network layer (this wait then returns
/// `Failed`) before the deadlock-watchdog fires. Only a wait that never
/// returns after the fetch resolved is a real deadlock.
pub(crate) const WAIT_HANG_TIMEOUT: Duration = Duration::from_secs(180);

/// What a dead owner left in the tmp a successor reclaims.
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const DEAD_OWNERS_BYTES: &[u8] = b"stale-from-previous-process";
