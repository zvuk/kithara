use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::atomic::{AtomicUsize, Ordering},
};

/// Number of active threads spawned via the platform `spawn_named`.
///
/// Incremented on spawn, decremented when the thread function returns.
/// Used by thread-budget tests to count only kithara-owned threads.
pub(crate) static ACTIVE_NAMED_THREADS: AtomicUsize = AtomicUsize::new(0);

/// Returns the number of currently active threads spawned via the platform
/// `spawn_named`.
#[must_use]
pub fn active_named_thread_count() -> usize {
    ACTIVE_NAMED_THREADS.load(Ordering::Acquire)
}

/// Wrap `f` to bracket its execution with the named-thread counter —
/// increments on entry (at call site, before spawn), decrements after the
/// closure returns. Used by all `spawn_named` variants.
pub(crate) fn counted<F, T>(f: F) -> impl FnOnce() -> T + Send + 'static
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    ACTIVE_NAMED_THREADS.fetch_add(1, Ordering::Release);
    move || {
        let result = f();
        ACTIVE_NAMED_THREADS.fetch_sub(1, Ordering::Release);
        result
    }
}

/// Stable `u64` hash of a thread id. Used both for shard indexing and (under
/// `flash`) as the engine's thread key: `current_thread_id` and the flash
/// `unpark`'s target derive from the SAME hasher so a park and its wake agree.
/// Generic over the backend's `ThreadId` type (std vs `wasm_safe_thread`).
#[inline]
#[must_use]
pub(crate) fn thread_id_hash<I: Hash>(id: I) -> u64 {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}
