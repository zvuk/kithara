use ::kithara::{
    events::{Envelope, EventBus, EventReceiver, TrackId},
    platform::{time::Duration, tokio::sync::broadcast},
    queue::{AdvanceReason, QueueEvent, TrackStatus},
};
use kithara_test_utils::kithara;

use crate::core::event_set::QueueBusEvent;

/// What an event wait actually observed, so a failure names the
/// outcome instead of reporting a bare boolean.
enum WaitOutcome {
    /// The awaited event arrived.
    Observed { lagged: u64, seen: usize },
    /// The bus closed before the awaited event arrived.
    Closed { lagged: u64, seen: usize },
    /// The budget expired before the awaited event arrived.
    TimedOut {
        budget_ms: u64,
        lagged: u64,
        seen: usize,
    },
}

impl WaitOutcome {
    fn observed(&self) -> bool {
        matches!(self, Self::Observed { .. })
    }
}

impl std::fmt::Display for WaitOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Observed { lagged, seen } => {
                write!(f, "observed after {seen} event(s), {lagged} dropped")
            }
            Self::Closed { lagged, seen } => {
                write!(f, "bus closed after {seen} event(s), {lagged} dropped")
            }
            Self::TimedOut {
                budget_ms,
                lagged,
                seen,
            } => write!(
                f,
                "no event within {budget_ms}ms, after {seen} event(s), {lagged} dropped"
            ),
        }
    }
}

/// Waits for an event the bus may deliver behind a dropped burst.
///
/// `Lagged` reports that the bus dropped the oldest envelopes and kept
/// delivering, so the wait continues exactly as the bridge's own event
/// task does; only `Closed` and the budget end it.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn wait_for_event(
    events: &mut EventReceiver<QueueBusEvent>,
    timeout_ms: u64,
    is_awaited: impl Fn(&QueueBusEvent) -> bool,
) -> WaitOutcome {
    let mut lagged = 0_u64;
    let mut seen = 0_usize;
    let ended = {
        let lagged = &mut lagged;
        let seen = &mut seen;
        let wait = async move {
            loop {
                hang_tick!();
                match ::kithara::platform::time::timeout(Duration::from_millis(10), events.recv())
                    .await
                {
                    Ok(Ok(Envelope { event, .. })) => {
                        *seen += 1;
                        hang_reset!();
                        if is_awaited(&event) {
                            return true;
                        }
                    }
                    Ok(Err(broadcast::error::RecvError::Lagged(dropped))) => {
                        *lagged += dropped;
                        hang_reset!();
                    }
                    Ok(Err(broadcast::error::RecvError::Closed)) => return false,
                    Err(_) => {}
                }
            }
        };
        ::kithara::platform::time::timeout(Duration::from_millis(timeout_ms), wait).await
    };
    match ended {
        Ok(true) => WaitOutcome::Observed { lagged, seen },
        Ok(false) => WaitOutcome::Closed { lagged, seen },
        Err(_) => WaitOutcome::TimedOut {
            lagged,
            seen,
            budget_ms: timeout_ms,
        },
    }
}

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn wait_for_eof_advance(
    events: &mut EventReceiver<QueueBusEvent>,
    id: TrackId,
    timeout_ms: u64,
) -> WaitOutcome {
    let outcome = wait_for_event(events, timeout_ms, |event| {
        matches!(
            event,
            QueueBusEvent::Queue(QueueEvent::CurrentTrackAdvance {
                reason: AdvanceReason::NaturalEof,
                id: Some(seen),
            }) if *seen == id
        )
    })
    .await;
    if outcome.observed() {
        hang_reset!();
    }
    outcome
}

/// A burst that outruns the bus drops the oldest envelopes and keeps
/// delivering, so a wait must survive the gap instead of reading it as
/// the awaited event never arriving.
#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn a_wait_survives_a_lagged_bus_and_still_observes_the_next_event() {
    const CAPACITY: usize = 4;

    let bus = EventBus::new(CAPACITY);
    let mut events = bus.subscribe::<QueueBusEvent>();
    let id = TrackId::from(23_u64);

    for _ in 0..=CAPACITY {
        hang_tick!();
        bus.publish(QueueEvent::TrackStatusChanged {
            id,
            status: TrackStatus::Loading,
        });
        hang_reset!();
    }
    bus.publish(QueueEvent::CurrentTrackAdvance {
        reason: AdvanceReason::NaturalEof,
        id: Some(id),
    });

    let outcome = wait_for_eof_advance(&mut events, id, 2000).await;
    assert!(
        outcome.observed(),
        "a dropped burst must not end the wait; wait: {outcome}"
    );
    assert!(
        matches!(&outcome, WaitOutcome::Observed { lagged, .. } if *lagged > 0),
        "the burst must outrun the bus, else the wait never met a gap; wait: {outcome}"
    );
}
