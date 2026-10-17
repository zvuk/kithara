use ::kithara::{
    platform::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
        tokio::task::{spawn, spawn_blocking},
    },
    play::PlayError,
    queue::{QueueCommand, QueueError},
};
use kithara_command::{Refused, Ticket};
use kithara_test_utils::kithara;

use super::items::ItemRegistry;
use crate::{
    pools::FfiPools,
    types::FfiError,
    web::{bridge::QueueBridge, observer::router::Routes},
};

/// Caller object custody and main-thread continuations for the Queue's postbox.
pub(crate) struct WasmInner {
    pub(super) items: Arc<Mutex<ItemRegistry>>,
    pub(super) routes: Routes,
    pub(super) bridge: QueueBridge,
}

impl Default for WasmInner {
    fn default() -> Self {
        let bridge = QueueBridge::default();
        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        Self {
            routes: Routes::new(Arc::clone(&items), Arc::clone(&bridge.queue)),
            bridge,
            items,
        }
    }
}

pub(crate) type Inner = WasmInner;

impl WasmInner {
    pub(super) fn post(
        &self,
        command: QueueCommand<FfiPools>,
    ) -> Result<Ticket<QueueError>, FfiError> {
        if !self.routes.arm() {
            return Err(FfiError::Internal {
                description: "player event channel unavailable".into(),
            });
        }
        self.bridge.post(command)
    }

    pub(super) fn submit(&self, command: QueueCommand<FfiPools>) -> Result<(), FfiError> {
        let ticket = self.post(command)?;
        let routes = self.routes.clone();
        spawn(async move {
            if let Err(error) = answer(ticket).await {
                routes.error(error.to_string());
            }
        });
        Ok(())
    }

    pub(super) fn send(&self, command: QueueCommand<FfiPools>) {
        if let Err(error) = self.submit(command) {
            self.routes.error(error.to_string());
        }
    }

    pub(super) fn report(&self, result: Result<(), FfiError>) {
        if let Err(error) = result {
            self.routes.error(error.to_string());
        }
    }
}

pub(super) async fn answer(ticket: Ticket<QueueError>) -> Result<(), FfiError> {
    spawn_blocking(move || wait(ticket))
        .await
        .map_err(|error| FfiError::Internal {
            description: error.to_string(),
        })?
        .map_err(|error| FfiError::Internal {
            description: error.to_string(),
        })
}

#[kithara::hang_watchdog(ctx = String, timeout = kithara_test_utils::hang::default_timeout().min(Duration::from_secs(4)))]
fn wait(mut ticket: Ticket<QueueError>) -> Result<(), QueueError> {
    let seq = ticket.seq().get();
    loop {
        let now = Instant::now();
        match ticket.wait_until(now + __hang_detector.remaining()) {
            Ok(answer) => {
                hang_reset!();
                return answer.map_err(|refused| match refused {
                    Refused::Owner(error) => error,
                    Refused::Unanswered => PlayError::Closed.into(),
                });
            }
            Err(pending) => {
                ticket = pending;
                hang_tick!(format!("Web queue post {seq} wait_outcome=timeout"));
            }
        }
    }
}
