use std::sync::atomic::{AtomicBool, Ordering};

use kithara_bufpool::HasPool;
use kithara_download::FetchCmd;

use crate::{stream::HlsCoord, variant::PlanCtx};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionSlot {
    Active,
    Incoming,
}

impl SessionSlot {
    const fn other(self) -> Self {
        match self {
            Self::Active => Self::Incoming,
            Self::Incoming => Self::Active,
        }
    }
}

#[derive(Default)]
pub(super) struct SessionTurns {
    incoming: AtomicBool,
}

impl SessionTurns {
    fn next(&self, has_incoming: bool) -> SessionSlot {
        if !has_incoming {
            self.incoming.store(false, Ordering::Relaxed);
            return SessionSlot::Active;
        }
        if self.incoming.fetch_xor(true, Ordering::Relaxed) {
            SessionSlot::Incoming
        } else {
            SessionSlot::Active
        }
    }

    fn reset(&self) {
        self.incoming.store(false, Ordering::Relaxed);
    }

    /// Serve both resident sessions within one poll's budget. A one-command
    /// budget alternates; a larger budget serves active, incoming, then active.
    pub(super) fn dispatch<S>(
        &self,
        coord: &HlsCoord<S>,
        ctx: &PlanCtx<S>,
        budget: usize,
    ) -> Vec<FetchCmd>
    where
        S: HasPool<u8> + Send + Sync + 'static,
    {
        let mut cmds: Vec<FetchCmd> = Vec::new();
        let prefetch_budget = ctx.config.download_batch_size.max(1).min(budget);
        let has_incoming = coord.has_incoming();
        if has_incoming && prefetch_budget == 1 {
            let first = self.next(true);
            cmds.extend(dispatch_session(coord, ctx, first, 1));
            if cmds.len() < prefetch_budget {
                cmds.extend(dispatch_session(coord, ctx, first.other(), 1));
            }
            return cmds;
        }
        self.reset();
        let active_budget = if has_incoming && prefetch_budget > 1 {
            prefetch_budget - 1
        } else {
            prefetch_budget
        };
        cmds.extend(coord.dispatch_active(ctx, active_budget));
        let mut remaining = prefetch_budget.saturating_sub(cmds.len());
        if has_incoming && remaining > 0 {
            cmds.extend(coord.dispatch_incoming(ctx, remaining));
            remaining = prefetch_budget.saturating_sub(cmds.len());
        }
        if remaining > 0 {
            cmds.extend(coord.dispatch_active(ctx, remaining));
        }
        if cmds.is_empty() {
            tracing::trace!(
                has_incoming = coord.has_incoming(),
                budget = prefetch_budget,
                "hls peer parked without commands"
            );
        }
        cmds
    }
}

fn dispatch_session<S>(
    coord: &HlsCoord<S>,
    ctx: &PlanCtx<S>,
    slot: SessionSlot,
    budget: usize,
) -> Vec<FetchCmd>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    match slot {
        SessionSlot::Active => coord.dispatch_active(ctx, budget),
        SessionSlot::Incoming => coord.dispatch_incoming(ctx, budget),
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{SessionSlot, SessionTurns};

    #[kithara::test]
    fn one_slot_scheduler_alternates_active_and_incoming() {
        let turns = SessionTurns::default();

        assert_eq!(turns.next(true), SessionSlot::Active);
        assert_eq!(turns.next(true), SessionSlot::Incoming);
        assert_eq!(turns.next(true), SessionSlot::Active);
        assert_eq!(turns.next(true), SessionSlot::Incoming);
    }

    #[kithara::test]
    fn one_slot_scheduler_resets_when_no_incoming_session_exists() {
        let turns = SessionTurns::default();
        assert_eq!(turns.next(true), SessionSlot::Active);
        assert_eq!(turns.next(true), SessionSlot::Incoming);

        assert_eq!(turns.next(false), SessionSlot::Active);
        assert_eq!(turns.next(true), SessionSlot::Active);
    }
}
