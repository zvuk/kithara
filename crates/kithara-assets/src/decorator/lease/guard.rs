use kithara_platform::sync::Arc;

/// RAII guard for a pin.
///
/// Dropping this guard unpins the corresponding `asset_root` and persists the new pin set
/// to disk (best-effort) via the decorator.
///
/// Uses `Arc` internally for reference counting - unpin happens only when the last clone is dropped.
/// When `inner` is `None`, the guard is a no-op (used when the lease is bypassed).
///
/// Non-generic: drop logic is captured as a closure for type erasure.
#[derive(Clone)]
pub struct LeaseGuard {
    inner: Option<Arc<LeaseGuardInner>>,
}

impl LeaseGuard {
    pub(super) fn inactive() -> Self {
        Self { inner: None }
    }

    pub(super) fn new(on_drop: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            inner: Some(Arc::new(LeaseGuardInner {
                on_drop: Box::new(on_drop),
            })),
        }
    }

    /// `true` while at least one clone of this guard still pins the lease.
    /// `false` for no-op guards constructed when the lease is bypassed.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }
}

struct LeaseGuardInner {
    on_drop: Box<dyn Fn() + Send + Sync>,
}

impl Drop for LeaseGuardInner {
    fn drop(&mut self) {
        (self.on_drop)();
    }
}
