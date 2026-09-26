use kithara_platform::time::Duration;
use kithara_stream::SourcePhase;
use kithara_test_fixtures::unit_fixtures::{RoutePcm, route_pcm};
use kithara_test_utils::kithara;

use super::rebuild::{
    RouteFixture, produced_data, route_signal_source, route_signal_source_with_eof,
};
use crate::{
    consts,
    pipeline::{
        seek::{ResumeState, SeekContext},
        track::{
            CurrentFsm, Track, TrackStep, WaitContext, WaitState, WaitingForSource, WaitingReason,
        },
    },
    traits::{AudioSource, AudioSourceExt},
};

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
        matches!(fixture.source.step_track(), TrackStep::StateChanged),
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
        matches!(fixture.source.step_track(), TrackStep::StateChanged),
        "byte-space EOF must resume the wait, not shortcut to Eof"
    );
    for _ in 0..8 {
        match fixture.source.step_track() {
            TrackStep::Eof => {
                assert!(matches!(fixture.source.state, CurrentFsm::AtEof(_)));
                return;
            }
            TrackStep::Failed => {
                panic!("a drained decoder at byte EOF must finalize as EOF, not fail")
            }
            _ => fixture.source.flush_deferred(),
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
    fixture.source.update_state(
        Track::<WaitingForSource>::new(WaitState {
            context: WaitContext::PostSeek(ResumeState {
                seek: SeekContext {
                    epoch: 0,
                    target: Duration::ZERO,
                },
                ..Default::default()
            }),
            reason: WaitingReason::Waiting,
        })
        .erase(),
    );
    *fixture.phase.lock() = SourcePhase::Eof;

    assert!(
        matches!(fixture.source.step_track(), TrackStep::StateChanged),
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
