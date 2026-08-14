use kithara::play::SessionHandle;
use kithara_platform::CancelToken;

use super::state::{BroadcastResult, Packager};

pub(crate) struct Backend;

/// No values: the packager is not in this build, so the on-air phase cannot be
/// constructed and the matches below have nothing to match.
pub(crate) enum Stream {}

impl Packager for Backend {
    type Live = Stream;

    fn is_live(live: &Stream) -> bool {
        match *live {}
    }

    fn start(_session: &SessionHandle, _shutdown: &CancelToken) -> BroadcastResult<Option<Stream>> {
        Err("this build carries no broadcaster; rebuild with `--features broadcast`".into())
    }

    fn stop(live: Stream) {
        match live {}
    }

    fn url(live: &Stream) -> &str {
        match *live {}
    }
}
