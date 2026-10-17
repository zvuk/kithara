use std::error::Error;

use crate::pools::AppHost;

pub(crate) type BroadcastResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// What the bar needs from a packager. `Live` has no values where the
/// `broadcast` feature is off, which makes the running phase unconstructable.
pub(crate) trait Packager: 'static {
    type Config: Clone + Send + 'static;
    type Live: Send + 'static;

    /// Whether this build carries a packager at all. The UI reads it instead
    /// of gating its own call sites on the feature.
    const IS_AVAILABLE: bool;

    fn is_live(live: &Self::Live) -> bool;

    /// Releases the Host output group before the packager drains.
    fn release(host: &AppHost) -> BroadcastResult<()>;

    /// `Ok(None)`: no device rate measured yet, so the request stands.
    fn start(host: &AppHost, config: &Self::Config) -> BroadcastResult<Option<Self::Live>>;

    /// Drains the stream and shuts it down. Blocking.
    fn stop(live: Self::Live);

    fn url(live: &Self::Live) -> &str;
}

#[cfg(test)]
mod tests {
    use kithara::host::HostConfig;

    use super::{
        super::{
            absent::Absent,
            broadcaster::{Broadcaster, Phase},
            ready::Ready,
            unmeasured::Unmeasured,
        },
        AppHost, Packager,
    };

    fn broadcaster<P>() -> Broadcaster<P>
    where
        P: Packager,
        P::Config: Default,
    {
        Broadcaster::new(P::Config::default())
    }

    fn host() -> AppHost {
        let pools = crate::pools::build(&crate::pools::PoolsSection::default()).expect("app pools");
        AppHost::new(HostConfig::offline(pools).build()).expect("test host")
    }

    #[kithara::test]
    fn a_request_without_a_measured_rate_keeps_asking() {
        let mut bar = broadcaster::<Unmeasured>();
        let host = host();

        bar.toggle(&host);
        bar.poll(&host);
        bar.poll(&host);

        assert!(matches!(bar.phase, Phase::Requested));
    }

    #[kithara::test]
    fn the_bar_asks_its_packager_whether_this_build_can_go_on_air() {
        assert!(Broadcaster::<Ready>::is_available());
        assert!(
            !Broadcaster::<Absent>::is_available(),
            "a build with no packager offers no air controls",
        );
    }

    #[kithara::test]
    fn a_request_no_packager_can_serve_returns_the_bar_to_off() {
        let mut bar = broadcaster::<Absent>();
        let host = host();

        bar.toggle(&host);
        bar.poll(&host);

        assert!(matches!(bar.phase, Phase::Off));
        assert!(!bar.is_on_air());
    }

    #[kithara::test]
    fn a_served_request_puts_the_bar_on_air_with_the_stream_url() {
        let mut bar = broadcaster::<Ready>();
        let host = host();

        bar.toggle(&host);
        assert!(!bar.is_on_air(), "a request is not yet a stream");
        bar.poll(&host);

        assert!(bar.is_on_air());
        assert_eq!(bar.url(), Some(Ready::URL));
    }

    #[kithara::test]
    fn stopping_hands_over_a_job_and_finishes_only_on_completion() {
        let mut bar = broadcaster::<Ready>();
        let host = host();
        bar.toggle(&host);
        bar.poll(&host);

        let stop = bar
            .toggle(&host)
            .expect("a running stream hands over its stop");
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
    fn toggling_a_pending_request_withdraws_it() {
        let mut bar = broadcaster::<Unmeasured>();
        let host = host();
        bar.toggle(&host);

        assert!(
            bar.toggle(&host).is_none(),
            "there is no stream to stop yet"
        );
        assert!(matches!(bar.phase, Phase::Off));

        bar.poll(&host);
        assert!(
            matches!(bar.phase, Phase::Off),
            "a withdrawn request must not start on the next frame"
        );
    }
}
