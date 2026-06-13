use std::{
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, Ordering},
    },
    task::Waker,
};

/// A synchronous cancel-waker: invoked from the cancelling thread when the node
/// is cancelled. Cheap, non-blocking, idempotent — typically a `Condvar`/`Notify`
/// wake.
pub(super) type SyncWaker = Arc<dyn Fn() + Send + Sync>;

/// One slot in a node's waker registry: either an async task waker (for a parked
/// `cancelled()` future) or a synchronous callback (for `on_cancel`).
pub(super) enum Slot {
    Task(Waker),
    Sync(SyncWaker),
}

impl Slot {
    fn fire(self) {
        match self {
            Self::Task(w) => w.wake(),
            Self::Sync(f) => f(),
        }
    }
}

/// Waker registry on one [`Node`]. `fired` latches once the node has cancelled
/// and drained, so a registration arriving after the drain fires immediately
/// instead of parking forever.
#[derive(Default)]
struct WakerSlots {
    fired: bool,
    next_id: u64,
    slots: Vec<(u64, Slot)>,
}

/// One node in the propagate-down cancel tree.
///
/// `flag` is the single hot field (one `Acquire` load on the RT read path).
/// `_parent` keeps the ancestor `Arc` chain alive while any descendant lives — it
/// is **never read** (the walk is always *down* through `children`), only held so
/// that dropping every clone of an intermediate token cannot deallocate a node a
/// `Weak` child still needs to reach. The leading underscore marks it as held for
/// its ownership effect alone (no read, no `allow`). `children` is the cold
/// propagation path; dead `Weak`s are swept in `cancel()`/`child()`.
pub(super) struct Node {
    flag: AtomicBool,
    _parent: Option<Arc<Self>>,
    children: Mutex<Vec<Weak<Self>>>,
    wakers: Mutex<WakerSlots>,
}

impl Node {
    pub(super) fn root() -> Arc<Self> {
        Arc::new(Self {
            flag: AtomicBool::new(false),
            _parent: None,
            children: Mutex::new(Vec::new()),
            wakers: Mutex::new(WakerSlots::default()),
        })
    }

    /// Derive a child. Born under the parent's children-lock so a concurrent
    /// `cancel()` either includes the new child in its snapshot or has already
    /// set the parent flag — in the latter case the child is born cancelled
    /// (`flag = true`, `fired = true`) so a future/waker on it never parks.
    pub(super) fn child(parent: &Arc<Self>) -> Arc<Self> {
        let mut kids = lock(&parent.children);
        let born_cancelled = parent.flag.load(Ordering::Acquire);
        let node = Arc::new(Self {
            flag: AtomicBool::new(born_cancelled),
            _parent: Some(Arc::clone(parent)),
            children: Mutex::new(Vec::new()),
            wakers: Mutex::new(WakerSlots {
                fired: born_cancelled,
                ..WakerSlots::default()
            }),
        });
        kids.retain(|w| w.strong_count() > 0);
        kids.push(Arc::downgrade(&node));
        node
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Cancel this node and, recursively, every live descendant.
    ///
    /// Idempotent: the `AcqRel` swap returns the prior value, so a repeat cancel
    /// neither re-drains nor re-walks. The flag is set with `Release` semantics
    /// **before** any waker fires, so a thread observing the wake is guaranteed
    /// to see the flag (pairs with the `Acquire` load in `is_cancelled`).
    pub(super) fn cancel(self: &Arc<Self>) {
        if self.flag.swap(true, Ordering::AcqRel) {
            return;
        }
        let drained: Vec<Slot> = {
            let mut w = lock(&self.wakers);
            w.fired = true;
            w.slots.drain(..).map(|(_, slot)| slot).collect()
        };
        for slot in drained {
            slot.fire();
        }
        let children: Vec<Arc<Self>> = {
            let mut kids = lock(&self.children);
            let live: Vec<Arc<Self>> = kids.iter().filter_map(Weak::upgrade).collect();
            kids.clear();
            live
        };
        for child in children {
            child.cancel();
        }
    }

    /// Register a slot on THIS node. If already fired, the slot fires at once and
    /// no id is stored. Returns `Some(id)` for a parked slot, `None` if it fired.
    pub(super) fn register(&self, slot: Slot) -> Option<u64> {
        let mut w = lock(&self.wakers);
        if !w.fired {
            let id = w.next_id;
            w.next_id += 1;
            w.slots.push((id, slot));
            drop(w);
            return Some(id);
        }
        drop(w);
        // Already fired: fire outside the lock.
        slot.fire();
        None
    }

    /// Overwrite the waker of an existing parked slot (each `Future::poll` may
    /// arrive with a fresh `Context`). If the slot is gone the node has fired —
    /// wake the new waker so the poll resolves.
    pub(super) fn refresh_task(&self, id: u64, waker: &Waker) {
        let mut w = lock(&self.wakers);
        if let Some((_, slot)) = w.slots.iter_mut().find(|(sid, _)| *sid == id) {
            *slot = Slot::Task(waker.clone());
        } else {
            drop(w);
            waker.wake_by_ref();
        }
    }

    pub(super) fn unregister(&self, id: u64) {
        lock(&self.wakers).slots.retain(|(sid, _)| *sid != id);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
