use kithara_platform::{sync::Arc, time::Duration};

use crate::NetError;

pub trait NetObserver: Send + Sync {
    fn body_resumed(&self, _resume_number: u32, _from_offset: u64, _honoured_range: bool) {}

    fn body_stalled(&self, _consumed: u64, _expected: Option<u64>, _stall: Duration) {}

    fn first_byte(&self, _ttfb: Duration, _status: u16, _partial: bool) {}

    fn retry_exhausted(&self, _max_retries: u32, _consumed: u64, _error: &NetError) {}

    fn retrying(&self, _attempt: u32, _max_retries: u32, _error: &NetError, _backoff: Duration) {}
}

#[derive(Clone)]
pub struct Observer(pub Arc<dyn NetObserver>);

impl std::fmt::Debug for Observer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Observer").finish()
    }
}
