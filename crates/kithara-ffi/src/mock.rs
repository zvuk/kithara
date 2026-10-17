#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use crate::native::session::mock::{offline, render};

#[cfg(all(test, not(target_arch = "wasm32")))]
mod playback {
    use kithara::{
        events::{Envelope, EventReceiver, TrackId},
        platform::{time::Duration, tokio::sync::broadcast},
        queue::{AdvanceReason, QueueEvent, TrackStatus},
    };

    use crate::core::event_set::QueueBusEvent;

    /// What an event wait actually observed, so a failure names the
    /// outcome instead of reporting a bare boolean.
    pub(crate) enum WaitOutcome {
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
        pub(crate) fn observed(&self) -> bool {
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
    #[kithara_test_utils::kithara::hang_watchdog]
    pub(crate) async fn wait_for_event(
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
                    let mut remaining = Duration::ZERO;
                    hang_park!(|budget| remaining = budget);
                    let received = kithara::platform::time::timeout(remaining, events.recv()).await;
                    let Ok(received) = received else { continue };
                    hang_reset!();
                    match received {
                        Ok(Envelope { event, .. }) => {
                            *seen += 1;
                            if is_awaited(&event) {
                                return true;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(dropped)) => *lagged += dropped,
                        Err(broadcast::error::RecvError::Closed) => return false,
                    }
                }
            };
            kithara::platform::time::timeout(Duration::from_millis(timeout_ms), wait).await
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

    pub(crate) async fn wait_for_status(
        events: &mut EventReceiver<QueueBusEvent>,
        id: TrackId,
        status: TrackStatus,
        timeout_ms: u64,
    ) -> WaitOutcome {
        wait_for_event(events, timeout_ms, |event| {
            matches!(
                event,
                QueueBusEvent::Queue(QueueEvent::TrackStatusChanged { id: seen, status: seen_status })
                    if *seen == id && *seen_status == status
            )
        })
        .await
    }

    pub(crate) async fn wait_for_eof_advance(
        events: &mut EventReceiver<QueueBusEvent>,
        id: TrackId,
        timeout_ms: u64,
    ) -> WaitOutcome {
        wait_for_event(events, timeout_ms, |event| {
            matches!(
                event,
                QueueBusEvent::Queue(QueueEvent::CurrentTrackAdvance {
                    reason: AdvanceReason::NaturalEof,
                    id: Some(seen),
                }) if *seen == id
            )
        })
        .await
    }

    #[kithara_test_utils::kithara::hang_watchdog]
    pub(crate) fn render_until_cancelled(
        cancel: &kithara::platform::CancelToken,
    ) -> Result<(), kithara::output::OfflineRenderError> {
        let mut cursor = 0;
        while !cancel.is_cancelled() {
            hang_tick!();
            match super::render(cursor..cursor + 512, cancel) {
                Ok(()) => {}
                Err(kithara::output::OfflineRenderError::Cancelled { .. })
                    if cancel.is_cancelled() =>
                {
                    break;
                }
                Err(error) => return Err(error),
            }
            cursor += 512;
            hang_reset!();
            hang_park!(|_| kithara::platform::thread::paced_backoff(Duration::from_millis(5)));
        }
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use playback::{
    WaitOutcome, render_until_cancelled, wait_for_eof_advance, wait_for_status,
};
