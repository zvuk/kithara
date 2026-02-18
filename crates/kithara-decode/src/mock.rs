use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use kithara_bufpool::pcm_pool;
use unimock::{MockFn, Unimock, matching};

pub use crate::traits::InnerDecoderMock;
use crate::{DecodeResult, InnerDecoder, PcmChunk, PcmMeta, PcmSpec};

/// Minimal mutex wrapper with infallible `lock()` for tests.
pub struct MockLog<T> {
    inner: std::sync::Mutex<T>,
}

impl<T> MockLog<T> {
    #[must_use]
    pub fn new(value: T) -> Self {
        Self {
            inner: std::sync::Mutex::new(value),
        }
    }

    /// # Panics
    ///
    /// Panics if the mutex is poisoned.
    pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.inner
            .lock()
            .expect("decoder mock log mutex should not be poisoned")
    }
}

/// Shared logs for seek and byte-length updates.
#[derive(Clone)]
pub struct InnerDecoderLogs {
    byte_len_log: Arc<MockLog<Vec<u64>>>,
    seek_log: Arc<MockLog<Vec<Duration>>>,
}

impl InnerDecoderLogs {
    #[must_use]
    pub fn byte_len_log(&self) -> Arc<MockLog<Vec<u64>>> {
        Arc::clone(&self.byte_len_log)
    }

    #[must_use]
    pub fn seek_log(&self) -> Arc<MockLog<Vec<Duration>>> {
        Arc::clone(&self.seek_log)
    }
}

/// Create a scripted decoder mock backed by unimock.
///
/// `next_chunk()` yields from `chunks` then returns EOF.
/// `seek()` logs positions and consumes preconfigured results in order.
#[must_use]
pub fn scripted_inner_decoder(
    spec: PcmSpec,
    chunks: Vec<PcmChunk>,
    seek_results: Vec<DecodeResult<()>>,
    duration: Option<Duration>,
) -> (Box<dyn InnerDecoder>, InnerDecoderLogs) {
    let chunk_queue = Arc::new(MockLog::new(VecDeque::from(chunks)));
    let seek_queue = Arc::new(MockLog::new(VecDeque::from(seek_results)));
    let seek_log = Arc::new(MockLog::new(Vec::new()));
    let byte_len_log = Arc::new(MockLog::new(Vec::new()));

    let next_chunk_queue = Arc::clone(&chunk_queue);
    let seek_results_queue = Arc::clone(&seek_queue);
    let seek_log_for_seek = Arc::clone(&seek_log);
    let byte_len_log_for_update = Arc::clone(&byte_len_log);

    let mock = Unimock::new((
        InnerDecoderMock::next_chunk
            .each_call(matching!())
            .answers_arc(Arc::new(move |_| Ok(next_chunk_queue.lock().pop_front())))
            .at_least_times(0),
        InnerDecoderMock::spec
            .each_call(matching!())
            .returns(spec)
            .at_least_times(0),
        InnerDecoderMock::seek
            .each_call(matching!(_))
            .answers_arc(Arc::new(move |_, pos| {
                seek_log_for_seek.lock().push(pos);
                if let Some(result) = seek_results_queue.lock().pop_front() {
                    return result;
                }
                Ok(())
            }))
            .at_least_times(0),
        InnerDecoderMock::update_byte_len
            .each_call(matching!(_))
            .answers_arc(Arc::new(move |_, len| {
                byte_len_log_for_update.lock().push(len);
            }))
            .at_least_times(0),
        InnerDecoderMock::duration
            .each_call(matching!())
            .returns(duration)
            .at_least_times(0),
    ))
    .no_verify_in_drop();

    (
        Box::new(mock),
        InnerDecoderLogs {
            byte_len_log,
            seek_log,
        },
    )
}

/// Create an infinite decoder mock backed by unimock.
///
/// Produces fixed-size chunks until `stop` becomes true.
#[must_use]
pub fn infinite_inner_decoder(
    spec: PcmSpec,
    stop: Arc<AtomicBool>,
) -> (Box<dyn InnerDecoder>, InnerDecoderLogs) {
    let seek_log = Arc::new(MockLog::new(Vec::new()));
    let byte_len_log = Arc::new(MockLog::new(Vec::new()));

    let seek_log_for_seek = Arc::clone(&seek_log);
    let byte_len_log_for_update = Arc::clone(&byte_len_log);

    let mock = Unimock::new((
        InnerDecoderMock::next_chunk
            .each_call(matching!())
            .answers_arc(Arc::new(move |_| {
                if stop.load(Ordering::Acquire) {
                    return Ok(None);
                }
                Ok(Some(PcmChunk::new(
                    PcmMeta {
                        spec,
                        ..Default::default()
                    },
                    pcm_pool().attach(vec![0.5; 1024]),
                )))
            }))
            .at_least_times(0),
        InnerDecoderMock::spec
            .each_call(matching!())
            .returns(spec)
            .at_least_times(0),
        InnerDecoderMock::seek
            .each_call(matching!(_))
            .answers_arc(Arc::new(move |_, pos| {
                seek_log_for_seek.lock().push(pos);
                Ok(())
            }))
            .at_least_times(0),
        InnerDecoderMock::update_byte_len
            .each_call(matching!(_))
            .answers_arc(Arc::new(move |_, len| {
                byte_len_log_for_update.lock().push(len);
            }))
            .at_least_times(0),
        InnerDecoderMock::duration
            .each_call(matching!())
            .returns(Some(Duration::from_secs(220)))
            .at_least_times(0),
    ))
    .no_verify_in_drop();

    (
        Box::new(mock),
        InnerDecoderLogs {
            byte_len_log,
            seek_log,
        },
    )
}
