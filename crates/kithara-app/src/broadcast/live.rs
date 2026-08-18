use kithara::{
    broadcast::{Broadcast, BroadcastConfig, BroadcastHandle, RingFeed},
    play::{Cmd, MixTapWriter, SessionHandle},
};
use kithara_platform::{
    CancelToken,
    sync::{Arc, atomic::AtomicU64},
};
use ringbuf::{HeapRb, traits::Split};

use super::state::{BroadcastResult, Packager};

pub(crate) struct Backend;

/// The running stream and the session feeding it; `Drop` releases the mix tap.
pub(crate) struct Stream {
    handle: BroadcastHandle,
    session: SessionHandle,
}

impl Packager for Backend {
    type Live = Stream;

    const IS_AVAILABLE: bool = true;

    fn is_live(live: &Stream) -> bool {
        live.handle.status().is_live
    }

    fn start(session: &SessionHandle, shutdown: &CancelToken) -> BroadcastResult<Option<Stream>> {
        let Some(config) = measured_config(session)? else {
            return Ok(None);
        };
        start(session, shutdown, &config).map(Some)
    }

    fn stop(live: Stream) {
        live.handle.stop();
    }

    fn url(live: &Stream) -> &str {
        live.handle.url()
    }
}

struct Ring;

impl Ring {
    const CHANNELS: usize = 2;
    const SECONDS: usize = 2;

    /// Interleaved samples the mix tap may run ahead of the packager by.
    fn capacity(sample_rate: usize) -> Option<usize> {
        sample_rate
            .checked_mul(Self::CHANNELS)
            .and_then(|capacity| capacity.checked_mul(Self::SECONDS))
    }
}

impl Drop for Stream {
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
) -> BroadcastResult<Stream> {
    let sample_rate = config.sample_rate;
    let capacity = ring_capacity(sample_rate)?;
    let (producer, consumer) = HeapRb::<f32>::new(capacity).split();
    let drops = Arc::new(AtomicU64::new(0));

    session.exec_ok(Cmd::EnableMixTap {
        writer: MixTapWriter::new(producer, Arc::clone(&drops)),
    })?;

    let feed = RingFeed::new(consumer, drops);
    match Broadcast::start(config, feed, Some(shutdown.child())) {
        Ok(handle) => Ok(Stream {
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
        .sample_rate()?
        .measured
        .map(|sample_rate| BroadcastConfig::builder().sample_rate(sample_rate).build()))
}

fn ring_capacity(sample_rate: u32) -> BroadcastResult<usize> {
    let sample_rate = usize::try_from(sample_rate)?;
    if sample_rate == 0 {
        return Err("session returned zero sample rate".into());
    }

    Ring::capacity(sample_rate).ok_or_else(|| "broadcast ring capacity overflow".into())
}

#[cfg(test)]
mod tests {
    use kithara::{
        audio::ConsumerWakeMode,
        play::{PlayError, Reply, SessionDispatcher, SessionSampleRate},
    };
    use kithara_platform::{
        sync::{
            Mutex,
            atomic::{AtomicU32, Ordering},
        },
        thread,
        time::Duration,
    };

    use super::*;

    struct SampleRateSession {
        sample_rate: AtomicU32,
        tap: Mutex<Option<MixTapWriter>>,
    }

    impl SampleRateSession {
        /// Requested, not measured: the broadcast reads only the measured rate.
        const REQUESTED_RATE: u32 = 44_100;

        fn new(sample_rate: u32) -> Self {
            Self {
                sample_rate: AtomicU32::new(sample_rate),
                tap: Mutex::new(None),
            }
        }
    }

    impl SessionDispatcher for SampleRateSession {
        fn consumer_wake_mode(&self) -> ConsumerWakeMode {
            ConsumerWakeMode::RealtimeDeferred
        }

        fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
            match cmd {
                Cmd::QuerySampleRate => {
                    let sample_rate = self.sample_rate.load(Ordering::Relaxed);
                    Ok(Reply::SampleRate(SessionSampleRate::new(
                        (sample_rate != 0).then_some(sample_rate),
                        Self::REQUESTED_RATE,
                    )))
                }
                Cmd::EnableMixTap { writer } => {
                    *self.tap.lock() = Some(writer);
                    Ok(Reply::Ok)
                }
                Cmd::DisableMixTap => {
                    self.tap.lock().take();
                    Ok(Reply::Ok)
                }
                _ => panic!("unexpected session command"),
            }
        }
    }

    fn on_air(sample_rate: u32) -> (Stream, Arc<SampleRateSession>, CancelToken) {
        let dispatcher = Arc::new(SampleRateSession::new(sample_rate));
        let session = SessionHandle::new(dispatcher.clone());
        let shutdown = CancelToken::root();
        let stream = Backend::start(&session, &shutdown)
            .expect("the packager starts")
            .expect("a measured rate yields a stream");
        (stream, dispatcher, shutdown)
    }

    #[kithara::test]
    fn configuration_waits_for_the_measured_session_sample_rate() {
        let dispatcher = Arc::new(SampleRateSession::new(0));
        let session = SessionHandle::new(dispatcher.clone());

        assert!(measured_config(&session).unwrap().is_none());
        assert!(
            Backend::start(&session, &CancelToken::root())
                .unwrap()
                .is_none(),
            "an unmeasured rate is a retry, not a failure"
        );

        dispatcher.sample_rate.store(48_000, Ordering::Relaxed);
        let config = measured_config(&session)
            .unwrap()
            .expect("measured sample rate");

        assert_eq!(config.sample_rate, 48_000);
    }

    #[kithara::test]
    fn starting_takes_the_mix_tap_and_stopping_gives_it_back() {
        let (stream, dispatcher, shutdown) = on_air(48_000);
        assert!(
            dispatcher.tap.lock().is_some(),
            "the running stream holds the session's mix tap"
        );

        Backend::stop(stream);

        assert!(
            dispatcher.tap.lock().is_none(),
            "the drained stream returns the mix tap"
        );
        shutdown.cancel();
    }

    #[kithara::test]
    fn a_dropped_producer_ends_the_stream() {
        let (stream, dispatcher, shutdown) = on_air(48_000);
        dispatcher.tap.lock().take();

        for _ in 0..1_000 {
            if !Backend::is_live(&stream) {
                break;
            }
            thread::paced_backoff(Duration::from_millis(1));
        }

        assert!(!Backend::is_live(&stream));
        shutdown.cancel();
    }

    #[kithara::test]
    fn missing_session_sample_rate_is_rejected_before_ring_creation() {
        assert!(ring_capacity(0).is_err());
    }
}
