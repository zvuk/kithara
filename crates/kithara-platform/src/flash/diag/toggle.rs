use std::sync::OnceLock;

fn env_on(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Whether sync-primitive tracing is enabled (`KITHARA_FLASH_SYNC_TRACE`). Read
/// from the environment once on first call (at primitive construction, not on
/// the lock hot path), then cached.
pub(in crate::flash) fn trace_enabled() -> bool {
    static STATE: OnceLock<bool> = OnceLock::new();
    *STATE.get_or_init(|| env_on("KITHARA_FLASH_SYNC_TRACE"))
}

/// Whether the dump should capture the current thread's backtrace
/// (`KITHARA_FLASH_SYNC_BT`). Read once on first call, then cached.
pub(in crate::flash) fn bt_enabled() -> bool {
    static STATE: OnceLock<bool> = OnceLock::new();
    *STATE.get_or_init(|| env_on("KITHARA_FLASH_SYNC_BT"))
}
