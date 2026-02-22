use tokio::sync::broadcast::{self, error::RecvError};
use tracing::{info, warn};

pub(crate) fn spawn_event_logger<E>(mut events_rx: broadcast::Receiver<E>)
where
    E: core::fmt::Debug + Clone + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match events_rx.recv().await {
                Ok(event) => info!(?event),
                Err(RecvError::Lagged(n)) => warn!(n, "events lagged"),
                Err(RecvError::Closed) => break,
            }
        }
    });
}
