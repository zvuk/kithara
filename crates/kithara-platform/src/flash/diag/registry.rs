use std::{
    collections::BTreeMap,
    fmt::Write as _,
    panic::Location,
    sync::{Arc, LazyLock, Weak},
};

use super::thread::ThreadDesc;
use crate::{flash::ids::ThreadKey, native::sync::Mutex};

/// What kind of synchronization primitive a [`PrimEntry`] describes. Lock kinds
/// (`Mutex`/`RwLock`) carry holder/waiter state; the engine-backed kinds (added
/// in the cvid-labeling stage) carry a `cvid` so the dump correlates an opaque
/// engine waiter with the real primitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::flash) enum PrimKind {
    Mutex,
    RwLock,
}

/// A thread currently blocked trying to acquire a lock.
struct AcquireRec {
    thread: ThreadDesc,
    at: &'static Location<'static>,
}

/// The current holder of a lock (the write side, for `RwLock`).
struct HolderRec {
    thread: ThreadDesc,
    at: &'static Location<'static>,
}

/// A current read holder of an `RwLock`. `count` covers a thread that re-enters
/// the read lock (`parking_lot` permits recursive reads), so the entry survives
/// until the thread's last read guard drops.
struct ReaderRec {
    thread: ThreadDesc,
    at: &'static Location<'static>,
    count: u32,
}

/// Mutable runtime state of one lock primitive, behind an uninstrumented
/// `native` lock (NEVER the wrapped `flash` lock — that would recurse).
#[derive(Default)]
struct LockState {
    holder: Option<HolderRec>,
    readers: BTreeMap<ThreadKey, ReaderRec>,
    pending: BTreeMap<ThreadKey, AcquireRec>,
}

/// One registered live primitive plus its runtime state. Held by an `Arc` on the
/// primitive itself; the registry keeps a `Weak`, so the entry is pruned
/// automatically when the primitive drops.
pub(in crate::flash) struct PrimEntry {
    id: u64,
    kind: PrimKind,
    cvid: Option<u64>,
    created_at: &'static Location<'static>,
    created_on: ThreadDesc,
    state: Mutex<LockState>,
}

impl PrimEntry {
    /// Record that the current thread is now blocked trying to acquire this lock
    /// (call BEFORE the blocking acquire). Cleared by [`Self::acquired`].
    pub(in crate::flash) fn enter_pending(&self, at: &'static Location<'static>) {
        let thread = ThreadDesc::current();
        let key = thread.key;
        self.state
            .lock()
            .pending
            .insert(key, AcquireRec { thread, at });
    }

    /// Record that the current thread now holds this lock, clearing any pending
    /// record for it. Used by both `lock()` (after [`Self::enter_pending`]) and a
    /// successful `try_lock()` (directly).
    pub(in crate::flash) fn acquired(&self, at: &'static Location<'static>) {
        let thread = ThreadDesc::current();
        let key = thread.key;
        let mut s = self.state.lock();
        s.pending.remove(&key);
        s.holder = Some(HolderRec { thread, at });
    }

    /// Record that the holder released this lock (guard dropped, or the unlocked
    /// window of [`Condvar`](crate::sync::Condvar) wait).
    pub(in crate::flash) fn released(&self) {
        self.state.lock().holder = None;
    }

    /// Record that the current thread now holds a read lock (`RwLock::read`),
    /// clearing any pending record. Re-entrant reads bump a per-thread count.
    pub(in crate::flash) fn read_acquired(&self, at: &'static Location<'static>) {
        let thread = ThreadDesc::current();
        let key = thread.key;
        let mut s = self.state.lock();
        s.pending.remove(&key);
        s.readers
            .entry(key)
            .and_modify(|r| r.count += 1)
            .or_insert(ReaderRec {
                thread,
                at,
                count: 1,
            });
    }

    /// Record that the current thread dropped a read guard; remove its reader
    /// record once its last read guard is gone.
    pub(in crate::flash) fn read_released(&self) {
        let key = ThreadDesc::current().key;
        let mut s = self.state.lock();
        if let Some(r) = s.readers.get_mut(&key) {
            r.count -= 1;
            if r.count == 0 {
                s.readers.remove(&key);
            }
        }
    }

    /// Append this primitive's diagnostic lines to `out`. `try_lock` on the state
    /// so a dump never blocks on a held meta-lock.
    fn render(&self, out: &mut String) {
        let _ = write!(
            out,
            "  #{id} {kind:?} cvid={cvid} created_at={at} by {by}",
            id = self.id,
            kind = self.kind,
            cvid = self.cvid.map_or_else(|| "-".to_string(), |c| c.to_string()),
            at = self.created_at,
            by = self.created_on,
        );
        match self.state.try_lock() {
            Ok(s) => {
                if let Some(h) = &s.holder {
                    let _ = write!(out, "\n      held by {} at {}", h.thread, h.at);
                }
                for r in s.readers.values() {
                    let _ = write!(out, "\n      read-held by {} at {}", r.thread, r.at);
                    if r.count > 1 {
                        let _ = write!(out, " (x{})", r.count);
                    }
                }
                for rec in s.pending.values() {
                    let _ = write!(out, "\n      WAITING: {} at {}", rec.thread, rec.at);
                }
            }
            Err(_) => {
                let _ = write!(out, "\n      <state lock held — mid-operation>");
            }
        }
        out.push('\n');
    }

    fn is_contended(&self) -> bool {
        self.state
            .try_lock()
            .is_ok_and(|s| (s.holder.is_some() || !s.readers.is_empty()) && !s.pending.is_empty())
    }

    #[cfg(test)]
    fn id_tag(&self) -> String {
        format!("#{} ", self.id)
    }
}

struct RegistryInner {
    next_id: u64,
    entries: BTreeMap<u64, Weak<PrimEntry>>,
}

static REGISTRY: LazyLock<Mutex<RegistryInner>> = LazyLock::new(|| {
    Mutex::new(RegistryInner {
        next_id: 0,
        entries: BTreeMap::new(),
    })
});

/// Register a primitive and return the `Arc<PrimEntry>` it should hold for its
/// lifetime, or `None` when tracing is off (so the primitive carries no overhead
/// beyond a null check). `created_at` is the construction site (`#[track_caller]`).
pub(in crate::flash) fn register(
    kind: PrimKind,
    cvid: Option<u64>,
    created_at: &'static Location<'static>,
) -> Option<Arc<PrimEntry>> {
    super::trace_enabled().then(|| build(kind, cvid, created_at))
}

/// Build + insert an entry unconditionally (the toggle-free core of
/// [`register`], also the deterministic seam for unit tests).
fn build(
    kind: PrimKind,
    cvid: Option<u64>,
    created_at: &'static Location<'static>,
) -> Arc<PrimEntry> {
    let mut reg = REGISTRY.lock();
    let id = reg.next_id;
    reg.next_id += 1;
    let entry = Arc::new(PrimEntry {
        id,
        kind,
        cvid,
        created_at,
        created_on: ThreadDesc::current(),
        state: Mutex::new(LockState::default()),
    });
    reg.entries.insert(id, Arc::downgrade(&entry));
    entry
}

/// Diagnostic snapshot of every live primitive, with a "contended locks" section
/// (held lock + blocked acquirers) that exposes the wait-for graph. `try_lock` on
/// the registry so a dump from a wedged process never itself hangs. Prunes dead
/// `Weak`s as a side effect.
pub(in crate::flash) fn snapshot() -> String {
    let Ok(mut reg) = REGISTRY.try_lock() else {
        return "sync registry: lock held — cannot snapshot\n".to_string();
    };
    let mut live = Vec::new();
    reg.entries.retain(|_, w| {
        w.upgrade().is_some_and(|e| {
            live.push(e);
            true
        })
    });
    if live.is_empty() {
        return "sync registry: 0 live primitives (KITHARA_FLASH_SYNC_TRACE off or none created)\n"
            .to_string();
    }
    let mut out = format!("sync registry: {} live primitives\n", live.len());
    for e in &live {
        e.render(&mut out);
    }
    let contended: Vec<&Arc<PrimEntry>> = live.iter().filter(|e| e.is_contended()).collect();
    if !contended.is_empty() {
        out.push_str("contended locks (held WHILE others wait — wait-for edges):\n");
        for e in contended {
            e.render(&mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::panic::Location;

    use super::{PrimKind, build, snapshot};

    // `build` is the toggle-free core, so these tests are deterministic without
    // touching the process-wide `KITHARA_FLASH_SYNC_TRACE` env. Assertions key on
    // THIS entry's id tag (not global counts), so they hold even when other tests
    // share the process (cargo test threads) rather than nextest's per-test fork.
    #[test]
    fn registers_holder_and_prunes_on_drop() {
        let loc: &'static Location<'static> = Location::caller();
        let entry = build(PrimKind::Mutex, None, loc);
        entry.acquired(loc);
        let tag = entry.id_tag();
        let dump = snapshot();
        assert!(dump.contains(&tag), "entry listed: {dump}");
        assert!(dump.contains("held by"), "holder listed: {dump}");
        drop(entry);
        assert!(
            !snapshot().contains(&tag),
            "dropped primitive pruned (tag {tag} gone)"
        );
    }

    #[test]
    fn pending_acquirer_shows_in_contended_section() {
        let loc: &'static Location<'static> = Location::caller();
        let entry = build(PrimKind::Mutex, Some(7), loc);
        entry.acquired(loc);
        entry.enter_pending(loc);
        let dump = snapshot();
        assert!(dump.contains(&entry.id_tag()), "entry listed: {dump}");
        assert!(dump.contains("cvid=7"), "cvid listed: {dump}");
        assert!(dump.contains("WAITING"), "pending acquirer listed: {dump}");
        assert!(dump.contains("contended locks"), "wait-for section: {dump}");
    }

    #[test]
    fn rwlock_read_holders_and_blocked_writer_show_in_dump() {
        let loc: &'static Location<'static> = Location::caller();
        let entry = build(PrimKind::RwLock, None, loc);
        entry.read_acquired(loc);
        entry.enter_pending(loc); // a writer blocked behind the reader
        let dump = snapshot();
        let tag = entry.id_tag();
        assert!(dump.contains(&tag), "entry listed: {dump}");
        assert!(dump.contains("read-held by"), "reader listed: {dump}");
        assert!(dump.contains("WAITING"), "blocked writer listed: {dump}");
        assert!(dump.contains("contended locks"), "wait-for section: {dump}");
        entry.read_released();
    }
}
