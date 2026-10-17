use std::{
    collections::VecDeque,
    num::NonZeroU32,
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_abr::{Abr, AbrController, AbrSettings, AbrState};
use kithara_decode::TrackMetadata;
use kithara_events::{DeferredBus, EventBus};
use kithara_platform::{CancelScope, sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{ActivityWriter, PlayheadState};
use kithara_test_fixtures::unit_fixtures::cursor_half;
use kithara_test_utils::kithara;

use super::{audio::*, context::AudioContext};
use crate::{
    AudioRead, AudioSession, AudioSource, Fetch, ReadOutcome, SeekOutcome, SourceSpan, TrackStep,
    WaitingReason,
    test_pools::{Pools, pools, sample_buffer},
};

struct StagedSource {
    chunks: VecDeque<AudioChunk>,
    spec: AudioSpec,
    failure: Option<crate::TrackFailureKind>,
    pending_seek: bool,
    preparations: Option<Arc<AtomicU64>>,
}
impl AudioSource for StagedSource {
    type Chunk = AudioChunk;
    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, crate::AudioReadError> {
        self.chunks.clear();
        if self.pending_seek {
            return Ok(SeekOutcome::Pending {
                target,
                reason: kithara_stream::PendingReason::NotReady(
                    kithara_stream::NotReadyCause::SourcePending,
                ),
            });
        }
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }
    fn set_host_sample_rate(&mut self, rate: NonZeroU32) {
        self.spec.sample_rate = rate;
    }
    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        Some(self.spec.sample_rate)
    }
    fn prepare_deferred(&mut self) -> Option<AudioSpec> {
        if let Some(preparations) = &self.preparations {
            preparations.fetch_add(1, Ordering::Relaxed);
        }
        None
    }
    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        if let Some(failure) = self.failure.take() {
            return TrackStep::Failed(failure);
        }
        self.chunks
            .pop_front()
            .map_or(TrackStep::Blocked(WaitingReason::Waiting), |chunk| {
                TrackStep::Produced(Fetch::data(chunk))
            })
    }
}
fn fixture(spec: AudioSpec, chunks: Vec<AudioChunk>) -> Audio<()> {
    Audio::new(
        Box::new(StagedSource {
            chunks: chunks.into(),
            spec,
            failure: None,
            pending_seek: false,
            preparations: None,
        }),
        context(),
        spec,
    )
}

fn context() -> AudioContext {
    let activity = ActivityWriter::new();
    AudioContext {
        playhead: Arc::new(PlayheadState::new()),
        emit: Arc::new(DeferredBus::new(
            EventBus::default(),
            crate::consts::AUDIO_EVENT_CAPACITY,
        )),
        metadata: TrackMetadata::default(),
        abr: None,
        activity: activity.reader(),
        activity_writer: Some(activity),
        cancel: CancelScope::new(None).token(),
    }
}
fn timed_chunk(
    pools: &Pools,
    pcm: &[f32],
    spec: AudioSpec,
    frames: u32,
    start: Duration,
    end: Duration,
) -> AudioChunk {
    let channels = usize::from(spec.channels.max(1));
    let frame_count = usize::try_from(frames).expect("test frame count fits usize");
    let samples = &pcm[..frame_count * channels];
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            timestamp: start,
            end_timestamp: end,
            frames,
            ..Default::default()
        },
        sample_buffer(pools, samples),
    )
}

#[kithara::test]
#[case::interleaved(0)]
#[case::planar(1)]
#[case::chunk(2)]
fn direct_pulls_publish_consumed_progress(#[case] read_kind: u8, cursor_half: Vec<f32>) {
    let pools = pools();
    let spec = AudioSpec::new(1, NonZeroU32::new(10).expect("test rate"));
    let chunk = timed_chunk(
        &pools,
        &cursor_half,
        spec,
        2,
        Duration::ZERO,
        Duration::from_millis(200),
    );
    let mut audio = fixture(spec, vec![chunk]);
    let mut events = audio.event_bus().subscribe::<crate::AudioEvent>();
    audio.preload().expect("preload chunk");
    assert!(events.try_recv().is_err(), "preload is not playback");
    let expected = match read_kind {
        0 => {
            audio.read(&mut [0.0]).expect("partial interleaved pull");
            100
        }
        1 => {
            audio
                .read_planar(&mut [&mut [0.0]])
                .expect("partial planar pull");
            100
        }
        _ => {
            audio.next_chunk().expect("whole chunk pull");
            200
        }
    };
    assert_eq!(audio.position(), Duration::from_millis(expected));
    assert!(matches!(
        events.try_recv().expect("committed progress").event,
        crate::AudioEvent::PlaybackProgress { position_ms, .. } if position_ms == expected
    ));
    assert!(events.try_recv().is_err(), "one event per committed pull");
}

#[kithara::hang_watchdog]
fn seek_completions(
    events: &mut kithara_events::EventReceiver<crate::AudioEvent>,
) -> Vec<Duration> {
    let mut completions = Vec::new();
    loop {
        hang_tick!();
        match events.try_recv() {
            Ok(envelope) => {
                hang_reset!();
                if let crate::AudioEvent::SeekComplete { position } = envelope.event {
                    completions.push(position);
                }
            }
            Err(kithara_events::TryRecvError::Empty) => return completions,
            Err(error) => panic!("event receiver failed: {error:?}"),
        }
    }
}

#[kithara::test]
fn off_rt_read_publishes_the_seek_completion_it_births(cursor_half: Vec<f32>) {
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
    let mut audio = fixture(spec, Vec::new());
    let mut events = audio.event_bus().subscribe::<crate::AudioEvent>();
    audio.preload().expect("mark the direct reader preloaded");
    let target = Duration::from_millis(250);
    crate::AudioControl::seek(&mut audio, target).expect("seek the direct reader");
    let mut chunk = timed_chunk(
        &pools(),
        &cursor_half,
        spec,
        4,
        target,
        target + spec.duration_for(4).expect("four frames"),
    );
    chunk.meta.frame_offset = 12_000;
    audio.cursor.begin_chunk(&chunk);
    audio.current_chunk = Some(chunk);

    let mut output = [0.0; 8];
    let outcome = audio.read(&mut output).expect("staged read");
    assert!(matches!(outcome, ReadOutcome::Frames { .. }));
    assert!(matches!(outcome, ReadOutcome::Frames { count, .. } if count.get() == 8));
    assert_eq!(
        seek_completions(&mut events),
        vec![target],
        "an off-RT read publishes the completion it births before returning"
    );
    let next = target + spec.duration_for(4).expect("four frames");
    let mut chunk = timed_chunk(
        &pools(),
        &cursor_half,
        spec,
        4,
        next,
        next + spec.duration_for(4).expect("four frames"),
    );
    chunk.meta.frame_offset = 12_004;
    audio.cursor.begin_chunk(&chunk);
    audio.current_chunk = Some(chunk);
    assert!(matches!(
        audio.read(&mut output).expect("next consumed read"),
        ReadOutcome::Frames { count, .. } if count.get() == 8
    ));
    assert_eq!(
        seek_completions(&mut events),
        Vec::<Duration>::new(),
        "later consumed PCM does not complete the same seek twice"
    );
}

struct SeekAbr {
    state: Arc<AbrState>,
    cancel: kithara_platform::CancelToken,
}

impl Abr for SeekAbr {
    fn cancel(&self) -> kithara_platform::CancelToken {
        self.cancel.clone()
    }

    fn state(&self) -> Option<Arc<AbrState>> {
        Some(self.state.clone())
    }
}

#[kithara::test]
#[case::interleaved(0)]
#[case::planar(1)]
#[case::chunk(2)]
#[case::render_commit(3)]
#[case::drop(4)]
#[case::retained_failure(5)]
fn seek_owns_one_abr_lock_until_post_seek_consumption(
    #[case] completion: u8,
    cursor_half: Vec<f32>,
) {
    for pending_seek in [false, true] {
        let cancel = CancelScope::new(None).token();
        let state = Arc::new(AbrState::new(kithara_abr::AbrMode::Auto(None)));
        let peer: Arc<dyn Abr> = Arc::new(SeekAbr {
            state: state.clone(),
            cancel: cancel.clone(),
        });
        let controller = AbrController::new(AbrSettings::builder().cancel(cancel).build());
        let handle = controller.register(&peer);
        handle.lock();
        let spec = AudioSpec::new(1, NonZeroU32::new(10).expect("test rate"));
        let mut context = context();
        context.abr = Some(handle.clone());
        let emit = context.emit.clone();
        let preparations = Arc::new(AtomicU64::new(0));
        let mut audio = Audio::<()>::new(
            Box::new(StagedSource {
                chunks: VecDeque::new(),
                spec,
                failure: (completion == 5).then_some(crate::TrackFailureKind::SourceCancelled),
                pending_seek,
                preparations: Some(preparations.clone()),
            }),
            context,
            spec,
        );
        let mut events = audio.event_bus().subscribe::<crate::AudioEvent>();
        let accepted = if pending_seek {
            SeekOutcome::Pending {
                target: Duration::ZERO,
                reason: kithara_stream::PendingReason::NotReady(
                    kithara_stream::NotReadyCause::SourcePending,
                ),
            }
        } else {
            SeekOutcome::Landed {
                target: Duration::ZERO,
                landed_at: Duration::ZERO,
            }
        };
        assert_eq!(audio.seek(Duration::ZERO).expect("opening seek"), accepted);
        assert_eq!(
            preparations.load(Ordering::Relaxed),
            u64::from(!pending_seek),
            "only a synchronous landing prepares eagerly"
        );
        assert_eq!(state.lock_count(), 1, "opening leaves ABR available");
        assert_eq!(
            crate::AudioControl::seek(&mut audio, Duration::ZERO).expect("seek"),
            accepted
        );
        assert_eq!(
            preparations.load(Ordering::Relaxed),
            2 * u64::from(!pending_seek)
        );
        assert_eq!(state.lock_count(), 2, "the seek owns one lock level");
        assert!(seek_completions(&mut events).is_empty());
        if completion == 5 {
            assert!(audio.preload().is_err());
            assert_eq!(
                state.lock_count(),
                1,
                "source failure releases the seek lock"
            );
            assert!(crate::AudioControl::seek(&mut audio, Duration::ZERO).is_err());
            assert_eq!(state.lock_count(), 1, "failed seek releases its lock");
        } else {
            audio.preload().expect("pending preload");
            assert_eq!(
                state.lock_count(),
                2,
                "pending output keeps the seek locked"
            );
            let prepared = preparations.load(Ordering::Relaxed);
            assert_eq!(
                crate::AudioControl::seek(&mut audio, Duration::ZERO).expect("superseding seek"),
                accepted
            );
            assert_eq!(
                preparations.load(Ordering::Relaxed),
                prepared + u64::from(!pending_seek)
            );
            assert!(seek_completions(&mut events).is_empty());
            assert_eq!(
                state.lock_count(),
                2,
                "supersession does not nest seek locks"
            );
        }
        if completion < 4 {
            let chunk = timed_chunk(
                &pools(),
                &cursor_half,
                spec,
                2,
                Duration::ZERO,
                Duration::from_millis(200),
            );
            audio.cursor.begin_chunk(&chunk);
            audio.current_chunk = Some(chunk);
            match completion {
                0 => {
                    audio.read(&mut [0.0]).expect("interleaved output");
                }
                1 => {
                    audio.read_planar(&mut [&mut [0.0]]).expect("planar output");
                }
                2 => {
                    audio.next_chunk().expect("chunk output");
                }
                _ => {
                    let chunk = audio.current_chunk.take().expect("prepared output");
                    let end = source_end(&chunk.meta, u64::from(chunk.meta.frames))
                        .expect("source boundary");
                    audio.commit_source_end(end, chunk.meta);
                }
            }
            assert_eq!(state.lock_count(), 1, "only the caller's lock remains");
            emit.flush();
            assert_eq!(seek_completions(&mut events), vec![Duration::ZERO]);
        }
        drop(audio);
        assert_eq!(
            state.lock_count(),
            1,
            "dropping releases only the seek lock"
        );
        handle.unlock();
        assert!(!handle.is_locked());
    }
}
#[kithara::test]
fn reads_preserve_consecutive_rendered_source_spans_and_revisions(cursor_half: Vec<f32>) {
    let pools = pools();
    let rate = NonZeroU32::new(48_000).expect("test rate");
    let spec = AudioSpec::new(1, rate);
    let mut first = timed_chunk(
        &pools,
        &cursor_half,
        spec,
        3,
        Duration::ZERO,
        Duration::from_millis(3),
    );
    first.meta.frame_offset = 100;
    first.meta.render_revision = 7;
    let mut second = timed_chunk(
        &pools,
        &cursor_half,
        spec,
        2,
        Duration::from_millis(3),
        Duration::from_millis(5),
    );
    second.meta.frame_offset = 1_000;
    second.meta.render_revision = 7;
    let mut changed = timed_chunk(
        &pools,
        &cursor_half,
        spec,
        2,
        Duration::from_millis(5),
        Duration::from_millis(7),
    );
    changed.meta.frame_offset = 2_000;
    changed.meta.render_revision = 8;
    first.meta.source_span =
        SourceSpan::new(100, 106, rate, 3).map(|span| span.with_render_revision(7));
    second.meta.source_span =
        SourceSpan::new(106, 110, rate, 2).map(|span| span.with_render_revision(7));
    changed.meta.source_span =
        SourceSpan::new(110, 114, rate, 2).map(|span| span.with_render_revision(8));
    let mut audio = fixture(spec, vec![first, second, changed]);
    audio.preload().expect("prime first chunk");
    let first_output_meta = audio.current_chunk.as_ref().map(|chunk| chunk.meta);
    let mut output = [0.0; 8];
    let first_read = audio.read(&mut output).expect("first read succeeds");
    assert_eq!(
        first_output_meta.map(|meta| meta.timestamp),
        Some(Duration::ZERO)
    );
    let ReadOutcome::Frames {
        count, source_span, ..
    } = first_read
    else {
        panic!("expected first rendered frames");
    };
    assert_eq!(count.get(), 5);
    assert_eq!(
        source_span,
        SourceSpan::new(100, 110, rate, 5).map(|span| span.with_render_revision(7))
    );

    let second_read = audio
        .read(&mut output[..1])
        .expect("partial changed-revision read succeeds");
    let ReadOutcome::Frames { source_span, .. } = second_read else {
        panic!("expected partial changed-revision frames");
    };
    assert_eq!(
        source_span,
        SourceSpan::new(110, 112, rate, 1).map(|span| span.with_render_revision(8))
    );

    let final_read = audio
        .read(&mut output)
        .expect("final changed-revision read succeeds");
    let ReadOutcome::Frames { source_span, .. } = final_read else {
        panic!("expected final changed-revision frames");
    };
    assert_eq!(
        source_span,
        SourceSpan::new(112, 114, rate, 1).map(|span| span.with_render_revision(8))
    );
}
#[kithara::test]
fn planar_read_crosses_chunks_but_preserves_mapping_boundaries() {
    let config = kithara_bufpool::PoolConfig::builder()
        .max_buffers(32)
        .max_retained_capacity(1)
        .build();
    let pools = crate::test_pools::pools_with(1024 * 1024, config, config);
    let rate = NonZeroU32::new(48_000).expect("test rate");
    let spec = AudioSpec::new(2, rate);
    let mut chunks = Vec::new();
    let first_map = std::num::NonZeroU64::new(1);
    let next_map = std::num::NonZeroU64::new(2);
    for (offset, pcm, mapping) in [
        (0, [0.0, 10.0, 1.0, 11.0], first_map),
        (2, [2.0, 12.0, 3.0, 13.0], first_map),
        (4, [4.0, 14.0, 5.0, 15.0], next_map),
    ] {
        let mut chunk = timed_chunk(
            &pools,
            &pcm,
            spec,
            2,
            Duration::ZERO,
            Duration::from_millis(1),
        );
        chunk.meta.frame_offset = offset;
        chunk.meta.mapping_revision = mapping;
        chunk.meta.source_span = SourceSpan::new(offset, offset + 2, rate, 2)
            .map(|span| span.with_mapping_revision(mapping));
        chunks.push(chunk);
    }
    let first_bytes = chunks[0].samples.capacity() * size_of::<f32>();
    let second_bytes = chunks[1].samples.capacity() * size_of::<f32>();
    let mut audio = fixture(spec, chunks);
    let allocated_before = pools.stats().allocated_bytes;
    let mut left = [-1.0; 5];
    let mut right = [-1.0; 5];
    let read = audio
        .read_planar(&mut [&mut left, &mut right])
        .expect("planar read succeeds");
    let ReadOutcome::Frames {
        count, source_span, ..
    } = read
    else {
        panic!("expected planar frames");
    };
    assert_eq!(count.get(), 4);
    assert_eq!(left, [0.0, 1.0, 2.0, 3.0, -1.0]);
    assert_eq!(right, [10.0, 11.0, 12.0, 13.0, -1.0]);
    assert_eq!(
        source_span,
        SourceSpan::new(0, 4, rate, 4).map(|span| span.with_mapping_revision(first_map))
    );
    assert!(pools.stats().allocated_bytes <= allocated_before - first_bytes);
    assert!(pools.stats().allocated_bytes <= allocated_before - first_bytes - second_bytes);
    assert_eq!(
        pools.stats().allocated_bytes,
        allocated_before - first_bytes - second_bytes
    );
    assert_eq!(
        audio
            .current_chunk
            .as_ref()
            .expect("next map remains resident")
            .meta
            .mapping_revision,
        next_map
    );
    assert_eq!(audio.cursor.consumed_frames(), 0);
}
#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(hang_timeout_secs(1))]
fn preload_returns_when_the_producer_has_delivered_nothing() {
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
    let mut audio = fixture(spec, Vec::new());
    audio
        .preload()
        .expect("preload primes whatever the producer delivered");
    assert!(audio.is_preloaded());
}

#[kithara::test]
fn partial_reads_keep_resumed_source_frames_and_rational_mapping(cursor_half: Vec<f32>) {
    let pools = pools();
    let output_rate = NonZeroU32::new(48_000).expect("output rate");
    let source_rate = NonZeroU32::new(44_100).expect("source rate");
    let spec = AudioSpec::new(2, output_rate);
    let span = SourceSpan::try_from((
        128 * u128::from(output_rate.get()),
        u128::from(source_rate.get()),
        output_rate.into(),
        source_rate,
        5,
    ))
    .expect("exact resumed mapping")
    .with_mapping_revision(std::num::NonZeroU64::new(4))
    .with_render_revision(7);
    let mut chunk = timed_chunk(
        &pools,
        &cursor_half,
        spec,
        5,
        span.position_at(0).expect("mapped start"),
        span.position_at(5).expect("mapped end"),
    );
    chunk.meta.frame_offset = 139;
    chunk.meta.source_span = Some(span);
    chunk.meta.mapping_revision = span.mapping_revision();
    chunk.meta.render_revision = span.render_revision();
    let mut audio = fixture(spec, vec![chunk]);
    let read = audio.read(&mut [0.0; 4]).expect("partial mapped read");
    let ReadOutcome::Frames {
        count, source_span, ..
    } = read
    else {
        panic!("expected partial mapped frames");
    };
    assert_eq!(count.get(), 4);
    assert_eq!(source_span, span.for_output_range(0..2));
    let current = audio.current_chunk.as_ref().expect("partially read chunk");
    assert_eq!(
        source_end(&current.meta, audio.cursor.consumed_frames())
            .expect("rendered source boundary"),
        crate::SourceEnd::new(129, source_rate).with_mapping_revision(span.mapping_revision()),
    );
    let crate::ChunkOutcome::Chunk(remaining) = audio.next_chunk().expect("remaining mapped PCM")
    else {
        panic!("expected remaining mapped chunk");
    };
    assert_eq!(remaining.meta.frames, 3);
    assert_eq!(remaining.meta.source_span, span.for_output_range(2..5));
    assert_eq!(Some(remaining.meta.timestamp), span.position_at(2));
    assert_eq!(Some(remaining.meta.end_timestamp), span.position_at(5));
}

#[kithara::test]
fn audio_cache_read_view_follows_the_source_coverage_and_duration() {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"));
    let metadata = context();
    let playhead = Arc::clone(&metadata.playhead);
    let audio: Audio<()> = Audio::new(
        Box::new(StagedSource {
            chunks: VecDeque::new(),
            spec,
            failure: None,
            pending_seek: false,
            preparations: None,
        }),
        metadata,
        spec,
    );
    let view = audio.playhead_read();

    playhead.set_cached(1, 4);
    assert_eq!(AudioRead::cached_span(&audio), Duration::ZERO);
    assert_eq!(view.cached(), Duration::ZERO);

    playhead.set_duration(Some(Duration::from_secs(187)));
    assert_eq!(
        AudioRead::cached_span(&audio),
        Duration::from_millis(46_750)
    );
    assert_eq!(view.cached(), Duration::from_millis(46_750));

    playhead.set_cached(4, 4);
    assert_eq!(AudioRead::cached_span(&audio), Duration::from_secs(187));
    assert_eq!(view.cached(), Duration::from_secs(187));

    playhead.set_cached(3, 0);
    assert_eq!(AudioRead::cached_span(&audio), Duration::from_secs(187));
    assert_eq!(view.cached(), Duration::from_secs(187));
}
