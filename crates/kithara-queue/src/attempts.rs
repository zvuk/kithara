use kithara_events::TrackId;
use kithara_platform::CancelToken;

/// Which loader lane a load attempt occupies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadClass {
    /// User-facing selection: one dedicated permit, isolated from
    /// prefetch so a hung background lane cannot starve selection.
    Interactive,
    /// Append-time background prefetch, capped by
    /// [`QueueConfig::max_concurrent_loads`](crate::QueueConfig::max_concurrent_loads).
    Prefetch,
}

/// Claim ticket held by a spawned attempt task. Lifecycle reports are
/// generation-checked, so a replaced ticket silently loses its claim.
pub(crate) struct Ticket {
    pub(crate) id: TrackId,
    pub(crate) generation: u64,
}

/// A track's live load attempt. Dropping the guard armed cancels the
/// attempt's per-track token, so removing a track aborts the load without an explicit call.
pub(crate) struct AttemptGuard {
    pub(crate) generation: u64,
    pub(crate) waiting: bool,
    /// `None` = disarmed: the token now belongs to the built `Resource`.
    cancel: Option<CancelToken>,
}

impl AttemptGuard {
    pub(crate) fn new(generation: u64, cancel: CancelToken) -> Self {
        Self {
            generation,
            waiting: true,
            cancel: Some(cancel),
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_none_or(CancelToken::is_cancelled)
    }

    /// Give the token up to its next owner; dropping then cancels nothing.
    pub(crate) fn disarm(&mut self) {
        self.cancel = None;
    }
}

impl Drop for AttemptGuard {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn drop_cancels_armed_guard() {
        let token = CancelToken::never().child();
        drop(AttemptGuard::new(0, token.clone()));
        assert!(token.is_cancelled());
    }

    #[kithara::test]
    fn drop_after_disarm_cancels_nothing() {
        let token = CancelToken::never().child();
        let mut guard = AttemptGuard::new(0, token.clone());
        guard.disarm();
        drop(guard);
        assert!(!token.is_cancelled());
    }
}
