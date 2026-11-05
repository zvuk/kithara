use std::time::Duration;

pub(crate) const OUTPUT_LIMIT: u64 = 1_048_576;
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// The daemon's counters without its entry inventory or session summaries.
pub(crate) const STATS_REQUEST: &[u8] = b"{\"stats\":{\"include_entries\":false,\"include_summaries\":false,\"sort_by\":null,\"event_hours\":24,\"event_secs\":86400,\"client_epoch\":0}}\n";
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(100);
pub(crate) const DAEMON_IDLE_TIMEOUT: &str = "180";
pub(crate) const LEASE_FILE: &str = ".kithara-job-lease";
pub(crate) const HEARTBEAT_FILE: &str = ".kithara-job-heartbeat";
pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
pub(crate) const LOCK_POLL: Duration = Duration::from_secs(1);
pub(crate) const LOCK_ANNOUNCE: Duration = Duration::from_secs(30);
#[cfg(windows)]
pub(crate) const FILE_ID_INFO: u32 = 18;
