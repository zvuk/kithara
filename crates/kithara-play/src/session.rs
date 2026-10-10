mod wire {
    use kithara_render::rt::BufferGeometryError;
    use kithara_warp::{BeatGridId, BeatGridIdAllocationError};

    pub type PlayerId = u64;

    #[derive(Debug, Clone, thiserror::Error)]
    #[non_exhaustive]
    pub enum SessionError {
        #[error("player not found: {0}")]
        PlayerNotFound(PlayerId),
        #[error("player identity space is exhausted")]
        PlayerIdExhausted,
        #[error("player already started: {0}")]
        AlreadyStarted(PlayerId),
        #[error("player not running: {0}")]
        NotRunning(PlayerId),
        #[error("session context not initialised")]
        NoContext,
        #[error("stream start failed: {0}")]
        StreamStart(String),
        #[error("graph edit failed: {0}")]
        Graph(String),
        #[error("session output tap already has a consumer")]
        TapActive,
        #[error("session transport has not been processed")]
        TransportNotProcessed,
        #[error("host command queue is full")]
        HostQueueFull,
        #[error(transparent)]
        BufferGeometry(#[from] BufferGeometryError),
        #[error("deck {0:?} is not in this session")]
        DeckNotFound(BeatGridId),
        #[error("deck {0:?} is already in this session")]
        DeckAttached(BeatGridId),
        #[error(transparent)]
        BeatGridIdAllocation(#[from] BeatGridIdAllocationError),
        #[error("stream stopped: {reason}; restart failed: {source}")]
        RestartFailed { reason: String, r#source: String },
    }

    /// What the session knows about its output rate.
    #[derive(Clone, Copy, Debug)]
    #[non_exhaustive]
    pub struct SessionSampleRate {
        /// The current Firewheel output rate; `None` means no output is measured.
        pub measured: Option<u32>,
        /// The rate the session last asked the device for.
        pub requested: u32,
    }

    impl SessionSampleRate {
        #[must_use]
        pub const fn new(measured: Option<u32>, requested: u32) -> Self {
            Self {
                measured,
                requested,
            }
        }

        /// The rate to build a resampler for.
        #[must_use]
        pub const fn output(self) -> u32 {
            match self.measured {
                Some(measured) => measured,
                None => self.requested,
            }
        }
    }
}

mod binding {
    use std::num::NonZeroU32;

    use arc_swap::ArcSwap;
    use kithara_platform::sync::Arc;
    use kithara_render::rt::StreamShape;

    use super::wire::SessionSampleRate;

    /// One publish of a session's output: the rate and the shape a deck
    /// reads together.
    #[derive(Clone, Copy, Debug)]
    pub struct OutputSnapshot {
        /// The rate the running backend settled on, beside the one the
        /// settings ask for.
        pub sample_rate: SessionSampleRate,
        /// The measured stream once one runs, the requested block before.
        pub stream_shape: Option<StreamShape>,
    }

    /// Where a session publishes its output for the decks it holds to read.
    #[derive(Clone)]
    pub struct SessionOutputView(Arc<ArcSwap<OutputSnapshot>>);

    impl SessionOutputView {
        /// A session that has published no output yet: nothing measured, no
        /// shape, and the rate its settings ask for.
        #[must_use]
        pub fn new(requested_sample_rate: NonZeroU32) -> Self {
            Self(Arc::new(ArcSwap::from_pointee(OutputSnapshot {
                sample_rate: SessionSampleRate::new(None, requested_sample_rate.get()),
                stream_shape: None,
            })))
        }

        /// The session's output changed: every deck it holds reads this from
        /// now on.
        pub fn publish(&self, sample_rate: SessionSampleRate, stream_shape: Option<StreamShape>) {
            self.0.store(Arc::new(OutputSnapshot {
                sample_rate,
                stream_shape,
            }));
        }

        /// The output the session last published, rate and shape from the
        /// same publish.
        #[must_use]
        pub fn get(&self) -> OutputSnapshot {
            **self.0.load()
        }
    }
}

pub use binding::{OutputSnapshot, SessionOutputView};
pub use wire::{PlayerId, SessionError, SessionSampleRate};

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::{TestTempDir, bufpool::pools, kithara};

    use super::{SessionOutputView, SessionSampleRate};

    fn sample_rate() -> NonZeroU32 {
        NonZeroU32::new(48_000).expect("fixture sample rate is non-zero")
    }

    #[kithara::test(native, tokio)]
    async fn a_view_reads_what_its_session_publishes_after_it_was_taken() {
        let output = SessionOutputView::new(sample_rate());
        let view = output.clone();
        assert_eq!(view.get().sample_rate.measured, None);

        output.publish(SessionSampleRate::new(Some(44_100), 48_000), None);

        assert_eq!(view.get().sample_rate.measured, Some(44_100));
        assert_eq!(view.get().sample_rate.output(), 44_100);
        assert_session_render_off_bus(&view).await;
    }

    async fn assert_session_render_off_bus(view: &SessionOutputView) {
        let pools = pools();
        let dir = TestTempDir::new();
        let prep = crate::ResourcePrep::builder()
            .worker(crate::PlayWorker::new(
                crate::PlayWorkerConfig::builder(pools.clone()).build(),
            ))
            .build();
        crate::mock::assert_prepared_render_off_bus(
            &prep,
            &view.get(),
            &pools,
            &dir.path().join("session.wav"),
        )
        .await
        .expect("session-prepared lane renders off the bus");
    }

    #[kithara::test(native, tokio)]
    async fn session_handle_delegates_explicit_consumer_wake_mode() {
        let view = SessionOutputView::new(sample_rate());
        assert_session_render_off_bus(&view).await;
    }
}
