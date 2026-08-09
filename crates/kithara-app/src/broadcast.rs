use std::error::Error;

use kithara::play::{Cmd, MixTapWriter, SessionHandle};
use kithara_broadcast::{Broadcast, BroadcastConfig, BroadcastHandle, RingFeed};
use kithara_platform::{
    CancelToken,
    sync::{Arc, atomic::AtomicU64},
};
use ringbuf::{HeapRb, traits::Split};

type BroadcastResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub(crate) struct BroadcastState {
    _handle: BroadcastHandle,
    session: SessionHandle,
}

impl BroadcastState {
    const CHANNELS: usize = 2;
    const RING_SECONDS: usize = 2;

    pub(crate) fn start(session: &SessionHandle, shutdown: &CancelToken) -> Option<Self> {
        match start(session, shutdown) {
            Ok(state) => {
                tracing::info!(url = state._handle.url(), "broadcast is live");
                Some(state)
            }
            Err(error) => {
                tracing::error!(%error, "broadcast did not start");
                None
            }
        }
    }
}

impl Drop for BroadcastState {
    fn drop(&mut self) {
        if let Err(error) = self.session.exec_ok(Cmd::DisableMixTap) {
            tracing::error!(%error, "failed to release broadcast mix tap");
        }
    }
}

fn start(session: &SessionHandle, shutdown: &CancelToken) -> BroadcastResult<BroadcastState> {
    let sample_rate = session.query_sample_rate(0);
    let config = config(sample_rate);
    let capacity = ring_capacity(sample_rate)?;
    let (producer, consumer) = HeapRb::<f32>::new(capacity).split();
    let drops = Arc::new(AtomicU64::new(0));

    session.exec_ok(Cmd::EnableMixTap {
        writer: MixTapWriter::new(producer, Arc::clone(&drops)),
    })?;

    let feed = RingFeed::new(consumer, drops);
    match Broadcast::start(&config, feed, Some(shutdown.child())) {
        Ok(handle) => Ok(BroadcastState {
            _handle: handle,
            session: session.clone(),
        }),
        Err(error) => {
            if let Err(disable_error) = session.exec_ok(Cmd::DisableMixTap) {
                tracing::error!(%disable_error, "failed to release mix tap after broadcast startup failure");
            }
            Err(error.into())
        }
    }
}

fn config(sample_rate: u32) -> BroadcastConfig {
    BroadcastConfig::builder().sample_rate(sample_rate).build()
}

fn ring_capacity(sample_rate: u32) -> BroadcastResult<usize> {
    let sample_rate = usize::try_from(sample_rate)?;
    if sample_rate == 0 {
        return Err("session returned zero sample rate".into());
    }

    sample_rate
        .checked_mul(BroadcastState::CHANNELS)
        .and_then(|capacity| capacity.checked_mul(BroadcastState::RING_SECONDS))
        .ok_or_else(|| "broadcast ring capacity overflow".into())
}

#[cfg(test)]
mod tests {
    use super::{config, ring_capacity};

    #[test]
    fn configuration_uses_the_session_sample_rate() {
        let config = config(44_100);

        assert_eq!(config.sample_rate, 44_100);
    }

    #[test]
    fn missing_session_sample_rate_is_rejected_before_ring_creation() {
        assert!(ring_capacity(0).is_err());
    }
}
