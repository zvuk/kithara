use std::error::Error;

use kithara::play::{Cmd, MixTapWriter, SessionHandle};
use kithara_broadcast::{Broadcast, BroadcastConfig, BroadcastHandle, RingFeed};
use kithara_platform::{
    CancelToken,
    sync::{Arc, atomic::AtomicU64},
};
use ringbuf::{HeapRb, traits::Split};

type BroadcastResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub(super) struct BroadcastService {
    session: SessionHandle,
    shutdown: CancelToken,
    phase: Phase,
}

enum Phase {
    Off,
    Requested,
    Running { _state: BroadcastState },
}

struct BroadcastState {
    handle: BroadcastHandle,
    session: SessionHandle,
}

impl BroadcastService {
    pub(super) fn new(session: SessionHandle, shutdown: CancelToken, requested: bool) -> Self {
        Self {
            session,
            shutdown,
            phase: if requested {
                Phase::Requested
            } else {
                Phase::Off
            },
        }
    }

    pub(super) fn poll(&mut self) {
        if !matches!(self.phase, Phase::Requested) {
            return;
        }
        let config = match measured_config(&self.session) {
            Ok(Some(config)) => config,
            Ok(None) => return,
            Err(error) => {
                tracing::error!(%error, "broadcast sample-rate query failed");
                self.phase = Phase::Off;
                return;
            }
        };
        match start(&self.session, &self.shutdown, &config) {
            Ok(state) => {
                tracing::info!(url = state.handle.url(), "broadcast is live");
                self.phase = Phase::Running { _state: state };
            }
            Err(error) => {
                tracing::error!(%error, "broadcast did not start");
                self.phase = Phase::Off;
            }
        }
    }
}

impl BroadcastState {
    const CHANNELS: usize = 2;
    const RING_SECONDS: usize = 2;
}

impl Drop for BroadcastState {
    fn drop(&mut self) {
        if let Err(error) = self.session.exec_ok(Cmd::DisableMixTap) {
            tracing::error!(%error, "failed to release broadcast mix tap");
        }
    }
}

fn start(
    session: &SessionHandle,
    shutdown: &CancelToken,
    config: &BroadcastConfig,
) -> BroadcastResult<BroadcastState> {
    let sample_rate = config.sample_rate;
    let capacity = ring_capacity(sample_rate)?;
    let (producer, consumer) = HeapRb::<f32>::new(capacity).split();
    let drops = Arc::new(AtomicU64::new(0));

    session.exec_ok(Cmd::EnableMixTap {
        writer: MixTapWriter::new(producer, Arc::clone(&drops)),
    })?;

    let feed = RingFeed::new(consumer, drops);
    match Broadcast::start(config, feed, Some(shutdown.child())) {
        Ok(handle) => Ok(BroadcastState {
            handle,
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

fn measured_config(session: &SessionHandle) -> BroadcastResult<Option<BroadcastConfig>> {
    Ok(session
        .measured_sample_rate()?
        .map(|sample_rate| BroadcastConfig::builder().sample_rate(sample_rate).build()))
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
    use kithara::play::{PlayError, Reply, SessionDispatcher};
    use kithara_platform::sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    };

    use super::*;

    struct SampleRateSession(AtomicU32);

    impl SessionDispatcher for SampleRateSession {
        fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
            assert!(matches!(cmd, Cmd::QuerySampleRate));
            let sample_rate = self.0.load(Ordering::Relaxed);
            Ok(Reply::SampleRate((sample_rate != 0).then_some(sample_rate)))
        }
    }

    #[test]
    fn configuration_waits_for_the_measured_session_sample_rate() {
        let dispatcher = Arc::new(SampleRateSession(AtomicU32::new(0)));
        let session = SessionHandle::new(dispatcher.clone());
        let mut service = BroadcastService::new(session.clone(), CancelToken::root(), true);

        service.poll();
        assert!(matches!(service.phase, Phase::Requested));
        assert!(measured_config(&session).unwrap().is_none());

        dispatcher.0.store(48_000, Ordering::Relaxed);
        let config = measured_config(&session)
            .unwrap()
            .expect("measured sample rate");

        assert_eq!(config.sample_rate, 48_000);
    }

    #[test]
    fn missing_session_sample_rate_is_rejected_before_ring_creation() {
        assert!(ring_capacity(0).is_err());
    }
}
