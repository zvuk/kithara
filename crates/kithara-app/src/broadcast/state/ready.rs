use std::sync::atomic::{AtomicBool, Ordering};

use kithara::host::HostConfig;

use super::{
    BroadcastResult, Packager,
    broadcaster::{Broadcaster, Phase},
    fixture::Stream,
};
use crate::pools::AppHost;

pub(super) struct Ready;

/// Raised by `start`, so lowering it in one test cannot leak into the next.
static LIVE: AtomicBool = AtomicBool::new(true);

impl Ready {
    pub(super) const URL: &str = "http://packager.test/master.m3u8";

    fn end_stream() {
        LIVE.store(false, Ordering::Relaxed);
    }

    fn stream() -> Stream {
        Stream(Self::URL.to_owned())
    }
}

impl Packager for Ready {
    type Config = ();
    type Live = Stream;

    const IS_AVAILABLE: bool = true;

    fn is_live(_live: &Stream) -> bool {
        LIVE.load(Ordering::Relaxed)
    }

    fn release(_host: &AppHost) -> BroadcastResult<()> {
        Ok(())
    }

    fn start(_host: &AppHost, _config: &()) -> BroadcastResult<Option<Stream>> {
        LIVE.store(true, Ordering::Relaxed);
        Ok(Some(Self::stream()))
    }

    fn stop(_live: Stream) {}

    fn url(live: &Stream) -> &str {
        &live.0
    }
}

#[kithara::test]
fn an_ended_stream_is_noticed_by_the_next_poll() {
    let mut broadcaster = Broadcaster::<Ready>::new(());
    let pools = crate::pools::build(&crate::pools::PoolsSection::default()).expect("app pools");
    let host = crate::pools::host(HostConfig::offline(pools).build()).expect("test host");
    broadcaster.toggle(&host);
    broadcaster.poll(&host);
    assert!(broadcaster.is_on_air());

    Ready::end_stream();
    broadcaster.poll(&host);

    assert!(matches!(broadcaster.phase, Phase::Off));
}
