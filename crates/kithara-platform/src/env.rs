/// Returns a process-wide mutex that serializes mutations of `std::env`
/// across threads.
///
/// `std::env::set_var` and `remove_var` are not thread-safe in the
/// presence of concurrent reads from other threads. Code that mutates
/// the process environment (test setup, startup-time configuration,
/// `#[kithara::test]` proc-macro env-guard) should hold this lock
/// across the mutation to prevent races.
#[must_use]
pub fn env_mutation_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}
