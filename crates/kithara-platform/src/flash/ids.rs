pub(in crate::flash) use super::system::{CvId, WaiterId};
use crate::{common::thread_id::thread_id_hash, native::thread::ThreadId};

/// Park/wake mechanism of a stateful flash primitive ([`crate::sync::Condvar`],
/// [`crate::sync::Notify`], `tokio::oneshot`; the bounded channel carries a
/// pair-form local mirror in `tokio::mpsc`), latched ONCE at construction:
/// `if flash_ambient() { Backend::Engine(next_condvar_id()) } else
/// { Backend::Native }`. Fixed for the primitive's life so EVERY caller —
/// wait AND notify, on ANY thread — uses the SAME mechanism. A notifier on a
/// thread that did not inherit the test's ambient (e.g. a raw
/// `std::thread::spawn`, which does not propagate it) still reaches an
/// engine-parked waiter. Selecting per-call on `flash_ambient()` instead would
/// diverge across threads of different ambient and silently lose the wakeup
/// (the waiter parks on the engine while the notifier signals the real
/// mechanism, or vice versa). The carried [`CvId`] keys the engine group; an
/// engine-backed primitive without a cvid is unrepresentable, and a
/// native-backed one mints none.
#[derive(Clone, Copy, Debug)]
pub(in crate::flash) enum Backend {
    /// Engine-backed: the creating context was flash-eligible (ambient).
    Engine(CvId),
    /// Real OS mechanism (the default-real path).
    Native,
}

/// Diagnostic for the [`Backend::Native`] arms: a wait/signal reaching a
/// NATIVE-latched primitive from a flash-ambient thread means the primitive
/// was created BEFORE `ambient_scope` took effect, so it is invisible to the
/// quiescence engine — a class of stalls that is otherwise undiagnosable
/// (the test wedges on a real wait the virtual clock knows nothing about).
/// The reverse direction (Engine-latched primitive signaled from a
/// non-ambient thread) is by design (see [`Backend`]) and stays silent.
/// `debug`, not `warn`: legitimate real carve-outs in `flash(false)` tests
/// land here too and must not scream.
#[inline]
pub(in crate::flash) fn trace_native_from_ambient(primitive: &'static str, op: &'static str) {
    if crate::flash::flash_ambient() {
        tracing::debug!(
            primitive,
            op,
            "native-latched primitive used from a flash-ambient thread \
             (created before ambient_scope; invisible to the engine)"
        );
    }
}

/// Hashed [`ThreadId`] key targeting a parked thread. [`ThreadKey::of`] is the
/// ONLY constructor, so the park side and the unpark side always derive the key
/// through the SAME hasher (parity contract of
/// [`thread_id_hash`](crate::common::thread_id::thread_id_hash)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::flash) struct ThreadKey(u64);

impl ThreadKey {
    pub(in crate::flash) fn of(id: ThreadId) -> Self {
        Self(thread_id_hash(id))
    }
}
