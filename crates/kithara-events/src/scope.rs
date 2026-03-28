#![forbid(unsafe_code)]

use portable_atomic::{AtomicU64, Ordering};
use smallvec::SmallVec;

static BUS_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_bus_id() -> u64 {
    BUS_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Hierarchical scope path for [`EventBus`](crate::EventBus).
///
/// Stores the chain `[self, parent, grandparent, …, root]`.
/// Inline for depth ≤ 4 (no heap allocation), spills to heap for deeper trees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusScope(SmallVec<[u64; 4]>);

impl BusScope {
    /// Create a root scope with the given id.
    pub(crate) fn root(id: u64) -> Self {
        let mut path = SmallVec::new();
        path.push(id);
        Self(path)
    }

    /// Create a child scope that prepends `id` to this scope's path.
    pub(crate) fn child(&self, id: u64) -> Self {
        let mut path = SmallVec::with_capacity(self.0.len() + 1);
        path.push(id);
        path.extend_from_slice(&self.0);
        Self(path)
    }

    /// Check whether `id` is anywhere in this scope's ancestor chain.
    ///
    /// Returns `true` if this scope is, or is a descendant of, the bus with `id`.
    #[must_use]
    pub fn contains(&self, id: u64) -> bool {
        self.0.contains(&id)
    }

    /// This scope's own id (first element of the path).
    #[must_use]
    pub fn id(&self) -> u64 {
        self.0[0]
    }

    /// Nesting depth (1 = root).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn root_scope() {
        let scope = BusScope::root(10);
        assert_eq!(scope.id(), 10);
        assert_eq!(scope.depth(), 1);
        assert!(scope.contains(10));
        assert!(!scope.contains(99));
    }

    #[kithara::test]
    fn child_scope() {
        let root = BusScope::root(1);
        let child = root.child(2);
        assert_eq!(child.id(), 2);
        assert_eq!(child.depth(), 2);
        assert!(child.contains(2));
        assert!(child.contains(1));
        assert!(!child.contains(99));
    }

    #[kithara::test]
    fn grandchild_scope() {
        let root = BusScope::root(1);
        let child = root.child(2);
        let grandchild = child.child(3);
        assert_eq!(grandchild.id(), 3);
        assert_eq!(grandchild.depth(), 3);
        assert!(grandchild.contains(3));
        assert!(grandchild.contains(2));
        assert!(grandchild.contains(1));
    }

    #[kithara::test]
    fn sibling_isolation() {
        let root = BusScope::root(1);
        let child_a = root.child(2);
        let child_b = root.child(3);
        assert!(child_a.contains(1));
        assert!(child_b.contains(1));
        assert!(!child_a.contains(3));
        assert!(!child_b.contains(2));
    }

    #[kithara::test]
    fn deep_nesting_spills_to_heap() {
        let mut scope = BusScope::root(1);
        for i in 2..=10 {
            scope = scope.child(i);
        }
        assert_eq!(scope.depth(), 10);
        assert_eq!(scope.id(), 10);
        assert!(scope.contains(1));
        assert!(scope.contains(5));
        assert!(scope.contains(10));
        assert!(!scope.contains(11));
    }

    #[kithara::test]
    fn next_bus_id_is_monotonic() {
        let a = next_bus_id();
        let b = next_bus_id();
        let c = next_bus_id();
        assert!(a < b);
        assert!(b < c);
    }

    #[kithara::test]
    fn clone_is_independent() {
        let root = BusScope::root(1);
        let child = root.child(2);
        let cloned = child.clone();
        assert_eq!(child, cloned);
        let grandchild = child.child(3);
        assert_ne!(grandchild, cloned);
    }
}
