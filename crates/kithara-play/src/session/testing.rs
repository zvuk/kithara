use std::{cell::RefCell, num::NonZeroU32, sync::Arc};

use firewheel::{FirewheelCtx, StreamInfo, backend::AudioBackend, processor::FirewheelProcessor};

use super::{Cmd, Reply, SessionDispatcher, SessionState, run_cmd};
use crate::error::PlayError;

struct NoopBackend {
    _processor: Option<FirewheelProcessor<Self>>,
}

#[derive(Clone)]
struct NoopConfig {
    sample_rate: u32,
}

impl Default for NoopConfig {
    fn default() -> Self {
        Self {
            sample_rate: SessionState::<NoopBackend>::DEFAULT_SAMPLE_RATE,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("noop backend error")]
struct NoopBackendError;

impl AudioBackend for NoopBackend {
    type Config = NoopConfig;
    type Enumerator = ();
    type Instant = kithara_platform::time::Instant;
    type StartStreamError = NoopBackendError;
    type StreamError = NoopBackendError;

    fn delay_from_last_process(
        &self,
        _process_timestamp: Self::Instant,
    ) -> Option<kithara_platform::time::Duration> {
        None
    }

    fn enumerator() -> Self::Enumerator {}

    fn poll_status(&mut self) -> Result<(), Self::StreamError> {
        Ok(())
    }

    fn set_processor(&mut self, processor: FirewheelProcessor<Self>) {
        self._processor = Some(processor);
    }

    fn start_stream(config: Self::Config) -> Result<(Self, StreamInfo), Self::StartStreamError> {
        let sample_rate = NonZeroU32::new(config.sample_rate).ok_or(NoopBackendError)?;
        let max_block_frames = NonZeroU32::new(512).ok_or(NoopBackendError)?;
        let stream_info = StreamInfo {
            sample_rate,
            sample_rate_recip: 1.0 / f64::from(config.sample_rate),
            prev_sample_rate: sample_rate,
            max_block_frames,
            num_stream_in_channels: 0,
            num_stream_out_channels: 2,
            input_to_output_latency_seconds: 0.0,
            declick_frames: max_block_frames,
            output_device_id: String::from("test-noop"),
            input_device_id: None,
        };
        Ok((Self { _processor: None }, stream_info))
    }
}

fn start_noop_stream(ctx: &mut FirewheelCtx<NoopBackend>, sample_rate: u32) -> Result<(), String> {
    ctx.start_stream(NoopConfig { sample_rate })
        .map_err(|err| err.to_string())
}

thread_local! {
    static TEST_STATE: RefCell<SessionState<NoopBackend>> =
        RefCell::new(SessionState::new(start_noop_stream));
}

struct TestSession;

impl SessionDispatcher for TestSession {
    fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        TEST_STATE.with(|state| {
            let mut state = state
                .try_borrow_mut()
                .map_err(|_| PlayError::Internal("test session is already borrowed".into()))?;
            Ok(run_cmd(&mut state, cmd))
        })
    }
}

pub(crate) fn test_session() -> Arc<dyn SessionDispatcher> {
    TEST_STATE.with(|state| state.replace(SessionState::new(start_noop_stream)));
    Arc::new(TestSession)
}
