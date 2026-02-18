use std::{
    collections::VecDeque,
    io::SeekFrom,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use crate::pipeline::worker::AudioCommand;
use kithara_bufpool::pcm_pool;
use kithara_decode::mock::{infinite_inner_decoder_loose, scripted_inner_decoder_loose};
use kithara_decode::{DecodeError, DecodeResult, InnerDecoder, PcmChunk, PcmMeta, PcmSpec};
use kithara_platform::Mutex;
use kithara_storage::WaitOutcome;
use kithara_stream::{MediaInfo, Source, Stream, StreamResult, StreamType};

use super::*;

// TestSource + TestStream

struct TestSourceState {
    data: Vec<u8>,
    len: Option<u64>,
    media_info: Option<MediaInfo>,
    segment_range: Option<Range<u64>>,
    /// Range of the first segment with current format (for ABR switch).
    /// Used by `format_change_segment_range()` to return where init data lives.
    format_change_range: Option<Range<u64>>,
    /// Variant fence: auto-detected on first read, blocks cross-variant reads.
    variant_fence: Option<usize>,
    /// Mapping of byte ranges to variant indices (for fence logic).
    variant_map: Vec<(Range<u64>, usize)>,
}

struct TestSource {
    state: Arc<Mutex<TestSourceState>>,
}

impl TestSource {
    fn new(data: Vec<u8>, len: Option<u64>) -> Self {
        Self {
            state: Arc::new(Mutex::new(TestSourceState {
                data,
                len,
                media_info: None,
                segment_range: None,
                format_change_range: None,
                variant_fence: None,
                variant_map: Vec::new(),
            })),
        }
    }

    fn state_handle(&self) -> Arc<Mutex<TestSourceState>> {
        Arc::clone(&self.state)
    }
}

impl Source for TestSource {
    type Error = std::io::Error;

    fn wait_range(&mut self, _range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        Ok(WaitOutcome::Ready)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        let mut state = self.state.lock();
        let offset_usize = offset as usize;
        if offset_usize >= state.data.len() {
            return Ok(0);
        }

        // Variant fence logic (mirrors HlsSource behavior).
        if !state.variant_map.is_empty() {
            let variant = state
                .variant_map
                .iter()
                .find(|(range, _)| range.contains(&offset))
                .map(|(_, v)| *v);

            if let Some(v) = variant {
                if state.variant_fence.is_none() {
                    state.variant_fence = Some(v);
                }
                if let Some(fence) = state.variant_fence
                    && v != fence
                {
                    return Ok(0);
                }
            }
        }

        // Clip reads at variant boundary (mirrors HlsSource::read_from_entry()).
        let data_end = if !state.variant_map.is_empty() {
            state
                .variant_map
                .iter()
                .find(|(range, _)| range.contains(&offset))
                .map_or(state.data.len(), |(range, _)| range.end as usize)
        } else {
            state.data.len()
        };
        let available = &state.data[offset_usize..data_end];
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);

        Ok(n)
    }

    fn clear_variant_fence(&mut self) {
        self.state.lock().variant_fence = None;
    }

    fn len(&self) -> Option<u64> {
        self.state.lock().len
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.state.lock().media_info.clone()
    }

    fn current_segment_range(&self) -> Option<Range<u64>> {
        self.state.lock().segment_range.clone()
    }

    fn format_change_segment_range(&self) -> Option<Range<u64>> {
        self.state.lock().format_change_range.clone()
    }
}

#[derive(Default)]
struct TestConfig {
    source: Option<TestSource>,
}

struct TestStream;

impl StreamType for TestStream {
    type Config = TestConfig;
    type Source = TestSource;
    type Error = std::io::Error;
    type Events = ();

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        config
            .source
            .ok_or_else(|| std::io::Error::other("no source"))
    }

    fn build_stream_context(
        _source: &Self::Source,
        position: Arc<AtomicU64>,
    ) -> Arc<dyn kithara_stream::StreamContext> {
        Arc::new(kithara_stream::NullStreamContext::new(position))
    }
}

// Helpers

fn make_chunk(spec: PcmSpec, num_samples: usize) -> PcmChunk {
    PcmChunk::new(
        PcmMeta {
            spec,
            ..Default::default()
        },
        pcm_pool().attach(vec![0.5; num_samples]),
    )
}

fn test_stream_from_source(source: TestSource) -> Stream<TestStream> {
    let config = TestConfig {
        source: Some(source),
    };
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(Stream::new(config))
        .unwrap()
}

fn make_shared_stream(
    data: Vec<u8>,
    len: Option<u64>,
) -> (SharedStream<TestStream>, Arc<Mutex<TestSourceState>>) {
    let source = TestSource::new(data, len);
    let state = source.state_handle();
    let stream = test_stream_from_source(source);
    (SharedStream::new(stream), state)
}

fn make_factory(decoders: Vec<Box<dyn InnerDecoder>>) -> DecoderFactory<TestStream> {
    let queue = Arc::new(Mutex::new(VecDeque::from(decoders)));
    Box::new(move |_stream, _info, _offset| queue.lock().pop_front())
}

/// Factory that records every `base_offset` it receives.
fn make_tracking_factory(
    decoders: Vec<Box<dyn InnerDecoder>>,
) -> (DecoderFactory<TestStream>, Arc<Mutex<Vec<u64>>>) {
    let queue = Arc::new(Mutex::new(VecDeque::from(decoders)));
    let offsets: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let offsets_clone = Arc::clone(&offsets);
    let factory: DecoderFactory<TestStream> = Box::new(move |_stream, _info, offset| {
        offsets_clone.lock().push(offset);
        queue.lock().pop_front()
    });
    (factory, offsets)
}

fn make_source(
    shared: SharedStream<TestStream>,
    decoder: Box<dyn InnerDecoder>,
    factory: DecoderFactory<TestStream>,
    media_info: Option<MediaInfo>,
) -> StreamAudioSource<TestStream> {
    let epoch = Arc::new(AtomicU64::new(0));
    StreamAudioSource::new(shared, decoder, factory, media_info, epoch, vec![])
}

fn v0_spec() -> PcmSpec {
    PcmSpec {
        channels: 2,
        sample_rate: 44100,
    }
}

fn v3_spec() -> PcmSpec {
    PcmSpec {
        channels: 2,
        sample_rate: 96000,
    }
}

fn v0_info() -> MediaInfo {
    MediaInfo::default()
        .with_sample_rate(44100)
        .with_channels(2)
}

fn v3_info() -> MediaInfo {
    MediaInfo::default()
        .with_sample_rate(96000)
        .with_channels(2)
}

// Tests

/// Test that ABR switch uses `format_change_segment_range()` to find init data.
///
/// Production scenario (HLS ABR switch V0 AAC → V3 FLAC):
///
/// Byte layout:
///   0..964431:        V0 segments 0-18 (AAC)
///   964431..1732515:  V3 segment 19 (FLAC, has ftyp + moov init data)
///   1732515..2476302: V3 segment 20 (FLAC, media only, NO ftyp)
///
/// The reader may pass segment 19 before `detect_format_change` runs,
/// so `current_segment_range()` would return segment 20 (no init data).
/// Using `format_change_segment_range()` returns segment 19 where
/// ftyp/moov lives, allowing decoder to be recreated correctly.
#[test]
fn apply_format_change_must_use_first_new_format_segment_offset() {
    // Use production-like offsets from the log
    const V3_SEGMENT_19_START: u64 = 964431;
    const V3_SEGMENT_20_START: u64 = 1732515;
    const V3_SEGMENT_20_END: u64 = 2476302;

    let (shared, state) = make_shared_stream(
        vec![0u8; V3_SEGMENT_20_END as usize],
        Some(V3_SEGMENT_20_END),
    );

    // V0 decoder: 4 chunks then EOF
    let v0_chunks = vec![make_chunk(v0_spec(), 1024); 4];
    let (v0_decoder, _) = scripted_inner_decoder_loose(v0_spec(), v0_chunks, vec![], None);

    // V3 decoder the factory will create
    let v3_chunks = vec![make_chunk(v3_spec(), 2048); 5];
    let (v3_decoder, _) = scripted_inner_decoder_loose(v3_spec(), v3_chunks, vec![], None);
    let (factory, factory_offsets) = make_tracking_factory(vec![v3_decoder]);

    let mut source = make_source(shared, v0_decoder, factory, Some(v0_info()));

    // Decode 1 V0 chunk
    let fetch = source.fetch_next();
    assert!(!fetch.is_eof);

    // Simulate: reader passed first V3 segment (964431..1732515)
    // and is now in segment 20 (1732515..2476302).
    // This is what happens in production: the reader reads through
    // V3 segment 19 data before detect_format_change has a chance to run.
    {
        let mut s = state.lock();
        s.media_info = Some(v3_info());
        // current_segment_range returns segment 20 — reader already past segment 19
        s.segment_range = Some(V3_SEGMENT_20_START..V3_SEGMENT_20_END);
        // format_change_segment_range returns the FIRST V3 segment where init data lives
        s.format_change_range = Some(V3_SEGMENT_19_START..V3_SEGMENT_20_START);
    }

    // Decode remaining V0 chunks + trigger EOF → apply_format_change
    loop {
        let fetch = source.fetch_next();
        if fetch.is_eof {
            break;
        }
    }

    // Verify factory was called at the correct offset
    let offsets = factory_offsets.lock();
    assert_eq!(offsets.len(), 1, "Factory should have been called once");

    // FIX: code now uses format_change_segment_range() which returns the FIRST segment
    // of the new format (964431), not current_segment_range() (1732515).
    assert_eq!(
        offsets[0], V3_SEGMENT_19_START,
        "Decoder must be recreated at first V3 segment ({V3_SEGMENT_19_START}) \
             where ftyp/moov init data lives, not at current segment ({V3_SEGMENT_20_START}) \
             which has no init data and causes 'missing ftyp atom' error"
    );
}

#[test]
fn basic_decode_to_eof() {
    let (shared, _state) = make_shared_stream(vec![0u8; 1000], Some(1000));
    let chunks = vec![make_chunk(v0_spec(), 1024); 3];
    let (decoder, _) = scripted_inner_decoder_loose(v0_spec(), chunks, vec![], None);
    let factory = make_factory(vec![]);
    let mut source = make_source(shared, decoder, factory, Some(v0_info()));

    for _ in 0..3 {
        let fetch = source.fetch_next();
        assert!(!fetch.is_eof);
        assert!(!fetch.data.pcm.is_empty());
    }

    let fetch = source.fetch_next();
    assert!(fetch.is_eof);
}

#[test]
fn format_change_recreates_decoder() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));
    let v0_chunks = vec![make_chunk(v0_spec(), 1024); 2];
    let (v0_decoder, _) = scripted_inner_decoder_loose(v0_spec(), v0_chunks, vec![], None);

    let v3_chunks = vec![make_chunk(v3_spec(), 2048); 3];
    let (v3_decoder, _) = scripted_inner_decoder_loose(v3_spec(), v3_chunks, vec![], None);
    let factory = make_factory(vec![v3_decoder]);

    let mut source = make_source(shared, v0_decoder, factory, Some(v0_info()));

    // Decode 1 V0 chunk
    let fetch = source.fetch_next();
    assert!(!fetch.is_eof);

    // Trigger format change
    {
        let mut s = state.lock();
        s.media_info = Some(v3_info());
        s.segment_range = Some(1000..2000);
    }

    // Decode remaining V0 chunk — detect_format_change sets boundary
    let fetch = source.fetch_next();
    assert!(!fetch.is_eof);
    assert!(source.has_pending_format_change());

    // V0 decoder exhausted → EOF → apply_format_change → V3 decoder
    let fetch = source.fetch_next();
    assert!(!fetch.is_eof, "Should get V3 data after format change");
    assert_eq!(fetch.data.spec(), v3_spec());
}

#[test]
fn seek_updates_epoch_and_calls_decoder() {
    let (shared, _state) = make_shared_stream(vec![0u8; 1000], Some(1000));
    let chunks = vec![make_chunk(v0_spec(), 1024); 5];
    let (decoder, logs) = scripted_inner_decoder_loose(v0_spec(), chunks, vec![], None);
    let seek_log = logs.seek_log();
    let factory = make_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = StreamAudioSource::new(
        shared,
        decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    source.handle_command(AudioCommand::Seek {
        position: Duration::from_secs(10),
        epoch: 42,
    });

    assert_eq!(epoch.load(Ordering::Acquire), 42);
    let seeks = seek_log.lock();
    assert_eq!(seeks.len(), 1);
    assert_eq!(seeks[0], Duration::from_secs(10));
}

#[test]
fn seek_skips_byte_len_update_after_abr_switch() {
    let (shared, _state) = make_shared_stream(vec![0u8; 1000], Some(1000));
    let chunks = vec![make_chunk(v3_spec(), 2048); 5];
    let (decoder, logs) = scripted_inner_decoder_loose(v3_spec(), chunks, vec![], None);
    let byte_len_log = logs.byte_len_log();
    let factory = make_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = StreamAudioSource::new(
        shared,
        decoder,
        factory,
        Some(v3_info()),
        Arc::clone(&epoch),
        vec![],
    );

    // Simulate that ABR switch already happened
    source.base_offset = 863137;

    source.handle_command(AudioCommand::Seek {
        position: Duration::from_secs(110),
        epoch: 1,
    });

    assert!(
        byte_len_log.lock().is_empty(),
        "update_byte_len must not be called when base_offset > 0"
    );
}

#[test]
fn failed_seek_after_abr_switch_recovers() {
    let (shared, _state) = make_shared_stream(vec![0u8; 2000], Some(2000));

    let v3_chunks = vec![make_chunk(v3_spec(), 2048); 5];
    let (v3_decoder, _) = scripted_inner_decoder_loose(
        v3_spec(),
        v3_chunks,
        vec![Err(DecodeError::SeekError("unexpected end of file".into()))],
        None,
    );

    let recovery_chunks = vec![make_chunk(v3_spec(), 2048); 5];
    let (recovery_decoder, _) =
        scripted_inner_decoder_loose(v3_spec(), recovery_chunks, vec![], None);
    let factory = make_factory(vec![recovery_decoder]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = StreamAudioSource::new(
        shared,
        v3_decoder,
        factory,
        Some(v3_info()),
        Arc::clone(&epoch),
        vec![],
    );

    source.base_offset = 863137;
    source.cached_media_info = Some(v3_info());

    source.handle_command(AudioCommand::Seek {
        position: Duration::from_secs(10),
        epoch: 1,
    });

    let fetch = source.fetch_next();
    assert!(!fetch.is_eof, "Should produce data after seek recovery");
}

/// Seek to 0 after ABR switch resets decoder to offset 0.
///
/// After ABR switch (base_offset > 0), seeking to time=0 should:
/// 1. Clear variant fence
/// 2. Recreate decoder at offset 0 (not base_offset)
/// 3. Reset cached_media_info so format change detection picks up V0→V1
/// 4. Reset base_offset to 0
///
/// Without this, OffsetReader translates seek(0) to base_offset, making
/// V0 segments unreachable — the flaky test root cause.
#[test]
fn seek_to_zero_after_abr_switch_resets_to_full_track() {
    let (shared, _state) = make_shared_stream(vec![0u8; 2000], Some(2000));

    // Current decoder: V3 at base_offset 863137
    let v3_chunks = vec![make_chunk(v3_spec(), 2048); 3];
    let (v3_decoder, _) = scripted_inner_decoder_loose(v3_spec(), v3_chunks, vec![], None);

    // Factory: V0 decoder at offset 0
    let v0_chunks = vec![make_chunk(v0_spec(), 1024); 5];
    let (v0_decoder, _) = scripted_inner_decoder_loose(v0_spec(), v0_chunks, vec![], None);
    let (factory, factory_offsets) = make_tracking_factory(vec![v0_decoder]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = StreamAudioSource::new(
        shared,
        v3_decoder,
        factory,
        Some(v3_info()),
        Arc::clone(&epoch),
        vec![],
    );

    // Simulate that ABR switch already happened
    source.base_offset = 863137;
    source.cached_media_info = Some(v3_info());

    // Seek to 0 — should reset decoder to offset 0
    source.handle_command(AudioCommand::Seek {
        position: Duration::ZERO,
        epoch: 1,
    });

    assert_eq!(
        source.current_base_offset(),
        0,
        "base_offset should be reset to 0 after seek-to-zero"
    );
    assert_eq!(epoch.load(Ordering::Acquire), 1);

    let offsets = factory_offsets.lock();
    assert_eq!(offsets.len(), 1, "Factory should be called once");
    assert_eq!(offsets[0], 0, "Factory should be called with offset 0");

    // Should be able to produce data from V0 decoder
    let fetch = source.fetch_next();
    assert!(!fetch.is_eof, "Should produce V0 data after reset");
}

/// **BASELINE TEST** — reproduces the exact production bug.
///
/// Scenario from production logs (HLS ABR switch V0 AAC → V3 FLAC):
/// 1. V0 decoder active, `base_offset`=0
/// 2. Format change detected (V0→V3), boundary set, `pending_format_change`=Some
/// 3. Seek command arrives BEFORE `apply_format_change` runs
/// 4. Old V0 decoder seek fails (at boundary EOF)
/// 5. `base_offset`==0 → existing recovery is skipped
/// 6. Seek position is lost — V3 decoder never receives it
///
/// Expected: pending format change is applied, seek retried on V3 decoder.
#[test]
fn seek_during_pending_format_change_retries_on_new_decoder() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));

    // V0 decoder: 3 chunks, then seek will fail
    let v0_chunks = vec![make_chunk(v0_spec(), 1024); 3];
    let (v0_decoder, _) = scripted_inner_decoder_loose(
        v0_spec(),
        v0_chunks,
        vec![Err(DecodeError::SeekError("unexpected end of file".into()))],
        None,
    );

    // V3 decoder that factory will create — should receive retried seek
    let v3_chunks = vec![make_chunk(v3_spec(), 2048); 10];
    let (v3_decoder, logs) = scripted_inner_decoder_loose(v3_spec(), v3_chunks, vec![], None);
    let v3_seek_log = logs.seek_log();
    let factory = make_factory(vec![v3_decoder]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = StreamAudioSource::new(
        shared,
        v0_decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    // Decode 1 V0 chunk
    let fetch = source.fetch_next();
    assert!(!fetch.is_eof);

    // Trigger format change (ABR switch V0→V3)
    {
        let mut s = state.lock();
        s.media_info = Some(v3_info());
        s.segment_range = Some(1000..2000);
    }

    // Decode another V0 chunk — detect_format_change sets boundary + pending
    let fetch = source.fetch_next();
    assert!(!fetch.is_eof);
    assert!(
        source.has_pending_format_change(),
        "Format change should be pending after detection"
    );

    // Seek arrives BEFORE format change is applied.
    // Old V0 decoder seek fails. base_offset=0.
    let seek_pos = Duration::from_secs_f64(147.48);
    source.handle_command(AudioCommand::Seek {
        position: seek_pos,
        epoch: 1,
    });

    // EXPECTED: format change was applied, V3 decoder received the seek
    assert_eq!(
        source.current_base_offset(),
        1000,
        "Pending format change should have been applied during seek recovery"
    );

    let v3_seeks = v3_seek_log.lock();
    assert_eq!(
        v3_seeks.len(),
        1,
        "V3 decoder should have received the retried seek"
    );
    assert_eq!(
        v3_seeks[0], seek_pos,
        "V3 decoder should receive seek to original position"
    );
}

/// **STRESS TEST** — rapid seeking for 20 seconds during ABR switch.
///
/// Reproduces production scenario where user scrubs the timeline rapidly
/// while ABR switches from V0 (AAC) to V3 (FLAC). After the old decoder
/// is exhausted and format change is applied at the wrong offset (1732515
/// instead of 964431), the factory fails to create a decoder ("missing ftyp").
/// Audio dies permanently — every subsequent `fetch_next` returns EOF.
///
/// 30-second timeout catches deadlocks.
#[test]
fn stress_rapid_seeks_during_abr_switch_must_not_kill_audio() {
    const V3_SEGMENT_19_START: u64 = 964431;
    const V3_SEGMENT_20_START: u64 = 1732515;
    const V3_SEGMENT_20_END: u64 = 2476302;

    let handle = std::thread::spawn(move || {
        let (shared, state) = make_shared_stream(
            vec![0u8; V3_SEGMENT_20_END as usize],
            Some(V3_SEGMENT_20_END),
        );

        // V0 decoder: produces chunks until stopped
        let v0_stop = Arc::new(AtomicBool::new(false));
        let (v0_decoder, _) = infinite_inner_decoder_loose(v0_spec(), Arc::clone(&v0_stop));

        // Factory: only succeeds at correct offset, returns None at wrong offset.
        // Mimics production: ftyp atom only at 964431, not at 1732515.
        let factory_offsets: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let factory_offsets_clone = Arc::clone(&factory_offsets);
        let factory: DecoderFactory<TestStream> = Box::new(move |_stream, _info, offset| {
            factory_offsets_clone.lock().push(offset);
            if offset == V3_SEGMENT_19_START {
                // Correct offset — decoder would succeed
                Some(infinite_inner_decoder_loose(v3_spec(), Arc::new(AtomicBool::new(false))).0)
            } else {
                // Wrong offset (1732515) — "missing ftyp atom" in production
                None
            }
        });

        let mut source = make_source(shared, v0_decoder, factory, Some(v0_info()));

        let start = Instant::now();
        let mut epoch = 0u64;
        let mut format_changed = false;
        let mut v0_stopped = false;
        let mut chunks_after_v0_stop = 0u64;
        let mut eof_after_v0_stop = 0u64;

        // Cycling through various seek positions (like rapid slider scrubbing)
        let seek_positions: &[f64] = &[
            23.5, 147.48, 88.7, 5.0, 200.0, 120.0, 45.0, 180.0, 10.0, 160.0, 55.0, 95.0, 30.0,
            175.0, 65.0, 210.0, 15.0, 110.0, 70.0, 195.0,
        ];

        while start.elapsed() < Duration::from_secs(20) {
            let pos_idx = (epoch as usize) % seek_positions.len();
            let seek_pos = Duration::from_secs_f64(seek_positions[pos_idx]);
            epoch += 1;

            source.handle_command(AudioCommand::Seek {
                position: seek_pos,
                epoch,
            });

            // Fetch a few chunks but don't drain (simulating rapid scrubbing)
            for _ in 0..3 {
                let fetch = source.fetch_next();
                if v0_stopped {
                    if fetch.is_eof {
                        eof_after_v0_stop += 1;
                    } else {
                        chunks_after_v0_stop += 1;
                    }
                }
                if fetch.is_eof {
                    break;
                }
            }

            // After 2s: ABR switch — media_info changes, reader past segment 19
            if !format_changed && start.elapsed() > Duration::from_secs(2) {
                let mut s = state.lock();
                s.media_info = Some(v3_info());
                s.segment_range = Some(V3_SEGMENT_20_START..V3_SEGMENT_20_END);
                // format_change_segment_range returns the FIRST V3 segment where init data lives
                s.format_change_range = Some(V3_SEGMENT_19_START..V3_SEGMENT_20_START);
                drop(s);
                format_changed = true;
            }

            // After 4s: old decoder hits boundary → EOF (simulates read boundary)
            if format_changed && !v0_stopped && start.elapsed() > Duration::from_secs(4) {
                v0_stop.store(true, Ordering::Release);
                v0_stopped = true;
            }
        }

        // After 20 seconds of rapid seeking with format_change_segment_range():
        //
        // Code uses correct offset (964431), factory succeeds,
        // V3 decoder installed, chunks_after_v0_stop > 0.
        assert!(
            chunks_after_v0_stop > 0,
            "Audio dead after ABR switch: {eof_after_v0_stop} EOFs, \
                 0 chunks produced after V0 decoder stopped. \
                 {epoch} seeks performed over 20s. \
                 Expected format_change_segment_range() to return 964431."
        );
    });

    // Timeout: 30s to catch deadlocks
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if handle.is_finished() {
            if let Err(e) = handle.join() {
                std::panic::resume_unwind(e);
            }
            return;
        }
        if Instant::now() > deadline {
            panic!("Test timed out after 30s — deadlock in seek/format-change interaction");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Variant fence blocks cross-variant reads and clears on reset.
///
/// Layout: [V0: 0..1000] [V3: 1000..2000]
/// - First read in V0 → fence auto-detects V0
/// - Seek to V3 offset → read returns 0 (fence blocks)
/// - `clear_variant_fence()` → read V3 → fence auto-detects V3
#[test]
fn source_variant_fence_blocks_cross_variant_reads() {
    let mut data = vec![0xAA; 2000];
    data[1000..].fill(0xBB);

    let source = TestSource::new(data, Some(2000));
    let state = source.state_handle();

    // Set up variant map: [V0: 0..1000] [V3: 1000..2000]
    {
        let mut s = state.lock();
        s.variant_map = vec![(0..1000, 0), (1000..2000, 3)];
    }

    let stream = test_stream_from_source(source);
    let mut shared = SharedStream::new(stream);

    // Read from V0 region — fence auto-detects V0
    let mut buf = vec![0u8; 100];
    let n = shared.read(&mut buf).unwrap();
    assert_eq!(n, 100, "V0 read should succeed");
    assert!(buf[..n].iter().all(|&b| b == 0xAA), "Should be V0 data");

    // Seek to V3 region — fence blocks
    shared.seek(SeekFrom::Start(1000)).unwrap();
    let mut buf = vec![0u8; 100];
    let n = shared.read(&mut buf).unwrap();
    assert_eq!(n, 0, "V3 read must be blocked by fence (V0)");

    // Clear fence → read V3
    shared.clear_variant_fence();
    shared.seek(SeekFrom::Start(1000)).unwrap();
    let mut buf = vec![0u8; 100];
    let n = shared.read(&mut buf).unwrap();
    assert_eq!(n, 100, "V3 read should succeed after fence clear");
    assert!(buf[..n].iter().all(|&b| b == 0xBB), "Should be V3 data");
}

// Encoded ABR switch test — verify no samples lost during decoder recreation

const SAMPLES_PER_SEGMENT: usize = 1200;
const SEGMENTS_PER_VARIANT: usize = 32;
const V0_SAMPLE_SIZE: usize = 4;
const V1_SAMPLE_SIZE: usize = 16;

// -- Byte-level encoding (what Source delivers) --

fn encode_v0_sample(variant: u8, segment: u8, gsi: u16) -> [u8; 4] {
    let val: u32 = (variant as u32) << 24 | (segment as u32) << 16 | (gsi as u32);
    val.to_be_bytes()
}

fn decode_v0_sample(bytes: [u8; 4]) -> (u32, u32, u64) {
    let val = u32::from_be_bytes(bytes);
    let variant = (val >> 24) & 0xFF;
    let segment = (val >> 16) & 0xFF;
    let gsi = (val & 0xFFFF) as u64;
    (variant, segment, gsi)
}

fn encode_v1_sample(variant: u32, segment: u32, gsi: u64) -> [u8; 16] {
    let val: u128 = (variant as u128) << 96 | (segment as u128) << 64 | (gsi as u128);
    val.to_be_bytes()
}

fn decode_v1_sample(bytes: &[u8; 16]) -> (u32, u32, u64) {
    let val = u128::from_be_bytes(*bytes);
    let variant = ((val >> 96) & 0xFFFF_FFFF) as u32;
    let segment = ((val >> 64) & 0xFFFF_FFFF) as u32;
    let gsi = (val & 0xFFFF_FFFF_FFFF_FFFF) as u64;
    (variant, segment, gsi)
}

/// Generate encoded byte stream from segment descriptors.
///
/// Each entry: `(variant, segment, start_gsi, sample_size)`.
fn generate_encoded_stream(segments: &[(u32, u32, u64, usize)]) -> Vec<u8> {
    let mut data = Vec::new();
    for &(variant, segment, start_gsi, sample_size) in segments {
        for i in 0..SAMPLES_PER_SEGMENT {
            let gsi = start_gsi + i as u64;
            match sample_size {
                V0_SAMPLE_SIZE => {
                    data.extend_from_slice(&encode_v0_sample(
                        variant as u8,
                        segment as u8,
                        gsi as u16,
                    ));
                }
                V1_SAMPLE_SIZE => {
                    data.extend_from_slice(&encode_v1_sample(variant, segment, gsi));
                }
                _ => panic!("unsupported sample_size {sample_size}"),
            }
        }
    }
    data
}

// -- PCM f32 bit-packing (what decoder outputs) --

fn encode_pcm_sample(variant_segment: u8, sample_index: u32) -> f32 {
    let exponent = (variant_segment as u32 + 1) & 0xFF;
    let bits: u32 = (exponent << 23) | (sample_index & 0x7F_FFFF);
    f32::from_bits(bits)
}

fn decode_pcm_sample(val: f32) -> (u8, u32) {
    let bits = val.to_bits();
    let variant_segment = (((bits >> 23) & 0xFF) - 1) as u8;
    let sample_index = bits & 0x7F_FFFF;
    (variant_segment, sample_index)
}

// -- EncodedDecoder --

/// Decoder that reads real bytes from `OffsetReader`, validates byte-level
/// consistency, and encodes output as PCM f32 with bit-packed metadata.
struct EncodedDecoder {
    reader: OffsetReader<TestStream>,
    spec: PcmSpec,
    sample_size: usize,
    samples_per_chunk: usize,
    expected_gsi: Option<u64>,
    expected_variant: Option<u32>,
}

impl EncodedDecoder {
    fn new(
        reader: OffsetReader<TestStream>,
        spec: PcmSpec,
        sample_size: usize,
        samples_per_chunk: usize,
    ) -> Self {
        Self {
            reader,
            spec,
            sample_size,
            samples_per_chunk,
            expected_gsi: None,
            expected_variant: None,
        }
    }

    fn read_exact_or_eof(&mut self, buf: &mut [u8]) -> std::io::Result<bool> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.reader.read(&mut buf[filled..]) {
                Ok(0) => return Ok(false),
                Ok(n) => filled += n,
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }
}

impl InnerDecoder for EncodedDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        let mut pcm = Vec::new();
        let mut sample_buf = vec![0u8; self.sample_size];

        for _ in 0..self.samples_per_chunk {
            match self.read_exact_or_eof(&mut sample_buf) {
                Ok(true) => {}
                Ok(false) => {
                    // EOF: return accumulated samples or None
                    return if pcm.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(PcmChunk::new(
                            PcmMeta {
                                spec: self.spec,
                                ..Default::default()
                            },
                            pcm_pool().attach(pcm),
                        )))
                    };
                }
                Err(e) => return Err(DecodeError::Io(e)),
            }

            let (variant, segment, gsi) = match self.sample_size {
                V0_SAMPLE_SIZE => {
                    let bytes: [u8; 4] = sample_buf[..4].try_into().unwrap();
                    decode_v0_sample(bytes)
                }
                V1_SAMPLE_SIZE => {
                    let bytes: [u8; 16] = sample_buf[..16].try_into().unwrap();
                    decode_v1_sample(&bytes)
                }
                _ => panic!("unsupported sample_size {}", self.sample_size),
            };

            // Validate: sequential GSI
            if let Some(expected) = self.expected_gsi {
                assert_eq!(
                    gsi, expected,
                    "GSI gap: expected {expected}, got {gsi} \
                         (variant={variant}, segment={segment})"
                );
            }
            self.expected_gsi = Some(gsi + 1);

            // Validate: single variant per decoder lifetime
            if let Some(expected_v) = self.expected_variant {
                assert_eq!(
                    variant, expected_v,
                    "Cross-variant read: expected variant {expected_v}, got {variant} \
                         (segment={segment}, gsi={gsi})"
                );
            }
            self.expected_variant = Some(variant);

            // Encode as f32: variant_segment encodes both variant and segment
            let local_segment = segment - variant * SEGMENTS_PER_VARIANT as u32;
            let variant_segment = (variant * SEGMENTS_PER_VARIANT as u32 + local_segment) as u8;
            pcm.push(encode_pcm_sample(variant_segment, gsi as u32));
        }

        Ok(Some(PcmChunk::new(
            PcmMeta {
                spec: self.spec,
                ..Default::default()
            },
            pcm_pool().attach(pcm),
        )))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
        Ok(())
    }

    fn update_byte_len(&self, _len: u64) {}

    fn duration(&self) -> Option<Duration> {
        None
    }
}

fn make_encoded_factory(spec: PcmSpec, sample_size: usize) -> DecoderFactory<TestStream> {
    Box::new(move |stream, _info, base_offset| {
        let reader = OffsetReader::new(stream, base_offset);
        Some(Box::new(EncodedDecoder::new(reader, spec, sample_size, 50)))
    })
}

fn v1_spec() -> PcmSpec {
    PcmSpec {
        channels: 1,
        sample_rate: 48000,
    }
}

fn v1_info() -> MediaInfo {
    MediaInfo::default()
        .with_sample_rate(48000)
        .with_channels(1)
}

/// **End-to-end ABR switch test** — verify no samples lost during decoder recreation.
///
/// Uses encoded byte streams (V0: 4 bytes/sample, V1: 16 bytes/sample) and a mock
/// decoder that reads real bytes via `OffsetReader`. Each f32 PCM sample encodes both
/// the variant/segment origin and the global sample index via IEEE 754 bit-packing.
///
/// Stream layout (64 segments × 1200 samples, 32 per variant):
///   V0 segments 0..31:  each 1200 × 4 = 4800 bytes  → 153600 bytes total
///   V1 segments 32..63: each 1200 × 16 = 19200 bytes → 614400 bytes total
///   Grand total: 768000 bytes, 76800 samples
///
/// Catches: wrong `base_offset`, forgotten seek, cross-variant data, sample gaps.
#[test]
fn abr_switch_must_not_lose_samples() {
    let spv = SEGMENTS_PER_VARIANT;
    let sps = SAMPLES_PER_SEGMENT;

    // Build segment descriptors: V0 (segments 0..spv), V1 (segments spv..2*spv)
    let mut segments = Vec::new();
    for seg in 0..spv {
        let gsi = (seg * sps) as u64;
        segments.push((0, seg as u32, gsi, V0_SAMPLE_SIZE));
    }
    for seg in 0..spv {
        let global_seg = spv + seg;
        let gsi = (global_seg * sps) as u64;
        segments.push((1, global_seg as u32, gsi, V1_SAMPLE_SIZE));
    }

    let data = generate_encoded_stream(&segments);
    let v0_bytes = spv * sps * V0_SAMPLE_SIZE; // 153600
    let v1_bytes = spv * sps * V1_SAMPLE_SIZE; // 614400
    let total_bytes = v0_bytes + v1_bytes; // 768000
    assert_eq!(data.len(), total_bytes);

    // First V1 segment byte range (for format_change_range)
    let v1_first_seg_end = v0_bytes + sps * V1_SAMPLE_SIZE;

    // Setup TestSource with variant map
    let source = TestSource::new(data, Some(total_bytes as u64));
    let state = source.state_handle();
    {
        let mut s = state.lock();
        s.media_info = Some(v1_info());
        s.format_change_range = Some(v0_bytes as u64..v1_first_seg_end as u64);
        s.variant_map = vec![
            (0..v0_bytes as u64, 0),
            (v0_bytes as u64..total_bytes as u64, 1),
        ];
    }
    let stream = test_stream_from_source(source);
    let shared = SharedStream::new(stream);

    // Initial decoder: V0 (4 bytes/sample)
    let v0_mono_spec = PcmSpec {
        channels: 1,
        sample_rate: 44100,
    };
    let v0_mono_info = MediaInfo::default()
        .with_sample_rate(44100)
        .with_channels(1);
    let initial_decoder = {
        let reader = OffsetReader::new(shared.clone(), 0);
        Box::new(EncodedDecoder::new(
            reader,
            v0_mono_spec,
            V0_SAMPLE_SIZE,
            50,
        ))
    };

    // Factory: V1 (16 bytes/sample)
    let factory = make_encoded_factory(v1_spec(), V1_SAMPLE_SIZE);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut src = StreamAudioSource::new(
        shared,
        initial_decoder,
        factory,
        Some(v0_mono_info),
        epoch,
        vec![],
    );

    // Collect all PCM samples
    let mut all_pcm: Vec<f32> = Vec::new();
    loop {
        let fetch = src.fetch_next();
        if !fetch.data.pcm.is_empty() {
            all_pcm.extend_from_slice(&fetch.data.pcm);
        }
        if fetch.is_eof {
            break;
        }
    }

    // Verify: decode each f32 and check both axes
    let total_samples = 2 * spv * sps; // 76800
    assert_eq!(
        all_pcm.len(),
        total_samples,
        "Expected {total_samples} samples, got {}",
        all_pcm.len()
    );

    for (i, &val) in all_pcm.iter().enumerate() {
        let (variant_segment, sample_index) = decode_pcm_sample(val);
        // variant_segment = global segment index (0..63)
        let expected_vs = (i / sps) as u8;
        assert_eq!(
            variant_segment, expected_vs,
            "sample {i}: expected variant_segment {expected_vs}, got {variant_segment}"
        );
        assert_eq!(
            sample_index, i as u32,
            "sample {i}: expected sample_index {i}, got {sample_index}"
        );
    }
}
