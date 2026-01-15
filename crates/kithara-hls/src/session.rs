//! HLS session management.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::{
    events::{EventEmitter, HlsEvent},
    fetch::FetchManager,
    source_adapter::HlsSource,
    stream::SegmentStream,
};

/// HLS playback session.
pub struct HlsSession {
    base_stream: Option<SegmentStream>,
    fetch: Arc<FetchManager>,
    events: Arc<EventEmitter>,
}

impl HlsSession {
    pub(crate) fn new(
        base_stream: SegmentStream,
        fetch: Arc<FetchManager>,
        events: Arc<EventEmitter>,
    ) -> Self {
        Self {
            base_stream: Some(base_stream),
            fetch,
            events,
        }
    }

    /// Subscribe to HLS events.
    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.events.subscribe()
    }

    /// Create Source for random-access reading. Consumes the session.
    pub fn source(mut self) -> HlsSource {
        let base_stream = self.base_stream.take().expect("source() called twice");
        HlsSource::new(base_stream, self.fetch, self.events)
    }
}
