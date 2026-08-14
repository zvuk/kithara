use std::{error::Error, mem};

use kithara::play::SessionHandle;
use kithara_platform::{
    CancelToken,
    time::{Duration, Instant},
    tokio::task,
};

pub(crate) type BroadcastResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// What the bar needs from a packager. `Live` has no values where the
/// `broadcast` feature is off, which makes [`Phase::Running`] unconstructable.
pub(crate) trait Packager: 'static {
    type Live: Send + 'static;

    fn is_live(live: &Self::Live) -> bool;

    /// `Ok(None)`: no device rate measured yet, so the request stands.
    fn start(
        session: &SessionHandle,
        shutdown: &CancelToken,
    ) -> BroadcastResult<Option<Self::Live>>;

    /// Drains the stream and shuts it down. Blocking.
    fn stop(live: Self::Live);

    fn url(live: &Self::Live) -> &str;
}

pub(crate) struct Broadcaster<P: Packager> {
    shutdown: CancelToken,
    phase: Phase<P>,
    session: SessionHandle,
}

enum Phase<P: Packager> {
    Off,
    Requested,
    Running { live: P::Live },
    Stopping,
}

/// A stream handed over for shutdown; the drain runs off the frame loop.
pub(crate) struct BroadcastStop<P: Packager>(P::Live);

impl<P: Packager> Broadcaster<P> {
    pub(crate) const fn new(session: SessionHandle, shutdown: CancelToken) -> Self {
        Self {
            session,
            shutdown,
            phase: Phase::Off,
        }
    }

    pub(crate) fn complete_stop(&mut self) {
        if matches!(self.phase, Phase::Stopping) {
            self.phase = Phase::Off;
        }
    }

    /// Serving. A pending request and a draining stop are both off air.
    pub(crate) const fn is_on_air(&self) -> bool {
        matches!(self.phase, Phase::Running { .. })
    }

    pub(crate) fn poll(&mut self) {
        if matches!(&self.phase, Phase::Running { live } if !P::is_live(live)) {
            self.phase = Phase::Off;
            return;
        }
        if !matches!(self.phase, Phase::Requested) {
            return;
        }
        match P::start(&self.session, &self.shutdown) {
            Ok(Some(live)) => {
                tracing::info!(url = P::url(&live), "broadcast is live");
                self.phase = Phase::Running { live };
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%error, "broadcast did not start");
                self.phase = Phase::Off;
            }
        }
    }

    pub(crate) fn toggle(&mut self) -> Option<BroadcastStop<P>> {
        match mem::replace(&mut self.phase, Phase::Off) {
            Phase::Off => self.phase = Phase::Requested,
            Phase::Requested => {}
            Phase::Running { live } => {
                self.phase = Phase::Stopping;
                return Some(BroadcastStop(live));
            }
            Phase::Stopping => self.phase = Phase::Stopping,
        }
        None
    }

    pub(crate) fn url(&self) -> Option<&str> {
        match &self.phase {
            Phase::Running { live } => Some(P::url(live)),
            Phase::Off | Phase::Requested | Phase::Stopping => None,
        }
    }
}

impl<P: Packager> BroadcastStop<P> {
    pub(crate) async fn run(self) -> Option<Duration> {
        let drain = task::spawn_blocking(move || {
            let started = Instant::now();
            P::stop(self.0);
            started.elapsed()
        });
        match drain.await {
            Ok(duration) => Some(duration),
            Err(error) => {
                tracing::error!(%error, "broadcast stop worker failed");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara::{
        audio::ConsumerWakeMode,
        play::{Cmd, PlayError, Reply, SessionDispatcher},
    };
    use kithara_platform::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;

    /// Fails loudly: only a packager may reach the session, never the phases.
    struct NoSession;

    impl SessionDispatcher for NoSession {
        fn consumer_wake_mode(&self) -> ConsumerWakeMode {
            ConsumerWakeMode::RealtimeDeferred
        }

        fn exec(&self, _cmd: Cmd) -> Result<Reply, PlayError> {
            panic!("the state machine must not reach the session")
        }
    }

    fn broadcaster<P: Packager>() -> Broadcaster<P> {
        Broadcaster::new(SessionHandle::new(Arc::new(NoSession)), CancelToken::root())
    }

    struct Stream(String);

    /// Raised by `start`, so lowering it in one test cannot leak into the next.
    static LIVE: AtomicBool = AtomicBool::new(true);

    struct Ready;

    impl Ready {
        const URL: &str = "http://packager.test/master.m3u8";

        fn stream() -> Stream {
            Stream(Self::URL.to_owned())
        }
    }

    impl Packager for Ready {
        type Live = Stream;

        fn is_live(_live: &Stream) -> bool {
            LIVE.load(Ordering::Relaxed)
        }

        fn start(
            _session: &SessionHandle,
            _shutdown: &CancelToken,
        ) -> BroadcastResult<Option<Stream>> {
            LIVE.store(true, Ordering::Relaxed);
            Ok(Some(Self::stream()))
        }

        fn stop(_live: Stream) {}

        fn url(live: &Stream) -> &str {
            &live.0
        }
    }

    struct Unmeasured;

    impl Packager for Unmeasured {
        type Live = Stream;

        fn is_live(_live: &Stream) -> bool {
            true
        }

        fn start(
            _session: &SessionHandle,
            _shutdown: &CancelToken,
        ) -> BroadcastResult<Option<Stream>> {
            Ok(None)
        }

        fn stop(_live: Stream) {}

        fn url(live: &Stream) -> &str {
            &live.0
        }
    }

    struct Absent;

    impl Packager for Absent {
        type Live = Stream;

        fn is_live(_live: &Stream) -> bool {
            true
        }

        fn start(
            _session: &SessionHandle,
            _shutdown: &CancelToken,
        ) -> BroadcastResult<Option<Stream>> {
            Err("no packager in this build".into())
        }

        fn stop(_live: Stream) {}

        fn url(live: &Stream) -> &str {
            &live.0
        }
    }

    #[kithara::test]
    fn a_request_without_a_measured_rate_keeps_asking() {
        let mut bar = broadcaster::<Unmeasured>();

        bar.toggle();
        bar.poll();
        bar.poll();

        assert!(matches!(bar.phase, Phase::Requested));
    }

    #[kithara::test]
    fn a_request_no_packager_can_serve_returns_the_bar_to_off() {
        let mut bar = broadcaster::<Absent>();

        bar.toggle();
        bar.poll();

        assert!(matches!(bar.phase, Phase::Off));
        assert!(!bar.is_on_air());
    }

    #[kithara::test]
    fn a_served_request_puts_the_bar_on_air_with_the_stream_url() {
        let mut bar = broadcaster::<Ready>();

        bar.toggle();
        assert!(!bar.is_on_air(), "a request is not yet a stream");
        bar.poll();

        assert!(bar.is_on_air());
        assert_eq!(bar.url(), Some(Ready::URL));
    }

    #[kithara::test]
    fn stopping_hands_over_a_job_and_finishes_only_on_completion() {
        let mut bar = broadcaster::<Ready>();
        bar.toggle();
        bar.poll();

        let stop = bar.toggle().expect("a running stream hands over its stop");
        assert!(matches!(bar.phase, Phase::Stopping));
        assert!(!bar.is_on_air());

        Ready::stop(stop.0);
        assert!(
            matches!(bar.phase, Phase::Stopping),
            "the bar waits for the drain to report back"
        );

        bar.complete_stop();
        assert!(matches!(bar.phase, Phase::Off));
    }

    #[kithara::test]
    fn a_stream_that_ends_on_its_own_is_noticed_by_the_next_poll() {
        let mut bar = broadcaster::<Ready>();
        bar.toggle();
        bar.poll();
        assert!(bar.is_on_air());

        LIVE.store(false, Ordering::Relaxed);
        bar.poll();

        assert!(matches!(bar.phase, Phase::Off));
    }

    #[kithara::test]
    fn toggling_a_pending_request_withdraws_it() {
        let mut bar = broadcaster::<Unmeasured>();
        bar.toggle();

        assert!(bar.toggle().is_none(), "there is no stream to stop yet");
        assert!(matches!(bar.phase, Phase::Off));

        bar.poll();
        assert!(
            matches!(bar.phase, Phase::Off),
            "a withdrawn request must not start on the next frame"
        );
    }
}
