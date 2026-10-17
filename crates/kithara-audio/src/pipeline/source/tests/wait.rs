use std::sync::atomic::{AtomicUsize, Ordering};

use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, GaplessMode,
};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::AudioSpec;
use kithara_stream::SourcePhase;
use kithara_test_fixtures::unit_fixtures::{RoutePcm, route_pcm};
use kithara_test_utils::kithara;

use super::rebuild::{
    RouteFixture, RouteSignalDecoder, media_info, produced_data, route_signal_source,
    route_signal_source_with_eof,
};
use crate::{
    TrackFailureKind, consts,
    pipeline::{
        decode::DecoderGeneration,
        fetch::Fetch,
        source::core::OwnerPhase,
        track::{TrackStep, WaitingReason},
    },
    traits::AudioSource,
};

#[derive(Clone, Copy)]
enum RetainedKind {
    Chunk,
    Error,
    Eof,
}

struct PrefillDecoder {
    inner: RouteSignalDecoder,
    prepared: Option<DecodeResult<DecoderChunkOutcome>>,
    consumed: Arc<AtomicUsize>,
}

impl Decoder for PrefillDecoder {
    delegate::delegate! {
        to self.inner {
            fn duration(&self) -> Option<kithara_decode::DecoderDuration>;
            fn spec(&self) -> AudioSpec;
            fn update_byte_len(&self, len: u64);
        }
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        if let Some(result) = self.prepared.take() {
            self.consumed.fetch_add(1, Ordering::Relaxed);
            result
        } else {
            self.inner.next_chunk()
        }
    }

    fn has_prepared_result(&self) -> bool {
        matches!(
            self.prepared,
            Some(Ok(DecoderChunkOutcome::Chunk(_) | DecoderChunkOutcome::Eof) | Err(_))
        )
    }

    fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        self.prepared = None;
        self.inner.seek(position)
    }
}

fn retain_result(
    fixture: &mut RouteFixture,
    route_pcm: &RoutePcm,
    kind: RetainedKind,
) -> Arc<AtomicUsize> {
    let consumed = Arc::new(AtomicUsize::new(0));
    let mut inner = RouteSignalDecoder::new(
        route_pcm,
        2,
        consts::SAMPLE_RATE,
        None,
        None,
        Arc::clone(&fixture.drops),
        fixture.pools.clone(),
    );
    let prepared = match kind {
        RetainedKind::Chunk => inner.next_chunk(),
        RetainedKind::Error => Err(DecodeError::InvalidData {
            detail: "retained pre-seek error",
        }),
        RetainedKind::Eof => Ok(DecoderChunkOutcome::Eof),
    };
    let decoder = PrefillDecoder {
        inner,
        prepared: Some(prepared),
        consumed: Arc::clone(&consumed),
    };
    let outgoing = fixture.source.decode.replace_active(DecoderGeneration::new(
        Box::new(decoder),
        Some(media_info(0)),
        0,
        None,
        None,
        GaplessMode::Disabled,
    ));
    drop(outgoing);
    fixture.source.waiting_for_source = true;
    consumed
}

#[kithara::test(tokio)]
#[case::decoding_pcm(RetainedKind::Chunk, false)]
#[case::decoding_error(RetainedKind::Error, false)]
#[case::decoding_eof(RetainedKind::Eof, false)]
#[case::waiting_pcm(RetainedKind::Chunk, true)]
#[case::waiting_error(RetainedKind::Error, true)]
#[case::waiting_eof(RetainedKind::Eof, true)]
async fn cancellation_preempts_a_retained_completed_result(
    route_pcm: RoutePcm,
    #[case] kind: RetainedKind,
    #[case] parked: bool,
) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    let consumed = retain_result(&mut fixture, &route_pcm, kind);
    if !parked {
        fixture.source.waiting_for_source = false;
    }
    assert!(
        fixture
            .source
            .decode
            .active()
            .decoder()
            .has_prepared_result()
    );
    *fixture.phase.lock() = SourcePhase::Cancelled;

    assert!(matches!(
        fixture.source.step_track(),
        TrackStep::Failed(TrackFailureKind::SourceCancelled)
    ));
    assert_eq!(consumed.load(Ordering::Relaxed), 0);
    assert!(matches!(fixture.source.phase, OwnerPhase::Failed { .. }));
}

#[kithara::test(tokio)]
#[case::pcm(RetainedKind::Chunk)]
#[case::error(RetainedKind::Error)]
#[case::eof(RetainedKind::Eof)]
async fn a_new_seek_discards_a_retained_completed_result(
    route_pcm: RoutePcm,
    #[case] kind: RetainedKind,
) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    let consumed = retain_result(&mut fixture, &route_pcm, kind);
    *fixture.phase.lock() = SourcePhase::Waiting;
    let target = Duration::from_secs(1);
    fixture.source.seek(target).expect("new seek");
    assert_eq!(consumed.load(Ordering::Relaxed), 0);
    assert!(
        !fixture
            .source
            .decode
            .active()
            .decoder()
            .has_prepared_result()
    );
    *fixture.phase.lock() = SourcePhase::Ready;

    for _ in 0..12 {
        fixture.source.prepare_deferred();
        match fixture.source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                assert_eq!(data.meta.frame_offset, u64::from(consts::SAMPLE_RATE));
                assert!(data.frames() > 0);
                assert!(data.samples.iter().any(|sample| sample.abs() > 0.001));
                assert_eq!(consumed.load(Ordering::Relaxed), 0);
                return;
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            _ => panic!("new seek must resume fresh PCM instead of the retained pre-seek result"),
        }
    }
    panic!("new seek did not produce PCM");
}

/// Park the track in `WaitingForSource(Playback)` the way a transient
/// not-ready source does, then flip the source to byte-space EOF.
fn park_playback_at_byte_eof(fixture: &mut RouteFixture) {
    *fixture.phase.lock() = SourcePhase::Waiting;
    assert!(
        matches!(
            fixture.source.step_track(),
            TrackStep::Blocked(WaitingReason::Waiting)
        ),
        "a waiting source must park the decoding track"
    );
    *fixture.phase.lock() = SourcePhase::Eof;
}

/// A byte-space EOF from the source must not end the track while the decoder
/// can still produce PCM. A seek into the last segment leaves the reader at
/// the stream total with frames still buffered in the demuxer; a transient
/// un-published layout parks the track for one tick, and the next poll sees
/// `SourcePhase::Eof`. Latching `AtEof` from that wait drops the buffered
/// tail (the `live_real_stream_random_seek_prefix_regression` flake) — only
/// the decode path may finalize natural EOF.
#[kithara::test(tokio)]
async fn byte_eof_resumes_decoding_while_the_decoder_still_produces(route_pcm: RoutePcm) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    park_playback_at_byte_eof(&mut fixture);

    assert!(
        matches!(fixture.source.phase, OwnerPhase::Decoding),
        "byte-space EOF must resume the wait into the decode path"
    );
    let TrackStep::Produced(fetch) = fixture.source.step_track() else {
        panic!("the resumed track must produce PCM from the buffered decoder");
    };
    assert!(
        produced_data(fetch).meta.frames > 0,
        "the resumed decode must yield audible PCM"
    );
}

/// With the decoder itself drained, the same parked byte-EOF still ends the
/// track — through the decode path's exhausted finalization, not a wait
/// shortcut (the first step resumes instead of reporting `Eof`).
#[kithara::test(tokio)]
async fn byte_eof_still_ends_a_drained_decoder_through_the_decode_path(route_pcm: RoutePcm) {
    let mut fixture = route_signal_source_with_eof(&route_pcm, consts::SAMPLE_RATE, 0).await;
    park_playback_at_byte_eof(&mut fixture);

    assert!(
        matches!(fixture.source.phase, OwnerPhase::Decoding),
        "byte-space EOF must resume the wait, not shortcut to Eof"
    );
    for _ in 0..8 {
        match fixture.source.step_track() {
            TrackStep::Eof => {
                assert!(matches!(fixture.source.phase, OwnerPhase::AtEof));
                return;
            }
            TrackStep::Failed(_) => {
                panic!("a drained decoder at byte EOF must finalize as EOF, not fail")
            }
            _ => {
                let _ = fixture.source.prepare_deferred();
                fixture.source.finish_deferred();
            }
        }
    }
    panic!("a drained decoder at byte EOF must still finalize the track");
}

/// The post-seek wait is the context that actually holds the flake's tail: a
/// byte-space EOF while awaiting the first post-seek chunk must resume into
/// `AwaitingResume` and decode that tail, not end the track.
#[kithara::test(tokio)]
async fn byte_eof_resumes_a_post_seek_wait_into_the_tail(route_pcm: RoutePcm) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    fixture
        .source
        .seek(Duration::ZERO)
        .expect("synchronous seek");
    park_playback_at_byte_eof(&mut fixture);
    *fixture.phase.lock() = SourcePhase::Eof;

    assert!(
        matches!(fixture.source.phase, OwnerPhase::Decoding),
        "byte-space EOF must resume the post-seek wait"
    );
    let TrackStep::Produced(fetch) = fixture.source.step_track() else {
        panic!("the post-seek tail must decode after a byte-space EOF resume");
    };
    assert!(
        produced_data(fetch).meta.frames > 0,
        "the post-seek resume must yield audible PCM"
    );
}
