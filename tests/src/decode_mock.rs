use std::{
    collections::VecDeque,
    sync::{
        Mutex as StdMutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara::{
    decode::{
        DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, DecoderTrackInfo,
        mock::DecoderMock,
    },
    platform::{sync::Arc, time::Duration},
    signal::{AudioChunk, AudioChunkInfo, AudioSpec},
};
use unimock::{MockFn, Unimock, matching};

use crate::bufpool_ext::pools;

/// Minimal mutex wrapper with infallible `lock()` for tests.
pub struct MockLog<T> {
    inner: StdMutex<T>,
}

impl<T> MockLog<T> {
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self {
            inner: StdMutex::new(value),
        }
    }

    /// # Panics
    ///
    /// Panics if the mutex is poisoned.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.inner
            .lock()
            .expect("BUG: decoder mock log mutex should not be poisoned")
    }
}

/// Shared logs for seek and byte-length updates.
#[derive(Clone)]
pub struct DecoderLogs {
    byte_len_log: Arc<MockLog<Vec<u64>>>,
    seek_log: Arc<MockLog<Vec<Duration>>>,
}

impl DecoderLogs {
    #[must_use]
    pub fn byte_len_log(&self) -> Arc<MockLog<Vec<u64>>> {
        Arc::clone(&self.byte_len_log)
    }

    #[must_use]
    pub fn seek_log(&self) -> Arc<MockLog<Vec<Duration>>> {
        Arc::clone(&self.seek_log)
    }
}

/// Options for [`scripted_decoder`].
#[derive(Clone, Default)]
pub struct ScriptedOptions {
    /// `DecoderTrackInfo` exposed by [`Decoder::track_info`]. Use a
    /// non-default value when the test cares about gapless metadata.
    pub track_info: DecoderTrackInfo,
    /// When `true`, `unimock` verifies in `Drop` that every expected
    /// call happened. Set `false` for data-plane tests that don't
    /// exercise every mocked method.
    pub verify_in_drop: bool,
}

/// Create a scripted decoder mock backed by `unimock`.
///
/// `next_chunk()` yields from `chunks` then returns EOF. `seek()` logs
/// positions and consumes preconfigured results in order. When the
/// queue is exhausted, seek returns [`DecoderSeekOutcome::Landed`] at
/// the requested position by default.
#[must_use]
pub fn scripted_decoder(
    spec: AudioSpec,
    chunks: Vec<AudioChunk>,
    seek_results: Vec<DecodeResult<DecoderSeekOutcome>>,
    duration: Option<Duration>,
    options: ScriptedOptions,
) -> (Box<dyn Decoder>, DecoderLogs) {
    let chunk_queue = Arc::new(MockLog::new(VecDeque::from(chunks)));
    let next_chunk_queue = Arc::clone(&chunk_queue);

    build_decoder_mock(
        spec,
        move |_| {
            Ok(next_chunk_queue
                .lock()
                .pop_front()
                .map_or(DecoderChunkOutcome::Eof, DecoderChunkOutcome::Chunk))
        },
        seek_results,
        duration,
        options.track_info,
        options.verify_in_drop,
    )
}

/// Create an infinite decoder mock backed by unimock.
///
/// Produces fixed-size chunks until `stop` becomes true.
#[must_use]
pub fn infinite_decoder(spec: AudioSpec, stop: Arc<AtomicBool>) -> (Box<dyn Decoder>, DecoderLogs) {
    build_infinite_decoder(spec, stop, true)
}

/// Create an infinite decoder mock with verification disabled in `Drop`.
///
/// Use this only for data-plane tests where not all mocked methods are expected
/// to be called in every scenario.
#[must_use]
pub fn infinite_decoder_loose(
    spec: AudioSpec,
    stop: Arc<AtomicBool>,
) -> (Box<dyn Decoder>, DecoderLogs) {
    build_infinite_decoder(spec, stop, false)
}

fn build_infinite_decoder(
    spec: AudioSpec,
    stop: Arc<AtomicBool>,
    verify_in_drop: bool,
) -> (Box<dyn Decoder>, DecoderLogs) {
    /// Default PCM sample value for mock audio.
    const SAMPLE_VALUE: f32 = 0.5;

    /// Default chunk size for infinite mock decoder.
    const MOCK_CHUNK_SIZE: usize = 1024;

    /// Default mock track duration in seconds.
    const MOCK_DURATION_SECS: u64 = 220;

    let pools = pools();
    build_decoder_mock(
        spec,
        move |_| {
            if stop.load(Ordering::Acquire) {
                return Ok(DecoderChunkOutcome::Eof);
            }
            let mut samples = pools
                .get_with_len::<f32>(MOCK_CHUNK_SIZE)
                .expect("mock chunk fits the test pool budget");
            samples.fill(SAMPLE_VALUE);
            Ok(DecoderChunkOutcome::Chunk(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    ..Default::default()
                },
                samples,
            )))
        },
        Vec::new(),
        Some(Duration::from_secs(MOCK_DURATION_SECS)),
        DecoderTrackInfo::default(),
        verify_in_drop,
    )
}

fn build_decoder_mock<F>(
    spec: AudioSpec,
    next_chunk: F,
    seek_results: Vec<DecodeResult<DecoderSeekOutcome>>,
    duration: Option<Duration>,
    track_info: DecoderTrackInfo,
    verify_in_drop: bool,
) -> (Box<dyn Decoder>, DecoderLogs)
where
    F: Fn(&mut Unimock) -> DecodeResult<DecoderChunkOutcome> + Send + Sync + 'static,
{
    let seek_queue = Arc::new(MockLog::new(VecDeque::from(seek_results)));
    let seek_log = Arc::new(MockLog::new(Vec::new()));
    let byte_len_log = Arc::new(MockLog::new(Vec::new()));

    let seek_log_for_seek = Arc::clone(&seek_log);
    let byte_len_log_for_update = Arc::clone(&byte_len_log);

    let mock = Unimock::new((
        DecoderMock::next_chunk
            .each_call(matching!())
            .answers_arc(Arc::new(next_chunk))
            .at_least_times(0),
        DecoderMock::spec
            .each_call(matching!())
            .returns(spec)
            .at_least_times(0),
        DecoderMock::seek
            .each_call(matching!(_))
            .answers_arc(Arc::new(move |_, pos| {
                seek_log_for_seek.lock().push(pos);
                if let Some(result) = seek_queue.lock().pop_front() {
                    return result;
                }
                Ok(DecoderSeekOutcome::Landed {
                    landed_at: pos,
                    landed_frame: 0,
                    landed_byte: None,
                    preroll: kithara::stream::PrerollHint::NotNeeded,
                })
            }))
            .at_least_times(0),
        DecoderMock::update_byte_len
            .each_call(matching!(_))
            .answers_arc(Arc::new(move |_, len| {
                byte_len_log_for_update.lock().push(len);
            }))
            .at_least_times(0),
        DecoderMock::duration
            .each_call(matching!())
            .returns(duration)
            .at_least_times(0),
        DecoderMock::track_info
            .each_call(matching!())
            .returns(track_info)
            .at_least_times(0),
    ));
    let mock = if verify_in_drop {
        mock
    } else {
        mock.no_verify_in_drop()
    };

    (
        Box::new(mock),
        DecoderLogs {
            byte_len_log,
            seek_log,
        },
    )
}
