use kithara_abr::{AbrMode, AbrReason, AbrState, VariantIndex};
use kithara_decode::{
    DecoderFactory as DecoderBuilder, GaplessInfo, GaplessMode, PcmChunk, duration_for_frames,
    frames_for_duration,
};
use kithara_events::{DecoderChangeCause, DecoderEvent, DeferredBus, Event, EventBus};
use kithara_platform::{sync::Arc, time::Duration, tokio::task::yield_now};
use kithara_stream::{
    AudioCodec, ContainerFormat, MediaInfo, OutgoingDisposition, VariantPromotion,
    VariantReaderPlan, VariantTransition, VariantTransitionId,
};
use kithara_test_utils::kithara;

use super::rebuild::{
    Consts, RouteFixture, TestDecoder, media_info, produced_data, route_signal_source,
    route_signal_source_with_finite_incoming, route_signal_source_with_gapless,
    route_signal_source_with_gapless_eof, route_signal_source_with_gaps,
};
use crate::{
    pipeline::{
        decode::{DecoderGeneration, transition::OutgoingFrontier},
        rebuild::{DecoderBuildComplete, DecoderBuildPurpose, state::BuildId},
        track::{AtEof, CurrentFsm, Failed, Track, TrackFailure, TrackStep},
    },
    renderer::AudioWorkerSource,
};

fn incoming_plan() -> VariantReaderPlan {
    let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    request_incoming_plan(&abr)
}

fn incoming_plan_at(frame: u64) -> VariantReaderPlan {
    let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    request_incoming_plan_at(&abr, duration_for_frames(Consts::SAMPLE_RATE, frame))
}

fn abandoned_incoming_plan_at(frame: u64) -> VariantReaderPlan {
    let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    request_incoming_plan_with_disposition(
        &abr,
        duration_for_frames(Consts::SAMPLE_RATE, frame),
        OutgoingDisposition::Abandoned,
    )
}

fn successive_incoming_plans() -> (VariantReaderPlan, VariantReaderPlan) {
    let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    let first = request_incoming_plan(&abr);
    assert!(
        abr.abort_pending(first.transition().id().abr_ticket()),
        "first transition fixture must release its exact ABR ticket"
    );
    let second = request_incoming_plan(&abr);
    (first, second)
}

fn request_incoming_plan(abr: &AbrState) -> VariantReaderPlan {
    request_incoming_plan_at(abr, Duration::ZERO)
}

fn request_incoming_plan_at(abr: &AbrState, landing: Duration) -> VariantReaderPlan {
    request_incoming_plan_with_disposition(abr, landing, OutgoingDisposition::Retained)
}

fn request_incoming_plan_with_disposition(
    abr: &AbrState,
    landing: Duration,
    disposition: OutgoingDisposition,
) -> VariantReaderPlan {
    abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
    let claim = abr
        .claim_pending_decision(VariantIndex::new(0))
        .expect("exact transition fixture requires a pending ABR claim");
    let transition = VariantTransition::new(
        VariantTransitionId::new(claim.ticket(), 0),
        VariantIndex::new(0),
        VariantIndex::new(1),
    )
    .with_outgoing_disposition(disposition);
    VariantReaderPlan::new(transition, media_info(1), landing)
}

fn assert_route_signal(chunk: &PcmChunk, expected_offset: u64) {
    assert_eq!(chunk.meta.frame_offset, expected_offset);
    for (frame, samples) in chunk.samples.chunks_exact(2).enumerate() {
        let absolute = expected_offset.saturating_add(u64::try_from(frame).unwrap_or(u64::MAX));
        let absolute_f64 = num_traits::cast::ToPrimitive::to_f64(&absolute).unwrap_or(f64::MAX);
        let time = absolute_f64 / f64::from(Consts::SAMPLE_RATE);
        let expected = (time * 440.0 * std::f64::consts::TAU).sin() * 0.25;
        let expected = num_traits::cast::ToPrimitive::to_f32(&expected).unwrap_or(0.0);
        assert_eq!(samples[0].to_bits(), expected.to_bits());
        assert_eq!(samples[1].to_bits(), expected.to_bits());
    }
}

// Waits for the build, not for a number of yields: on a real clock the build
// takes milliseconds while sixty-four yields take microseconds.
#[kithara::hang_watchdog]
async fn wait_for_incoming_priming(fixture: &mut RouteFixture, transition: VariantTransition) {
    loop {
        yield_now().await;
        fixture.source.flush_deferred();
        if fixture.source.decode.incoming_is_priming(transition) {
            return;
        }
        hang_tick!();
    }
}

#[kithara::hang_watchdog]
async fn wait_for_abandoned_incoming_progress(
    fixture: &mut RouteFixture,
    transition: VariantTransition,
) {
    loop {
        yield_now().await;
        fixture.source.flush_deferred();
        if fixture.control.promote_calls() != 0
            || fixture.source.decode.incoming_is_priming(transition)
        {
            return;
        }
        hang_tick!();
    }
}

#[kithara::test(tokio)]
async fn eof_transition_retires_staged_incoming_and_aborts_variant() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let plan = incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();

    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, transition).await;

    assert!(fixture.source.decode.incoming_is_priming(transition));
    assert_eq!(fixture.control.aborted_transition(), None);
    assert!(fixture.drops.lock().is_empty());

    fixture.source.update_state(Track::<AtEof>::new(()).erase());
    assert!(matches!(fixture.source.state, CurrentFsm::AtEof(_)));
    fixture.source.flush_deferred();

    assert!(fixture.source.decode.incoming_transition().is_none());
    assert_eq!(fixture.control.aborted_transition(), Some(transition));
    assert_eq!(fixture.drops.lock().as_slice(), &[99]);
}

#[kithara::test(tokio)]
async fn failed_source_removal_retires_staged_incoming_and_aborts_variant() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let plan = incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();

    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, transition).await;

    assert!(fixture.source.decode.incoming_is_priming(transition));
    assert_eq!(fixture.control.aborted_transition(), None);
    assert!(fixture.drops.lock().is_empty());

    fixture
        .source
        .update_state(Track::<Failed>::new(TrackFailure::SourceCancelled).erase());
    let control = fixture.control.clone();
    let drops = fixture.drops.clone();
    drop(fixture.source);

    assert_eq!(control.aborted_transition(), Some(transition));
    assert_eq!(drops.lock().as_slice(), &[99, 1]);
}

#[kithara::test(tokio)]
async fn exact_incoming_reader_pending_keeps_outgoing_pcm_running() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    fixture.control.set_exact_plan(incoming_plan());

    fixture.source.flush_deferred();

    let TrackStep::Produced(fetch) = fixture.source.step_track() else {
        panic!("outgoing must keep producing while the incoming reader is preparing");
    };
    let chunk = produced_data(fetch);
    assert!(!chunk.samples.is_empty());
    assert_eq!(chunk.meta.frame_offset, 0);
    assert_eq!(fixture.control.plan_calls(), 1);
    assert_eq!(fixture.control.prepare_calls(), 1);
    assert_eq!(fixture.control.take_calls(), 1);
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(0)
    );
    assert!(matches!(fixture.source.state, CurrentFsm::Decoding(_)));
}

#[kithara::test(tokio)]
async fn exact_incoming_build_pending_keeps_outgoing_pcm_running() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let plan = incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();

    fixture.source.flush_deferred();

    assert!(fixture.source.decode.incoming_is_building(transition));
    let TrackStep::Produced(fetch) = fixture.source.step_track() else {
        panic!("outgoing must keep producing while the incoming decoder is building");
    };
    let chunk = produced_data(fetch);
    assert!(!chunk.samples.is_empty());
    assert_eq!(chunk.meta.frame_offset, 0);
    assert_eq!(fixture.control.plan_calls(), 1);
    assert_eq!(fixture.control.prepare_calls(), 1);
    assert_eq!(fixture.control.take_calls(), 1);
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(0)
    );
    assert!(matches!(fixture.source.state, CurrentFsm::Decoding(_)));
}

#[kithara::test(tokio)]
async fn raw_decode_head_ignores_pcm_held_back_by_gapless_trimming() {
    let mut fixture = route_signal_source_with_gapless(
        Consts::SAMPLE_RATE,
        GaplessInfo::new(
            0,
            u64::try_from(Consts::ROUTE_CHUNK_FRAMES.saturating_mul(2)).unwrap_or(u64::MAX),
        ),
    )
    .await;

    let TrackStep::Produced(fetch) = fixture.source.step_track() else {
        panic!("gapless trimming must eventually release its first raw output chunk");
    };
    let output = produced_data(fetch);
    let epoch = fixture.source.seek_obs.epoch();
    let raw_head = fixture
        .source
        .resume
        .decode_head(epoch)
        .expect("released post-gapless PCM must advance the resume cursor");
    let emitted_end = output
        .meta
        .frame_offset
        .saturating_add(u64::from(output.meta.frames));

    assert_eq!(raw_head, (emitted_end, Consts::SAMPLE_RATE));
}

#[kithara::test(tokio)]
async fn incoming_completion_never_replaces_active_before_staged_pcm() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let plan = incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();

    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, transition).await;

    assert_eq!(fixture.control.promote_calls(), 0);
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(0)
    );
    let TrackStep::Produced(fetch) = fixture.source.step_track() else {
        panic!("outgoing must remain authoritative while incoming PCM is staged");
    };
    let chunk = produced_data(fetch);
    assert_eq!(chunk.meta.frame_offset, 0);
    assert_eq!(fixture.control.promote_calls(), 0);
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(0)
    );
}

#[kithara::test(tokio)]
async fn same_spec_priming_retains_the_full_join_after_the_emitted_frontier() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let plan = incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();

    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, transition).await;

    let TrackStep::Produced(fetch) = fixture.source.step_track() else {
        panic!("outgoing must emit while the same-spec incoming generation is priming");
    };
    let chunk = produced_data(fetch);
    let frontier = chunk
        .meta
        .frame_offset
        .saturating_add(u64::from(chunk.meta.frames));
    let join_frames = u64::try_from(frames_for_duration(
        Consts::SAMPLE_RATE,
        Duration::from_millis(20),
    ))
    .unwrap_or(u64::MAX);
    let (retained_first, retained_end, retained_spec) = fixture
        .source
        .decode
        .active()
        .staged_span()
        .expect("same-spec priming must retain real outgoing PCM");

    assert_eq!(retained_spec.sample_rate.get(), Consts::SAMPLE_RATE);
    assert!(retained_first <= frontier);
    assert!(
        retained_end >= frontier.saturating_add(join_frames),
        "retained outgoing span {retained_first}..{retained_end} does not cover the full join after \
         frontier {frontier}"
    );
}

#[kithara::test(tokio)]
async fn exact_primed_generation_promotes_once_at_outgoing_frontier() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let plan = incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();
    fixture.control.set_promotion(VariantPromotion::Promoted);

    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, transition).await;

    let TrackStep::Produced(outgoing) = fixture.source.step_track() else {
        panic!("outgoing must remain authoritative while incoming PCM is first staged");
    };
    assert_eq!(produced_data(outgoing).meta.frame_offset, 0);
    assert_eq!(fixture.control.promote_calls(), 0);

    fixture.source.flush_deferred();

    assert_eq!(fixture.control.promote_calls(), 1);
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(1)
    );
    let chunk_frames = u64::try_from(Consts::ROUTE_CHUNK_FRAMES).unwrap_or(u64::MAX);
    let mut expected_offset = chunk_frames;
    let mut post_cut_frames = 0usize;
    while post_cut_frames < 1_024 {
        let TrackStep::Produced(incoming) = fixture.source.step_track() else {
            panic!("promoted incoming must keep emitting its staged PCM");
        };
        let incoming = produced_data(incoming);
        assert_route_signal(&incoming, expected_offset);
        expected_offset = expected_offset.saturating_add(u64::from(incoming.meta.frames));
        post_cut_frames = post_cut_frames.saturating_add(incoming.frames());
        fixture.source.flush_deferred();
        assert_eq!(fixture.control.promote_calls(), 1);
    }
    assert!(post_cut_frames >= 1_024);
}

#[kithara::test(tokio)]
async fn retained_reader_plan_keeps_promotion_cut_open_before_decoder_build() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let TrackStep::Produced(first_outgoing) = fixture.source.step_track() else {
        panic!("the active decoder must establish an exact production frontier");
    };
    assert_eq!(produced_data(first_outgoing).meta.frame_offset, 0);
    let cut = u64::try_from(Consts::ROUTE_CHUNK_FRAMES).unwrap_or(u64::MAX);
    let plan = incoming_plan_at(cut);
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);

    fixture.source.flush_deferred();

    assert!(fixture.source.decode.incoming_is_preparing(transition));
    assert_eq!(
        fixture.control.landing(),
        Some(duration_for_frames(Consts::SAMPLE_RATE, cut))
    );
    assert_eq!(
        fixture.source.decode.incoming_frontier(),
        Some(OutgoingFrontier::Awaiting)
    );
    let TrackStep::Produced(next_outgoing) = fixture.source.step_track() else {
        panic!("the active decoder must keep producing until the incoming cut is latched");
    };
    assert_eq!(produced_data(next_outgoing).meta.frame_offset, cut);
    assert_eq!(
        fixture.source.resume.decode_head(0),
        Some((cut.saturating_mul(2), Consts::SAMPLE_RATE))
    );
    assert!(fixture.source.decode.active().staged_span().is_none());
}

#[kithara::test(tokio)]
async fn finite_incoming_latches_cut_while_outgoing_fills_the_join_tail() {
    const INCOMING_CHUNKS: usize = 5;

    let mut fixture =
        route_signal_source_with_finite_incoming(Consts::SAMPLE_RATE, INCOMING_CHUNKS).await;
    let TrackStep::Produced(first_outgoing) = fixture.source.step_track() else {
        panic!("the active decoder must establish an exact production frontier");
    };
    assert_eq!(produced_data(first_outgoing).meta.frame_offset, 0);
    assert!(fixture.source.decode.active().staged_span().is_none());
    let cut = u64::try_from(Consts::ROUTE_CHUNK_FRAMES).unwrap_or(u64::MAX);
    let incoming_first = u64::try_from(frames_for_duration(
        Consts::SAMPLE_RATE,
        duration_for_frames(Consts::SAMPLE_RATE, cut),
    ))
    .unwrap_or(u64::MAX);
    assert_eq!(incoming_first, 255);
    assert!(incoming_first < cut);
    let plan = incoming_plan_at(cut);
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();
    fixture.control.set_promotion(VariantPromotion::Promoted);

    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, transition).await;
    fixture.source.flush_deferred();

    let staged = fixture
        .source
        .decode
        .incoming_staged_span()
        .expect("four incoming chunks must cover the join at the exact cut");
    assert_eq!(staged.0, cut);
    assert_eq!(staged.1, incoming_first.saturating_add(1_024));
    assert!(fixture.source.decode.incoming_is_priming(transition));
    assert_eq!(fixture.control.promote_calls(), 0);
    assert_eq!(fixture.control.aborted_transition(), None);

    fixture.source.flush_deferred();

    assert_eq!(fixture.source.decode.incoming_staged_span(), Some(staged));
    assert!(fixture.source.decode.incoming_is_priming(transition));
    assert!(fixture.source.decode.active().staged_span().is_none());
    assert_eq!(fixture.control.promote_calls(), 0);
    assert_eq!(fixture.control.aborted_transition(), None);

    let TrackStep::Blocked(_) = fixture.source.step_track() else {
        panic!("outgoing publication must stop at the latched cut while its join tail fills");
    };
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .staged_span()
            .map(|span| (span.0, span.1)),
        Some((256, 1_280))
    );
    assert_eq!(
        fixture.source.resume.decode_head(0),
        Some((cut, Consts::SAMPLE_RATE))
    );

    fixture.source.flush_deferred();

    assert_eq!(fixture.control.promote_calls(), 1);
    assert_eq!(fixture.control.aborted_transition(), None);
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(1)
    );
}

#[kithara::test(tokio)]
async fn abandoned_incoming_hard_cuts_without_outgoing_join_pcm() {
    const INCOMING_CHUNKS: usize = 5;

    let mut fixture =
        route_signal_source_with_finite_incoming(Consts::SAMPLE_RATE, INCOMING_CHUNKS).await;
    let TrackStep::Produced(first_outgoing) = fixture.source.step_track() else {
        panic!("the active decoder must establish an exact production frontier");
    };
    assert_eq!(produced_data(first_outgoing).meta.frame_offset, 0);
    assert!(fixture.source.decode.active().staged_span().is_none());
    let cut = u64::try_from(Consts::ROUTE_CHUNK_FRAMES).unwrap_or(u64::MAX);
    let landing = duration_for_frames(Consts::SAMPLE_RATE, cut);
    let plan = abandoned_incoming_plan_at(cut);
    let transition = plan.transition();
    assert_eq!(
        transition.outgoing_disposition(),
        OutgoingDisposition::Abandoned
    );
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();
    fixture.control.set_promotion(VariantPromotion::Promoted);

    fixture.source.flush_deferred();
    wait_for_abandoned_incoming_progress(&mut fixture, transition).await;

    assert_eq!(fixture.control.landing(), Some(landing));
    fixture.source.flush_deferred();

    assert_eq!(fixture.control.promote_calls(), 1);
    assert_eq!(fixture.control.aborted_transition(), None);
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(1)
    );
    let TrackStep::Produced(incoming) = fixture.source.step_track() else {
        panic!("the hard-cut incoming must continue from its staged exact cut");
    };
    let incoming = produced_data(incoming);
    assert_route_signal(&incoming, cut);
    fixture.source.flush_deferred();
    assert_eq!(fixture.control.promote_calls(), 1);
}

#[kithara::test(tokio)]
async fn promotion_preserves_the_normalized_timeline_gap() {
    const ACTIVE_GAP: u64 = 17;
    const INCOMING_GAP: u64 = 3;

    let mut fixture = route_signal_source_with_gaps(ACTIVE_GAP, INCOMING_GAP).await;
    let plan = incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();
    fixture.control.set_promotion(VariantPromotion::Promoted);

    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, transition).await;
    assert!(matches!(
        fixture.source.step_track(),
        TrackStep::Produced(_)
    ));
    fixture.source.flush_deferred();

    assert_eq!(
        fixture.source.decode.active().timeline_gap(),
        ACTIVE_GAP,
        "the promoted generation must retain the splice proof's shared timeline origin"
    );
}

/// The incoming lands *on* the outgoing decode frontier, never ahead of it.
///
/// A promotion proof is only minted once the outgoing walks into the incoming's
/// staged span, and the outgoing stops decoding the moment its PCM ring is
/// full: a landing placed ahead of the frontier waits on motion nobody owes it,
/// and the switch never commits. The frontier advances one whole chunk at a
/// time here, so a landing that is not a whole number of chunks is a landing
/// that was pushed past it.
#[kithara::test(tokio)]
async fn incoming_lands_on_the_outgoing_frontier_not_ahead_of_it() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let plan = incoming_plan();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();

    // A frontier has to exist before it can be landed against: until the
    // outgoing decodes, the source plans with no landing at all.
    let mut landing = None;
    for _ in 0..64 {
        yield_now().await;
        fixture.source.flush_deferred();
        landing = fixture.control.landing();
        if landing.is_some() {
            break;
        }
        if !matches!(fixture.source.step_track(), TrackStep::Produced(_)) {
            break;
        }
    }

    let landing = landing.expect("an exact transition plans against the outgoing frontier");
    let frames = num_traits::cast::<f64, u64>(
        (landing.as_secs_f64() * f64::from(Consts::SAMPLE_RATE)).round(),
    )
    .unwrap_or(u64::MAX);
    let chunk = u64::try_from(Consts::ROUTE_CHUNK_FRAMES).unwrap_or(u64::MAX);
    assert_eq!(
        frames % chunk,
        0,
        "landing {landing:?} is {frames} frames — not a whole chunk, so it sits past \
         the frontier the promotion proof is stated against"
    );
}

#[kithara::test(tokio)]
async fn locked_promotion_keeps_primed_incoming_and_outgoing_authoritative() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let plan = incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();
    fixture.control.set_promotion(VariantPromotion::Deferred);

    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, transition).await;

    let TrackStep::Produced(first_outgoing) = fixture.source.step_track() else {
        panic!("outgoing must remain audible while incoming PCM is first staged");
    };
    assert_eq!(produced_data(first_outgoing).meta.frame_offset, 0);

    fixture.source.flush_deferred();

    assert_eq!(fixture.control.promote_calls(), 1);
    assert!(fixture.source.decode.incoming_is_priming(transition));
    let incoming_first = fixture
        .source
        .decode
        .incoming_staged_span()
        .map(|(first, _, _)| first);
    assert_eq!(
        incoming_first,
        Some(u64::try_from(Consts::ROUTE_CHUNK_FRAMES).unwrap_or(u64::MAX)),
        "deferred publication must restore the owned, cut-trimmed incoming generation"
    );
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(0)
    );
    let TrackStep::Produced(next_outgoing) = fixture.source.step_track() else {
        panic!("publication lock must not interrupt outgoing PCM");
    };
    assert_eq!(
        produced_data(next_outgoing).meta.frame_offset,
        u64::try_from(Consts::ROUTE_CHUNK_FRAMES).unwrap_or(u64::MAX)
    );
    fixture.source.flush_deferred();
    assert_eq!(fixture.control.promote_calls(), 2);
    let incoming_first = fixture
        .source
        .decode
        .incoming_staged_span()
        .map(|(first, _, _)| first);
    assert_eq!(
        incoming_first,
        Some(u64::try_from(Consts::ROUTE_CHUNK_FRAMES.saturating_mul(2)).unwrap_or(u64::MAX))
    );

    fixture.control.set_promotion(VariantPromotion::Promoted);
    fixture.source.flush_deferred();

    assert_eq!(fixture.control.promote_calls(), 3);
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(1)
    );
    let expected_offset =
        u64::try_from(Consts::ROUTE_CHUNK_FRAMES.saturating_mul(2)).unwrap_or(u64::MAX);
    let TrackStep::Produced(incoming) = fixture.source.step_track() else {
        panic!("the restored incoming generation must produce after publication unlock");
    };
    assert_route_signal(&produced_data(incoming), expected_offset);
    fixture.source.flush_deferred();
    assert_eq!(fixture.control.promote_calls(), 3);
}

#[kithara::test(tokio)]
async fn stale_prepared_promotion_returns_incoming_for_shell_retirement() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let plan = incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();
    fixture.control.set_promotion(VariantPromotion::Stale);

    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, transition).await;
    let TrackStep::Produced(outgoing) = fixture.source.step_track() else {
        panic!("outgoing must name the cut before stale publication");
    };
    assert_eq!(produced_data(outgoing).meta.frame_offset, 0);
    fixture.source.flush_deferred();

    assert_eq!(fixture.control.promote_calls(), 1);
    assert!(fixture.source.decode.incoming_transition().is_none());
    assert_eq!(fixture.drops.lock().as_slice(), &[99]);
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(0)
    );
}

#[kithara::test(tokio)]
async fn gapless_eof_flushes_once_and_drains_every_frame_across_repeated_ticks() {
    const RAW_CHUNKS: usize = 4;
    const TRAILING_FRAMES: u64 = 300;

    let mut fixture = route_signal_source_with_gapless_eof(
        Consts::SAMPLE_RATE,
        GaplessInfo::new(0, TRAILING_FRAMES),
        RAW_CHUNKS,
    )
    .await;
    let plan = incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();
    fixture.control.set_promotion(VariantPromotion::Deferred);
    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, transition).await;

    let expected = RAW_CHUNKS
        .saturating_mul(Consts::ROUTE_CHUNK_FRAMES)
        .saturating_sub(usize::try_from(TRAILING_FRAMES).unwrap_or(usize::MAX));
    let mut frames = 0usize;
    let mut next_offset = 0u64;
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
            TrackStep::Failed => panic!("finite gapless fixture must reach EOF cleanly"),
        }
        fixture.source.flush_deferred();
    }

    assert_eq!(
        frames, expected,
        "EOF must release every non-trimmed frame once"
    );
    assert_eq!(next_offset, u64::try_from(expected).unwrap_or(u64::MAX));
}

#[kithara::test(tokio)]
async fn newer_ticket_supersedes_only_incoming_generation() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let (first, second) = successive_incoming_plans();
    let first_transition = first.transition();
    let second_transition = second.transition();
    fixture.control.set_exact_plan(first);
    fixture.control.set_exact_reader_ready();

    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, first_transition).await;
    let TrackStep::Produced(outgoing) = fixture.source.step_track() else {
        panic!("outgoing must remain audible while the first incoming generation is staged");
    };
    assert_eq!(produced_data(outgoing).meta.frame_offset, 0);

    fixture.control.set_exact_plan(second);
    fixture.control.set_exact_reader_ready();
    fixture.source.flush_deferred();

    assert!(
        fixture
            .source
            .decode
            .incoming_is_building(second_transition)
    );
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(0)
    );
    assert_eq!(fixture.drops.lock().as_slice(), &[99]);
}

#[kithara::test(tokio)]
async fn stale_incoming_completion_retires_generation_in_shell() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let (first, second) = successive_incoming_plans();
    let first_transition = first.transition();
    let second_transition = second.transition();
    let first_build = BuildId::fixture(41);

    assert!(
        fixture
            .source
            .decode
            .begin_incoming(first_transition, OutgoingFrontier::Awaiting)
            .is_none()
    );
    assert!(
        fixture
            .source
            .decode
            .mark_incoming_building(first_transition, first_build)
    );
    assert!(
        fixture
            .source
            .decode
            .begin_incoming(second_transition, OutgoingFrontier::Awaiting)
            .is_none()
    );

    let pushed = fixture
        .source
        .rebuild
        .completion()
        .push(DecoderBuildComplete {
            build: first_build,
            purpose: DecoderBuildPurpose::Incoming(first_transition),
            result: Ok(DecoderGeneration::new(
                Box::new(TestDecoder::new(77, fixture.drops.clone())),
                Some(media_info(1)),
                0,
                0,
                None,
                GaplessMode::Disabled,
            )),
        });
    assert!(pushed.is_ok());
    assert!(fixture.drops.lock().is_empty());

    fixture.source.flush_deferred();

    assert_eq!(fixture.drops.lock().as_slice(), &[77]);
    assert_eq!(
        fixture
            .source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(0)
    );
}

#[kithara::test(tokio)]
async fn incoming_media_facts_choose_reader_profile() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    fixture.control.enable_byte_map();
    let template = incoming_plan();
    let mut incoming_media = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Mp3))
        .maybe_container(Some(ContainerFormat::MpegAudio))
        .build();
    incoming_media.variant_index = Some(1);
    let plan = VariantReaderPlan::new(
        template.transition(),
        incoming_media.clone(),
        template.landing_time(),
    );
    fixture.control.set_exact_plan(plan);

    fixture.source.flush_deferred();

    let byte_map = fixture.source.shared_stream.byte_map();
    let expected = DecoderBuilder::reader_profile(&incoming_media, byte_map.as_deref());
    let active = DecoderBuilder::reader_profile(&media_info(0), byte_map.as_deref());
    assert_ne!(expected, active);
    assert_eq!(fixture.control.prepared_profile(), Some(expected));
}

#[kithara::test(tokio)]
async fn exact_promotion_emits_variant_switch_decoder_event() {
    let mut fixture = route_signal_source(Consts::SAMPLE_RATE).await;
    let bus = EventBus::new(16);
    let mut events = bus.subscribe();
    fixture.source = fixture
        .source
        .with_emit(Arc::new(DeferredBus::new(bus, 16)));
    let plan = incoming_plan();
    let transition = plan.transition();
    fixture.control.set_exact_plan(plan);
    fixture.control.set_exact_reader_ready();
    fixture.control.set_promotion(VariantPromotion::Promoted);

    fixture.source.flush_deferred();
    wait_for_incoming_priming(&mut fixture, transition).await;
    assert!(matches!(
        fixture.source.step_track(),
        TrackStep::Produced(_)
    ));
    fixture.source.flush_deferred();

    let mut changed = false;
    while let Ok(envelope) = events.try_recv() {
        changed |= matches!(
            envelope.event,
            Event::Decoder(DecoderEvent::DecoderChanged {
                cause: DecoderChangeCause::VariantSwitch,
                variant: Some(1),
                ..
            })
        );
    }
    assert!(changed);
}
