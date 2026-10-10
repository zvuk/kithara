use std::num::NonZeroU32;

use kithara_abr::AbrState;
use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, GaplessInfo,
    GaplessMode,
};
use kithara_events::{DeferredBus, EventBus};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::AudioSpec;
use kithara_stream::{AudioCodec, PrerollHint, SourcePhase, VariantPromotion, VariantTransition};
use kithara_test_fixtures::unit_fixtures::{RoutePcm, route_pcm};
use kithara_test_utils::{flight, kithara};

use crate::{
    DecodeErrorKind, DecoderEvent, consts,
    pipeline::source::tests::{
        AudioEvent, AudioLaneEvent, AudioSource, DecoderFactory, DecoderGeneration, OwnerPhase,
        RecreateCause, RecreateState, TrackFailureKind, TrackStep, WaitingReason,
        rebuild::{
            media_info, produced_data, route_signal_source, route_signal_source_with_gapless_eof,
            test_source,
        },
    },
};

fn decode_failure(error: DecodeError) -> (TrackFailureKind, Option<DecodeError>) {
    (
        TrackFailureKind::Decode {
            kind: crate::map_decode_error_kind(&error),
        },
        Some(error),
    )
}

struct FailedDecoder {
    seek_error: Option<DecodeError>,
}

impl Decoder for FailedDecoder {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }
    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        Err(DecodeError::InvalidData {
            detail: "fixture decode failure",
        })
    }
    fn seek(&mut self, _position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        Err(self.seek_error.take().expect("one scripted seek"))
    }
    fn spec(&self) -> AudioSpec {
        AudioSpec::new(2, NonZeroU32::MIN)
    }
    fn update_byte_len(&self, _len: u64) {}
}

#[kithara::test(native, tokio, tracing("warn"))]
#[case::decode_shell(
    decode_failure(DecodeError::InvalidData { detail: "terminal-log-contract" }),
    "terminal-log-contract",
    false
)]
#[case::decode_drop(
    decode_failure(DecodeError::InvalidData { detail: "terminal-log-contract" }),
    "terminal-log-contract",
    true
)]
#[case::recreate_shell((TrackFailureKind::RecreateFailed { offset: 8193 }, None), "8193", false)]
#[case::recreate_drop((TrackFailureKind::RecreateFailed { offset: 8193 }, None), "8193", true)]
#[case::cancel_shell((TrackFailureKind::SourceCancelled, None), "source cancelled", false)]
#[case::cancel_drop((TrackFailureKind::SourceCancelled, None), "source cancelled", true)]
async fn terminal_failure_is_logged_once_without_dispatch_reentry(
    route_pcm: RoutePcm,
    #[case] failure: (TrackFailureKind, Option<DecodeError>),
    #[case] detail: &str,
    #[case] teardown_only: bool,
) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    assert!(
        failure_log_output().is_empty(),
        "the fixture must start without a track failure"
    );
    fixture.source.fail(failure.0, failure.1);
    assert!(
        failure_log_output().is_empty(),
        "the produce core must not format diagnostics"
    );
    if teardown_only {
        drop(fixture.source);
    } else {
        fixture.source.finish_deferred();
        let first = failure_log_output();
        assert_eq!(
            first.len(),
            1,
            "a shell pass must record the terminal failure: {first:?}"
        );
        fixture.source.finish_deferred();
        assert_eq!(
            failure_log_output(),
            first,
            "a second shell pass must not repeat the failure"
        );
        drop(fixture.source);
        assert_eq!(
            failure_log_output(),
            first,
            "teardown must not repeat the shell's failure log"
        );
    }

    let logged = failure_log_output();
    assert_eq!(logged.len(), 1, "the failure is recorded once: {logged:?}");
    assert!(
        logged[0].contains(detail),
        "the original failure detail must survive: {logged:?}"
    );
    assert!(
        !logged[0].contains(" (x"),
        "the failure must not carry a folded repeat count: {logged:?}"
    );
}

#[kithara::test(native, tokio, tracing("warn"))]
#[case::decode_invalid(
    decode_failure(DecodeError::InvalidData { detail: "terminal-reentry-invalid" }),
    "terminal-reentry-invalid"
)]
#[case::decode_unsupported(
    decode_failure(DecodeError::UnsupportedCodec { codec: AudioCodec::Mp3 }),
    "Mp3"
)]
#[case::recreate((TrackFailureKind::RecreateFailed { offset: 8193 }, None), "8193")]
#[case::cancel((TrackFailureKind::SourceCancelled, None), "source cancelled")]
async fn terminal_failure_reentry_defers_one_full_diagnostic(
    route_pcm: RoutePcm,
    #[case] failure: (TrackFailureKind, Option<DecodeError>),
    #[case] detail: &str,
) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    fixture.source.fail(failure.0, failure.1);
    for _ in 0..2 {
        assert!(matches!(fixture.source.step_track(), TrackStep::Failed(_)));
        assert!(
            failure_log_output().is_empty(),
            "Failed reentry must not format a terminal diagnostic on the produce core"
        );
    }

    fixture.source.finish_deferred();
    let first = failure_log_output();
    assert_eq!(
        first.len(),
        1,
        "the shell must drain the failure once: {first:?}"
    );
    assert!(
        first[0].contains(detail),
        "the full cause must survive: {first:?}"
    );
    assert!(matches!(fixture.source.step_track(), TrackStep::Failed(_)));
    fixture.source.finish_deferred();
    drop(fixture.source);
    assert_eq!(
        failure_log_output(),
        first,
        "later produce, shell, and teardown passes must not repeat the diagnostic"
    );
}

fn failure_log_output() -> Vec<String> {
    flight::tail()
        .into_iter()
        .filter(|line| line.contains("track failed:"))
        .collect()
}

enum TerminalTransition {
    Cancel,
    Recreate,
    Seek,
}

#[kithara::test(native, tokio, tracing("warn"))]
#[case::cancel(TerminalTransition::Cancel, "source cancelled")]
#[case::recreate(TerminalTransition::Recreate, "8193")]
#[case::failed_seek(TerminalTransition::Seek, "terminal-seek-failure")]
async fn real_terminal_transitions_wait_for_the_diagnostic_shell(
    route_pcm: RoutePcm,
    #[case] transition: TerminalTransition,
    #[case] detail: &str,
) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    assert!(failure_log_output().is_empty());
    let expected = match transition {
        TerminalTransition::Cancel => {
            *fixture.phase.lock() = SourcePhase::Cancelled;
            assert!(matches!(
                fixture.source.step_track(),
                TrackStep::Failed(TrackFailureKind::SourceCancelled)
            ));
            assert!(matches!(
                fixture.source.phase,
                OwnerPhase::Failed {
                    failure: TrackFailureKind::SourceCancelled,
                    ..
                }
            ));
            TrackFailureKind::SourceCancelled
        }
        TerminalTransition::Recreate => {
            fixture.source.factory = DecoderFactory::new(
                |_, _, _| {
                    Err(DecodeError::InvalidData {
                        detail: "recreate failure",
                    })
                },
                None,
            );
            let error = fixture
                .source
                .install_replacement(
                    RecreateState {
                        cause: RecreateCause::FormatBoundary,
                        media_info: Some(media_info(0)),
                        offset: 8193,
                    },
                    None,
                )
                .expect_err("failed real reconstruction");
            let failure = fixture.source.fail(
                TrackFailureKind::RecreateFailed { offset: 8193 },
                Some(error),
            );
            assert_eq!(failure, TrackFailureKind::RecreateFailed { offset: 8193 });
            assert!(matches!(
                fixture.source.phase,
                OwnerPhase::Failed {
                    failure: TrackFailureKind::RecreateFailed { offset: 8193 },
                    ..
                }
            ));
            TrackFailureKind::RecreateFailed { offset: 8193 }
        }
        TerminalTransition::Seek => {
            let replacement = DecoderGeneration::new(
                Box::new(FailedDecoder {
                    seek_error: Some(DecodeError::SeekFailed {
                        detail: "terminal-seek-failure",
                    }),
                }),
                Some(media_info(0)),
                0,
                None,
                None,
                GaplessMode::Disabled,
            );
            drop(fixture.source.decode.replace_active(replacement));
            let error = fixture
                .source
                .seek_owned(Duration::from_millis(20))
                .expect_err("real failed seek");
            let crate::AudioReadError::Decode(error) = error else {
                panic!("decoder seek must preserve its direct decode error");
            };
            let failure = decode_failure(error);
            fixture.source.fail(failure.0, failure.1);
            assert!(matches!(
                fixture.source.phase,
                OwnerPhase::Failed {
                    error: Some(DecodeError::SeekFailed {
                        detail: "terminal-seek-failure"
                    }),
                    ..
                }
            ));
            TrackFailureKind::Decode {
                kind: DecodeErrorKind::SeekFailed,
            }
        }
    };
    assert!(
        matches!(fixture.source.phase, OwnerPhase::Failed { failure, .. } if failure == expected)
    );
    assert!(
        failure_log_output().is_empty(),
        "the transition must not emit the terminal log"
    );
    assert!(matches!(fixture.source.step_track(), TrackStep::Failed(_)));
    assert!(
        failure_log_output().is_empty(),
        "the later Failed step must not emit the terminal log"
    );
    fixture.source.finish_deferred();
    let first = failure_log_output();
    assert_eq!(
        first.len(),
        1,
        "the shell must report the actual terminal transition"
    );
    assert!(first[0].contains(detail), "{first:?}");
    fixture.source.finish_deferred();
    drop(fixture.source);
    assert_eq!(failure_log_output(), first);
}

#[kithara::test(tokio)]
async fn decode_error_precedes_track_failure_on_event_bus() {
    let mut source = test_source(0).await.source;
    let bus = EventBus::new(16);
    let mut events = bus.subscribe();
    source.emit = Arc::new(DeferredBus::new(bus, 16));
    let replacement = DecoderGeneration::new(
        Box::new(FailedDecoder {
            seek_error: Some(DecodeError::Interrupted),
        }),
        Some(media_info(0)),
        0,
        None,
        None,
        GaplessMode::Disabled,
    );
    drop(source.decode.replace_active(replacement));

    assert!(matches!(source.step_track(), TrackStep::Failed(_)));
    assert!(events.try_recv().is_err());
    source.finish_deferred();

    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(AudioLaneEvent::Decoder(DecoderEvent::DecodeError {
            detail: "fixture decode failure",
            ..
        }))
    ));
    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(AudioLaneEvent::Audio(AudioEvent::TrackFailed {
            failure: TrackFailureKind::Decode {
                kind: DecodeErrorKind::InvalidData,
            },
        }))
    ));
}

#[kithara::test(tokio)]
async fn rebuild_factory_panic_fails_track_without_hang() {
    let mut source = test_source(1).await.source;
    source.factory = DecoderFactory::new(|_, _, _| panic!("decoder construction blew up"), None);
    source.set_host_sample_rate(NonZeroU32::new(96_000).expect("host rate"));
    assert!(matches!(
        source.step_track(),
        TrackStep::Failed(TrackFailureKind::RecreateFailed { offset: 0 })
    ));
    assert!(matches!(
        source.phase,
        OwnerPhase::Failed {
            failure: TrackFailureKind::RecreateFailed { offset: 0 },
            ..
        }
    ));
    source.finish_deferred();
    assert!(matches!(
        source.step_track(),
        TrackStep::Failed(TrackFailureKind::RecreateFailed { offset: 0 })
    ));
}

struct ByteEofDecoder {
    phase: Arc<kithara_platform::sync::Mutex<SourcePhase>>,
}

impl Decoder for ByteEofDecoder {
    fn duration(&self) -> Option<Duration> {
        None
    }
    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        if *self.phase.lock() == SourcePhase::Waiting {
            Ok(DecoderChunkOutcome::Pending(
                kithara_stream::PendingReason::NotReady(
                    kithara_stream::NotReadyCause::SourcePending,
                ),
            ))
        } else {
            Ok(DecoderChunkOutcome::Eof)
        }
    }
    fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        Ok(DecoderSeekOutcome::Landed {
            landed_at: position,
            landed_frame: 0,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
        })
    }
    fn spec(&self) -> AudioSpec {
        AudioSpec::new(2, NonZeroU32::MIN)
    }
    fn update_byte_len(&self, _len: u64) {}
}

#[kithara::test(tokio)]
async fn byte_eof_still_ends_a_drained_decoder_through_the_decode_path(route_pcm: RoutePcm) {
    let mut fixture = route_signal_source(&route_pcm, consts::SAMPLE_RATE).await;
    let generation = DecoderGeneration::new(
        Box::new(ByteEofDecoder {
            phase: fixture.phase.clone(),
        }),
        Some(media_info(0)),
        0,
        None,
        None,
        GaplessMode::Disabled,
    );
    drop(fixture.source.decode.replace_active(generation));
    *fixture.phase.lock() = SourcePhase::Waiting;
    assert!(
        matches!(
            fixture.source.step_track(),
            TrackStep::Blocked(WaitingReason::Waiting)
        ),
        "a waiting source must park the decoding track"
    );
    *fixture.phase.lock() = SourcePhase::Eof;
    assert!(
        matches!(fixture.source.step_track(), TrackStep::StateChanged),
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
            _ => fixture.source.finish_deferred(),
        }
    }
    panic!("a drained decoder at byte EOF must still finalize the track");
}

#[kithara::test(tokio)]
async fn seek_after_terminal_failure_returns_the_first_cause() {
    let mut source = test_source(0).await.source;
    let failure = TrackFailureKind::RecreateFailed { offset: 8193 };
    source.fail(failure, None);
    source.factory = DecoderFactory::new(|_, _, _| panic!("failed source must not rebuild"), None);
    let error = source
        .seek(Duration::from_secs(1))
        .expect_err("terminal source");
    assert_eq!(TrackFailureKind::from(&error), failure);
    assert!(matches!(source.step_track(), TrackStep::Failed(actual) if actual == failure));
}

#[kithara::test(tokio)]
async fn gapless_eof_flushes_once_and_drains_every_frame_across_repeated_ticks(
    route_pcm: RoutePcm,
) {
    const RAW_CHUNKS: usize = 4;
    const TRAILING_FRAMES: u64 = 300;

    let mut fixture = route_signal_source_with_gapless_eof(
        &route_pcm,
        consts::SAMPLE_RATE,
        GaplessInfo::new(0, TRAILING_FRAMES),
        RAW_CHUNKS,
    )
    .await;
    let abr = AbrState::new(kithara_abr::AbrMode::Auto(Some(
        kithara_abr::VariantIndex::new(0),
    )));
    abr.request_target(
        kithara_abr::VariantIndex::new(1),
        kithara_abr::AbrReason::ManualOverride,
    );
    let claim = abr
        .claim_pending_decision(kithara_abr::VariantIndex::new(0))
        .expect("exact transition fixture requires a pending ABR claim");
    let transition = VariantTransition::new(
        kithara_stream::VariantTransitionId::new(claim.ticket()),
        kithara_abr::VariantIndex::new(0),
        kithara_abr::VariantIndex::new(1),
    );
    let plan = kithara_stream::VariantReaderPlan::new(transition, media_info(1), Duration::ZERO);
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();
    fixture.control.set_promotion(VariantPromotion::Deferred);
    let _ = fixture.source.prepare_deferred();
    fixture.source.finish_deferred();
    assert!(fixture.source.decode.incoming_is_priming(transition));

    let expected = RAW_CHUNKS
        .saturating_mul(consts::ROUTE_CHUNK_FRAMES)
        .saturating_sub(usize::try_from(TRAILING_FRAMES).unwrap_or(usize::MAX));
    let mut frames = 0usize;
    let mut next_offset = 0u64;
    // Drain while the deferred transition is in flight until the source
    // exhausts; EOF must stay held for the transition and never surface.
    while !fixture.source.decode.active().is_source_exhausted() {
        match fixture.source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                assert_eq!(chunk.meta.frame_offset, next_offset);
                next_offset = next_offset.saturating_add(u64::from(chunk.meta.frames));
                frames = frames.saturating_add(chunk.frames());
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            TrackStep::Eof => panic!("EOF must stay held while a transition is in flight"),
            TrackStep::Failed(_) => panic!("finite gapless fixture must reach EOF cleanly"),
        }
        let _ = fixture.source.prepare_deferred();
        fixture.source.finish_deferred();
    }

    // Resolve the wedged transition: the next promote attempt retires the
    // incoming, releasing the held EOF so it can finalize and flush the
    // gapless boundary exactly once.
    fixture.control.set_promotion(VariantPromotion::Stale);
    loop {
        match fixture.source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                assert_eq!(chunk.meta.frame_offset, next_offset);
                next_offset = next_offset.saturating_add(u64::from(chunk.meta.frames));
                frames = frames.saturating_add(chunk.frames());
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            TrackStep::Eof => break,
            TrackStep::Failed(_) => panic!("finite gapless fixture must reach EOF cleanly"),
        }
        let _ = fixture.source.prepare_deferred();
        fixture.source.finish_deferred();
    }

    assert_eq!(
        frames, expected,
        "EOF must release every non-trimmed frame once"
    );
    assert_eq!(next_offset, u64::try_from(expected).unwrap_or(u64::MAX));
}
