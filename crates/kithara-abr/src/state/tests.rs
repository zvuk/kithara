use std::{
    future::{Future, poll_fn},
    pin::pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
};

use kithara_events::{DEFAULT_EVENT_BUS_CAPACITY, Envelope, EventBus};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Notify},
    time::{Duration, Duration as StdDuration, Instant},
};
use kithara_test_utils::kithara;
use proptest::prelude::*;
use unimock::{MockFn, Unimock, matching};

use super::{AbrDecision, AbrState, AbrView, PendingAbrClaim, PendingAbrDecision};
use crate::{
    Abr, AbrController, AbrEvent, AbrMock, AbrMode, AbrProgressSnapshot, AbrReason, AbrSettings,
    BandwidthSource, Estimator, ThroughputEstimator, VariantDuration, VariantIndex, VariantInfo,
};

/// Peer that answers with the state and variants it is given and keeps the
/// default `Abr` behaviour everywhere else. Every test in this module
/// reaches all three methods; unimock rejects a clause that none does.
fn abr_peer(state: &Arc<AbrState>, variants: Vec<VariantInfo>) -> Arc<dyn Abr> {
    Arc::new(Unimock::new((
        AbrMock::cancel
            .each_call(matching!())
            .returns(CancelToken::never()),
        AbrMock::state
            .each_call(matching!())
            .returns(Some(Arc::clone(state))),
        AbrMock::variants.each_call(matching!()).returns(variants),
    )))
}

/// Canonical 3-variant fixture used by every test in this module. Private
/// to the test module so it never leaks into the public API.
fn test_variants_3() -> Vec<VariantInfo> {
    vec![
        VariantInfo {
            variant_index: VariantIndex::new(0),
            bandwidth_bps: Some(256_000),
            duration: VariantDuration::Unknown,
            name: None,
            codecs: None,
            container: None,
        },
        VariantInfo {
            variant_index: VariantIndex::new(1),
            bandwidth_bps: Some(512_000),
            duration: VariantDuration::Unknown,
            name: None,
            codecs: None,
            container: None,
        },
        VariantInfo {
            variant_index: VariantIndex::new(2),
            bandwidth_bps: Some(1_024_000),
            duration: VariantDuration::Unknown,
            name: None,
            codecs: None,
            container: None,
        },
    ]
}

fn settings_fast() -> AbrSettings {
    AbrSettings::builder()
        .min_switch_interval(Duration::ZERO)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build()
}

fn view_with_bw<'a>(
    bps: Option<u64>,
    variants: &'a [VariantInfo],
    settings: &'a AbrSettings,
) -> AbrView<'a> {
    AbrView {
        variants,
        settings,
        estimate_bps: bps,
        buffer_ahead: None,
        bytes_downloaded: 10 * 1024 * 1024,
    }
}

#[kithara::test]
fn decide_locked_never_switches() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.lock();
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(Some(10_000_000), &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert!(!d.changed());
    assert_eq!(d.reason(), AbrReason::Locked);
}

#[kithara::test]
fn decide_many_samples_during_lock_never_switches() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    let initial = state.current_variant_index();
    state.lock();
    let variants = test_variants_3();
    let settings = settings_fast();
    for i in 0..100u64 {
        let view = view_with_bw(Some(10_000_000 * (i + 1)), &variants, &settings);
        let _ = state.decide(&view, Instant::now() + Duration::from_secs(i * 60));
    }
    assert_eq!(state.current_variant_index(), initial);
}

#[kithara::test]
fn decide_manual_mode_always_target() {
    let state = AbrState::new(AbrMode::Manual(VariantIndex::new(2)));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(None, &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason(), AbrReason::ManualOverride);
    assert_eq!(d.target(), VariantIndex::new(2));
}

#[kithara::test]
fn a_manual_pick_taken_while_a_seek_holds_the_lock_still_decides() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.lock();
    state.set_mode(AbrMode::Manual(VariantIndex::new(1)));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(Some(10_000_000), &variants, &settings);

    let d = state.decide(&view, Instant::now());

    assert!(d.changed());
    assert_eq!(d.reason(), AbrReason::ManualOverride);
    assert_eq!(d.target(), VariantIndex::new(1));
}

#[kithara::test]
fn decide_no_estimate_stays_put() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(1))));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(None, &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason(), AbrReason::NoEstimate);
    assert!(!d.changed());
}

#[kithara::test]
fn decide_upswitch_when_bandwidth_allows() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(Some(3_000_000), &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason(), AbrReason::UpSwitch);
    assert_eq!(d.target(), VariantIndex::new(2));
}

#[kithara::test]
fn decide_downswitch_when_bandwidth_drops() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(2))));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(Some(300_000), &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason(), AbrReason::DownSwitch);
    assert_eq!(d.target(), VariantIndex::new(0));
}

#[kithara::test]
fn decide_urgent_downswitch_when_buffer_low() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(2))));
    let variants = test_variants_3();
    let settings = AbrSettings::builder()
        .urgent_downswitch_buffer(Duration::from_secs(5))
        .down_hysteresis_ratio(0.01)
        .min_switch_interval(Duration::ZERO)
        .build();
    let view = AbrView {
        estimate_bps: Some(700_000),
        buffer_ahead: Some(Duration::from_secs(2)),
        bytes_downloaded: 10 * 1024 * 1024,
        variants: &variants,
        settings: &settings,
    };
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason(), AbrReason::UrgentDownSwitch);
}

#[kithara::test]
fn decide_buffer_too_low_for_upswitch() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    let variants = test_variants_3();
    let settings = AbrSettings::builder()
        .min_buffer_for_up_switch(Duration::from_secs(10))
        .min_switch_interval(Duration::ZERO)
        .build();
    let view = AbrView {
        estimate_bps: Some(3_000_000),
        buffer_ahead: Some(Duration::from_secs(2)),
        bytes_downloaded: 10 * 1024 * 1024,
        variants: &variants,
        settings: &settings,
    };
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason(), AbrReason::BufferTooLowForUpSwitch);
    assert!(!d.changed());
}

#[kithara::test]
fn apply_updates_current_variant_and_timestamp() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.apply_decision(
        &AbrDecision::UpSwitch {
            from: VariantIndex::new(0),
            to: VariantIndex::new(2),
            reason: AbrReason::UpSwitch,
        },
        Instant::now(),
    );
    assert_eq!(state.current_variant_index(), VariantIndex::new(2));
}

#[kithara::test]
fn apply_decision_stay_is_noop() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(1))));
    state.apply_decision(
        &AbrDecision::Stay {
            current: VariantIndex::new(1),
            reason: AbrReason::AlreadyOptimal,
        },
        Instant::now(),
    );
    assert_eq!(state.current_variant_index(), VariantIndex::new(1));
}

#[kithara::test]
fn lock_is_refcounted() {
    let state = AbrState::new(AbrMode::Auto(None));
    state.lock();
    state.lock();
    assert!(state.is_locked());
    state.unlock();
    assert!(state.is_locked());
    state.unlock();
    assert!(!state.is_locked());
}

#[kithara::test]
fn pending_target_is_empty_on_fresh_state() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    assert_eq!(state.pending_target(), None);
}

#[kithara::test]
fn request_target_records_intent_without_committing() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    assert_eq!(state.pending_target(), Some(VariantIndex::new(2)));
    assert_eq!(
        state.current_variant_index(),
        VariantIndex::new(0),
        "request_target must not move current_variant; commit_pending owns that step"
    );
}

#[kithara::test]
fn request_target_replace_pending_latest_wins() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(1), AbrReason::UpSwitch);
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    assert_eq!(
        state.pending_target(),
        Some(VariantIndex::new(2)),
        "second request_target must replace the first (latest-wins semantics)"
    );
}

#[kithara::test]
fn request_target_repeat_same_target_keeps_the_ticket() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    let first = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("first request must be claimable");

    state.request_target(VariantIndex::new(2), AbrReason::UrgentDownSwitch);

    assert_eq!(
        state
            .claim_pending_decision(VariantIndex::new(0))
            .map(PendingAbrDecision::ticket),
        Some(first.ticket()),
        "a repeat request for the queued target must not re-mint the ticket \
         the consumer built its incoming session against"
    );
    assert!(state.commit_pending(first, Instant::now()));
    assert_eq!(state.current_variant_index(), VariantIndex::new(2));
}

#[kithara::test]
fn stale_claim_cannot_commit_after_same_target_aba() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    let stale = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("first request must be claimable");

    state.request_target(VariantIndex::new(1), AbrReason::DownSwitch);
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    let current = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("replacement request must be claimable");

    assert_ne!(stale.ticket(), current.ticket());
    assert_eq!(stale.decision(), current.decision());
    assert!(!state.commit_pending(stale, Instant::now()));
    assert_eq!(state.current_variant_index(), VariantIndex::new(0));
    assert_eq!(
        state
            .claim_pending_decision(VariantIndex::new(0))
            .map(PendingAbrDecision::ticket),
        Some(current.ticket()),
        "stale commit must preserve the replacement request"
    );
    assert!(state.commit_pending(current, Instant::now()));
    assert_eq!(state.current_variant_index(), VariantIndex::new(2));
    assert_eq!(state.pending_target(), None);
}

#[kithara::test]
fn claim_from_old_current_cannot_commit_after_variant_changes() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    let stale = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("request must be claimable from the initial variant");

    state.apply_decision(
        &AbrDecision::UpSwitch {
            from: VariantIndex::new(0),
            to: VariantIndex::new(1),
            reason: AbrReason::UpSwitch,
        },
        Instant::now(),
    );

    assert!(!state.commit_pending(stale, Instant::now()));
    assert_eq!(state.current_variant_index(), VariantIndex::new(1));
    assert_eq!(state.pending_target(), Some(VariantIndex::new(2)));

    let current = state
        .claim_pending_decision(VariantIndex::new(1))
        .expect("pending request must be reclaimable from the live variant");
    assert!(state.commit_pending(current, Instant::now()));
    assert_eq!(state.current_variant_index(), VariantIndex::new(2));
}

#[kithara::test]
fn claim_before_lock_cannot_commit_until_reclaimed_after_unlock() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    let before_lock = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("request must be claimable before lock");

    state.lock();
    assert!(!state.commit_pending(before_lock, Instant::now()));
    assert_eq!(state.current_variant_index(), VariantIndex::new(0));
    assert_eq!(state.pending_target(), Some(VariantIndex::new(2)));

    state.unlock();
    let after_unlock = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("pending request must be reclaimable after unlock");
    assert!(state.commit_pending(after_unlock, Instant::now()));
    assert_eq!(state.current_variant_index(), VariantIndex::new(2));
}

#[kithara::test]
fn pending_claim_distinguishes_absent_from_locked() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    assert_eq!(
        state.pending_claim(VariantIndex::new(0)),
        PendingAbrClaim::Absent
    );

    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    assert!(matches!(
        state.pending_claim(VariantIndex::new(0)),
        PendingAbrClaim::Ready(_)
    ));

    state.lock();
    assert!(matches!(
        state.pending_claim(VariantIndex::new(0)),
        PendingAbrClaim::Locked(_)
    ));
    assert_eq!(state.pending_target(), Some(VariantIndex::new(2)));
}

#[kithara::test]
fn a_manual_intent_formed_under_the_lock_is_published_only_after_unlock() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.lock();
    state.set_mode(AbrMode::Manual(VariantIndex::new(1)));
    state.request_target(VariantIndex::new(1), AbrReason::ManualOverride);

    assert!(matches!(
        state.pending_claim(VariantIndex::new(0)),
        PendingAbrClaim::Locked(_)
    ));
    assert_eq!(state.current_variant_index(), VariantIndex::new(0));

    state.unlock();
    let claim = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("the manual intent formed under the lock must outlive it");
    assert!(state.commit_pending(claim, Instant::now()));
    assert_eq!(state.current_variant_index(), VariantIndex::new(1));
}

#[kithara::test]
fn abort_pending_only_clears_matching_ticket() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(1), AbrReason::UpSwitch);
    let stale = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("first request must be claimable");

    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    let current = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("replacement request must be claimable");

    assert!(!state.abort_pending(stale.ticket()));
    assert_eq!(state.pending_target(), Some(VariantIndex::new(2)));
    assert!(state.abort_pending(current.ticket()));
    assert_eq!(state.pending_target(), None);
}

#[kithara::test]
fn retract_throughput_pending_drops_intent_without_moving_variant() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(2))));
    state.request_target(VariantIndex::new(0), AbrReason::DownSwitch);
    state.retract_throughput_pending(VariantIndex::new(2));
    assert_eq!(state.pending_target(), None);
    assert_eq!(
        state.current_variant_index(),
        VariantIndex::new(2),
        "retracting an intent must not move the audible variant"
    );
}

#[kithara::test]
fn retract_throughput_pending_preserves_manual_intent() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::ManualOverride);
    state.retract_throughput_pending(VariantIndex::new(0));
    assert_eq!(
        state.pending_target(),
        Some(VariantIndex::new(2)),
        "a user-driven pending must not be retracted by a throughput verdict"
    );
}

#[kithara::test]
#[case::urgent_down(AbrReason::UrgentDownSwitch)]
#[case::escape_stalled(AbrReason::EscapeStalled)]
fn retract_throughput_pending_preserves_a_rescue(#[case] reason: AbrReason) {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), reason);
    state.retract_throughput_pending(VariantIndex::new(0));
    assert_eq!(
        state.pending_target(),
        Some(VariantIndex::new(2)),
        "a rescue off a variant that stopped delivering must survive a \
         recovered throughput estimate — the estimate recovers as soon as \
         another variant's segment lands, while the stalled one is still stalled"
    );
}

#[kithara::test]
fn peek_pending_decision_returns_none_when_no_request() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    assert!(state.peek_pending_decision(VariantIndex::new(0)).is_none());
    assert_eq!(state.current_variant_index(), VariantIndex::new(0));
}

#[kithara::test]
fn peek_pending_decision_does_not_mutate_current_variant() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    let decision = state
        .peek_pending_decision(VariantIndex::new(0))
        .expect("pending request must produce a decision");
    assert_eq!(decision.target(), VariantIndex::new(2));
    assert_eq!(decision.reason(), AbrReason::UpSwitch);
    assert!(decision.changed());
    assert_eq!(
        state.current_variant_index(),
        VariantIndex::new(0),
        "peek must not mutate"
    );
    assert_eq!(
        state.pending_target(),
        Some(VariantIndex::new(2)),
        "peek must not consume pending"
    );
}

#[kithara::test]
fn apply_decision_publishes_and_clears_matching_pending() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    let decision = state
        .peek_pending_decision(VariantIndex::new(0))
        .expect("pending request must produce a decision");
    state.apply_decision(&decision, Instant::now());
    assert_eq!(state.current_variant_index(), VariantIndex::new(2));
    assert_eq!(
        state.pending_target(),
        None,
        "matching pending must be cleared atomically"
    );
}

#[kithara::test]
fn apply_decision_preserves_pending_overwritten_after_peek() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    let decision = state
        .peek_pending_decision(VariantIndex::new(0))
        .expect("pending request must produce a decision");
    state.request_target(VariantIndex::new(3), AbrReason::DownSwitch);
    state.apply_decision(&decision, Instant::now());
    assert_eq!(
        state.current_variant_index(),
        VariantIndex::new(2),
        "captured decision applies regardless of later pending overwrite"
    );
    assert_eq!(
        state.pending_target(),
        Some(VariantIndex::new(3)),
        "new pending must survive an apply for a different target"
    );
}

#[kithara::test]
fn peek_pending_decision_honors_is_locked() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    state.lock();
    assert!(
        state.peek_pending_decision(VariantIndex::new(0)).is_none(),
        "locked state must not surface pending decisions"
    );
    assert_eq!(state.current_variant_index(), VariantIndex::new(0));
    assert_eq!(
        state.pending_target(),
        Some(VariantIndex::new(2)),
        "deferred decision must remain pending until unlock"
    );
}

#[kithara::test]
fn peek_pending_decision_returns_none_when_target_equals_current() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(1))));
    state.request_target(VariantIndex::new(1), AbrReason::AlreadyOptimal);
    assert!(
        state.peek_pending_decision(VariantIndex::new(1)).is_none(),
        "self-switch (target == current) must not produce a decision"
    );
    assert_eq!(state.current_variant_index(), VariantIndex::new(1));
}

#[kithara::test]
fn apply_decision_after_unlock_applies_still_pending_intent() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.lock();
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
    assert!(state.peek_pending_decision(VariantIndex::new(0)).is_none());
    state.unlock();
    let decision = state
        .peek_pending_decision(VariantIndex::new(0))
        .expect("post-unlock peek must surface the still-pending intent");
    state.apply_decision(&decision, Instant::now());
    assert_eq!(decision.target(), VariantIndex::new(2));
    assert_eq!(state.current_variant_index(), VariantIndex::new(2));
}

/// Ping-pong root (#107): a queued `Manual(w)` boundary switch must die
/// the moment the user re-pins to another variant. Without the supersede
/// rule the slot is only cleared by `apply_decision` or a seek, so a
/// later boundary commits the superseded pin against the user's latest
/// choice.
#[kithara::test]
fn set_mode_clears_superseded_pending() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.set_mode(AbrMode::Manual(VariantIndex::new(2)));
    state.request_target(VariantIndex::new(2), AbrReason::ManualOverride);
    state.set_mode(AbrMode::Manual(VariantIndex::new(0)));
    assert_eq!(
        state.pending_target(),
        None,
        "explicit set_mode must supersede the queued switch intent"
    );
    assert!(
        state
            .peek_pending_decision(state.current_variant_index())
            .is_none(),
        "no boundary commit may surface a superseded pin"
    );
}

/// A user pinning the variant the controller had already queued keeps that
/// queued switch — but it is the user's switch now, and the `VariantApplied`
/// it eventually publishes is the only thing telling the app which it was.
/// Left as the throughput reason, a manual switch reaches the app disguised as
/// an automatic one, and a listener waiting for its own command never hears it.
#[kithara::test]
fn manual_mode_restating_a_queued_target_claims_it_as_manual() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::DownSwitch);
    state.set_mode(AbrMode::Manual(VariantIndex::new(2)));
    assert_eq!(
        state
            .peek_pending_decision(state.current_variant_index())
            .map(|decision| decision.reason()),
        Some(AbrReason::ManualOverride),
        "a manually restated switch must publish as a manual one"
    );
}

/// A listener's pick is a new intent even when the machine happened to want
/// the same variant. The attempt already in flight for it may die for reasons
/// of its own — a discarded session, a rebuilt reader — and every holder of
/// its ticket may say so. Sharing that ticket makes the command and the dead
/// attempt one object, and the command goes down with it, unheard.
#[kithara::test]
fn a_manual_pick_outlives_the_abort_of_the_attempt_it_restated() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::UrgentDownSwitch);
    let in_flight = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("the queued rescue must be claimable");

    state.set_mode(AbrMode::Manual(VariantIndex::new(2)));

    assert!(
        !state.abort_pending(in_flight.ticket()),
        "the aborted attempt must not answer for the manual pick that replaced it"
    );
    assert_eq!(state.pending_target(), Some(VariantIndex::new(2)));
}

/// Re-pinning a manual override the slot already carries is one command said
/// twice. A fresh ticket there would orphan the attempt already running for
/// it and start the same switch over.
#[kithara::test]
fn re_pinning_the_same_manual_override_keeps_its_attempt() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::UrgentDownSwitch);
    state.set_mode(AbrMode::Manual(VariantIndex::new(2)));
    let claimed = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("the manual pick must be claimable");

    state.set_mode(AbrMode::Manual(VariantIndex::new(2)));

    assert!(
        state.abort_pending(claimed.ticket()),
        "one command twice is one attempt, and its ticket still answers for it"
    );
}

/// The restated switch keeps the target the controller queued — claiming it
/// for the user must not re-aim it.
#[kithara::test]
fn manual_mode_restating_a_queued_target_keeps_that_target() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.request_target(VariantIndex::new(2), AbrReason::DownSwitch);
    state.set_mode(AbrMode::Manual(VariantIndex::new(2)));
    assert_eq!(state.pending_target(), Some(VariantIndex::new(2)));
}

/// Same supersede rule when the re-pin lands mid-seek (state locked):
/// the queued intent dies at `set_mode`, not at unlock — otherwise the
/// post-unlock boundary would commit the stale pin.
#[kithara::test]
fn set_mode_under_lock_clears_superseded_pending() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.set_mode(AbrMode::Manual(VariantIndex::new(2)));
    state.request_target(VariantIndex::new(2), AbrReason::ManualOverride);
    state.lock();
    state.set_mode(AbrMode::Manual(VariantIndex::new(0)));
    state.unlock();
    assert_eq!(
        state.pending_target(),
        None,
        "post-unlock boundary must not commit a pin superseded during the lock"
    );
}

/// Race half of the supersede rule: a tick that read the old mode can
/// reach `request_target` after `set_mode` already cleared the slot. A
/// manual pin admits exactly one pending target — its own index — so a
/// conflicting write is refused instead of resurrecting the stale intent
/// (`Stay` arms never clean the slot, so it would stay polluted).
#[kithara::test]
fn request_target_refuses_target_conflicting_with_manual_pin() {
    let state = AbrState::new(AbrMode::Manual(VariantIndex::new(0)));
    state.request_target(VariantIndex::new(2), AbrReason::ManualOverride);
    assert_eq!(
        state.pending_target(),
        None,
        "a request derived from a superseded mode must not enter the slot"
    );
}

#[kithara::test]
fn request_target_accepts_manual_pin_target() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    state.set_mode(AbrMode::Manual(VariantIndex::new(2)));
    state.request_target(VariantIndex::new(2), AbrReason::ManualOverride);
    assert_eq!(
        state.pending_target(),
        Some(VariantIndex::new(2)),
        "the pinned target itself must queue normally"
    );
}

/// Wall-clock interval measured against the instant `AbrState` captured at
/// construction: both must come from the same clock, so this one opts out
#[kithara::test(flash(false))]
fn min_switch_interval_prevents_oscillation() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    let variants = test_variants_3();
    let settings = AbrSettings::builder()
        .min_switch_interval(Duration::from_secs(30))
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let now = Instant::now();
    let view = AbrView {
        estimate_bps: Some(3_000_000),
        buffer_ahead: None,
        bytes_downloaded: 10 * 1024 * 1024,
        variants: &variants,
        settings: &settings,
    };
    // The interval also guards the first switch of a session, so let it elapse
    // before the pair of decisions this test is about.
    let settled = now + Duration::from_secs(30);
    let d1 = state.decide(&view, settled);
    assert!(d1.changed());
    state.apply_decision(&d1, settled);

    // Evidence that would immediately send the variant back down — this is the
    // oscillation the interval exists to damp.
    let reversal = AbrView {
        estimate_bps: Some(1),
        buffer_ahead: None,
        bytes_downloaded: 10 * 1024 * 1024,
        variants: &variants,
        settings: &settings,
    };
    let d2 = state.decide(&reversal, settled + Duration::from_secs(1));
    assert_eq!(d2.reason(), AbrReason::MinInterval);
}

/// Wall-clock interval measured against the instant `AbrState` captured at
/// construction: both must come from the same clock, so this one opts out
#[kithara::test(flash(false))]
fn min_switch_interval_guards_the_first_switch_of_a_session() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    let variants = test_variants_3();
    let settings = AbrSettings::builder()
        .min_switch_interval(Duration::from_secs(30))
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let now = Instant::now();
    let view = AbrView {
        estimate_bps: Some(3_000_000),
        buffer_ahead: None,
        bytes_downloaded: 10 * 1024 * 1024,
        variants: &variants,
        settings: &settings,
    };

    assert_eq!(
        state.decide(&view, now + Duration::from_secs(1)).reason(),
        AbrReason::MinInterval,
        "a fast first sample must not flip the variant out from under a \
         listener who has barely started playing"
    );
    assert!(
        state.decide(&view, now + Duration::from_secs(30)).changed(),
        "once the interval has elapsed the same evidence must be acted on"
    );
}

#[kithara::test]
fn urgent_down_switch_is_not_held_by_the_switch_interval() {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(2))));
    let variants = test_variants_3();
    let settings = AbrSettings::builder()
        .min_switch_interval(Duration::from_secs(30))
        .urgent_downswitch_buffer(Duration::from_secs(5))
        .build();
    let now = Instant::now();
    let view = AbrView {
        estimate_bps: Some(1),
        buffer_ahead: Some(Duration::ZERO),
        bytes_downloaded: 10 * 1024 * 1024,
        variants: &variants,
        settings: &settings,
    };

    let decision = state.decide(&view, now + Duration::from_secs(1));
    assert_eq!(
        decision.reason(),
        AbrReason::UrgentDownSwitch,
        "a starving reader must not be held on a variant that cannot feed it"
    );
    assert!(decision.changed());
}

/// A locked `AbrState` must never change variant, regardless of bandwidth
/// samples. Parametrized to cover both directions:
/// * locked-at-0 under very-high bandwidth → up-switch rejected
/// * locked-at-2 under very-low bandwidth → down-switch rejected
#[kithara::test]
#[case::rejects_up_switch(0, 20_000_000, 100_000)]
#[case::rejects_down_switch(2, 10_000, 1)]
fn locked_state_rejects_switch(
    #[case] locked_variant: usize,
    #[case] base_bps: u64,
    #[case] step_bps: u64,
) {
    let variants = test_variants_3();
    let settings = settings_fast();
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(locked_variant))));
    state.lock();

    let now = Instant::now();
    for i in 0..50u64 {
        let v = view_with_bw(Some(base_bps + i * step_bps), &variants, &settings);
        let d = state.decide(&v, now + StdDuration::from_millis(i));
        assert!(!d.changed(), "locked state decided to switch at iter {i}");
    }
    assert_eq!(
        state.current_variant_index(),
        VariantIndex::new(locked_variant)
    );
}

struct TickPeer {
    state: Arc<AbrState>,
    wake: Arc<Notify>,
    cancel: CancelToken,
}

struct CountingEstimator {
    samples: Arc<AtomicUsize>,
}

impl Estimator for CountingEstimator {
    fn estimate_bps(&self) -> Option<u64> {
        Some(2_000_000)
    }

    fn push_sample(&self, _bytes: u64, _duration: Duration, _source: BandwidthSource) {
        self.samples.fetch_add(1, Ordering::AcqRel);
    }

    fn seed_initial_bps(&self, _bps: u64) {}
}

struct CountingTickPeer {
    state: Arc<AbrState>,
    ticks: Arc<AtomicUsize>,
    cancel: CancelToken,
}

impl Abr for CountingTickPeer {
    fn cancel(&self) -> CancelToken {
        self.cancel.clone()
    }

    fn progress(&self) -> Option<AbrProgressSnapshot> {
        self.ticks.fetch_add(1, Ordering::AcqRel);
        None
    }

    fn state(&self) -> Option<Arc<AbrState>> {
        Some(Arc::clone(&self.state))
    }

    fn variants(&self) -> Vec<VariantInfo> {
        audio_variants_4tier()
    }
}

impl Abr for TickPeer {
    fn cancel(&self) -> CancelToken {
        self.cancel.clone()
    }

    fn state(&self) -> Option<Arc<AbrState>> {
        Some(Arc::clone(&self.state))
    }

    fn variants(&self) -> Vec<VariantInfo> {
        audio_variants_4tier()
    }

    fn wake(&self) {
        self.wake.notify_one();
    }
}

async fn poll_controller(controller: &Arc<AbrController>, deadline_elapsed: bool) -> bool {
    poll_controller_at(controller, Instant::now(), deadline_elapsed).await
}

/// Drive one poll pass at an instant the caller names. A test that states what
/// a wall-clock interval holds owns the clock the controller reads, so the
/// assertion is the property and not a race against the test's own setup.
async fn poll_controller_at(
    controller: &Arc<AbrController>,
    now: Instant,
    deadline_elapsed: bool,
) -> bool {
    poll_fn(|cx| Poll::Ready(controller.poll_ticks(cx, now, deadline_elapsed))).await
}

/// Whether a wake reached this peer. `notify_one` stores a permit when no
/// waiter is parked, so the first poll is ready exactly when a wake was issued
/// - which states the property without spending a timeout on its absence.
async fn was_woken(wake: &Notify) -> bool {
    let mut notified = pin!(wake.notified());
    poll_fn(|cx| Poll::Ready(Future::poll(notified.as_mut(), cx)))
        .await
        .is_ready()
}

#[kithara::test(tokio)]
async fn bandwidth_samples_are_preserved_across_immediate_ticks() {
    const SAMPLES: usize = 64;

    let samples = Arc::new(AtomicUsize::new(0));
    let ticks = Arc::new(AtomicUsize::new(0));
    let controller = AbrController::with_estimator(
        settings_fast(),
        Arc::new(CountingEstimator {
            samples: Arc::clone(&samples),
        }),
    );
    let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
    let peer: Arc<dyn Abr> = Arc::new(CountingTickPeer {
        state,
        cancel: CancelToken::never(),
        ticks: Arc::clone(&ticks),
    });
    let handle = controller.register(&peer);

    for _ in 0..SAMPLES {
        controller.record_bandwidth(
            handle.peer_id(),
            32 * 1024,
            Duration::from_millis(50),
            BandwidthSource::Network,
        );
    }

    assert_eq!(samples.load(Ordering::Acquire), SAMPLES);
    assert_eq!(ticks.load(Ordering::Acquire), SAMPLES);
    assert!(!poll_controller(&controller, false).await);
}

/// Publish two back-to-back samples through a controller whose emit throttle
/// is `interval`, and count what reached the bus.
fn throughput_samples_published_with(interval: Duration) -> usize {
    let controller = AbrController::with_estimator(
        AbrSettings::builder()
            .throughput_sample_min_interval(interval)
            .build(),
        Arc::new(ThroughputEstimator::new()) as Arc<_>,
    );
    let peer: Arc<dyn Abr> = Arc::new(TickPeer {
        cancel: CancelToken::never(),
        state: Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))))),
        wake: Arc::new(Notify::default()),
    });
    let bus = EventBus::new(DEFAULT_EVENT_BUS_CAPACITY);
    let mut rx = bus.subscribe();
    let handle = controller.register(&peer).with_bus(bus);

    for _ in 0..2 {
        controller.record_bandwidth(
            handle.peer_id(),
            32 * 1024,
            Duration::from_millis(50),
            BandwidthSource::Network,
        );
    }

    std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|Envelope { event, .. }| matches!(event, AbrEvent::ThroughputSample { .. }))
        .count()
}

/// The knob is a throttle, so a zero interval throttles nothing.
#[kithara::test]
fn a_zero_sample_interval_publishes_every_throughput_sample() {
    assert_eq!(throughput_samples_published_with(Duration::ZERO), 2);
}

/// A second sample arriving inside the interval is dropped, which is what
/// makes the interval a parameter worth setting.
#[kithara::test]
fn a_throughput_sample_inside_the_interval_is_not_published() {
    assert_eq!(
        throughput_samples_published_with(Duration::from_secs(3600)),
        1
    );
}

fn audio_variants_4tier() -> Vec<VariantInfo> {
    [66_000_u64, 134_000, 270_000, 900_000]
        .into_iter()
        .enumerate()
        .map(|(i, bps)| VariantInfo {
            variant_index: VariantIndex::new(i),
            bandwidth_bps: Some(bps),
            duration: VariantDuration::Unknown,
            name: None,
            codecs: None,
            container: None,
        })
        .collect()
}

/// Cold-start with the default `initial_throughput_bps = Some(2 Mbps)`
/// seed: first `tick` must request the highest variant fitting under
/// `2 Mbps / safety_factor (1.5) ≈ 1.33 Mbps` — variant 3 (900 kbps).
/// Without the seed (pre-refactor), `estimate_bps()` returns `None` →
/// `AbrReason::NoEstimate` → no pending switch and the player would
/// stay on the initial LQ variant until samples accumulate.
#[kithara::test(tokio)]
async fn auto_mode_with_default_seed_picks_high_variant_on_cold_start() {
    let settings = AbrSettings::builder()
        .min_switch_interval(Duration::ZERO)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let controller = AbrController::new(settings);
    let state = Arc::new(AbrState::new(AbrMode::Auto(None)));
    let peer = abr_peer(&state, audio_variants_4tier());
    let handle = controller.register(&peer);
    controller.run_tick(handle.peer_id(), Instant::now());
    assert_eq!(
        state.pending_target(),
        Some(VariantIndex::new(3)),
        "cold-start with default 2 Mbps seed must request the top variant"
    );
    drop(handle);
}

/// Explicit opt-out: `initial_throughput_bps = None` preserves the
/// historical cold-start path. First `tick` sees no estimate, returns
/// `AbrReason::NoEstimate`, no pending switch — player stays on the
/// initial variant (0).
#[kithara::test(tokio)]
async fn auto_mode_without_seed_stays_on_initial_variant_on_cold_start() {
    let settings = AbrSettings::builder()
        .initial_throughput_bps(None)
        .min_switch_interval(Duration::ZERO)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let controller = AbrController::new(settings);
    let state = Arc::new(AbrState::new(AbrMode::Auto(None)));
    let peer = abr_peer(&state, audio_variants_4tier());
    let handle = controller.register(&peer);
    controller.run_tick(handle.peer_id(), Instant::now());
    assert_eq!(state.current_variant_index(), VariantIndex::new(0));
    assert_eq!(state.pending_target(), None);
    drop(handle);
}

/// The six below state the anti-oscillation interval, and none of them sleeps.
/// `switch_interval_remaining` measures from the session's start, so each names
/// that instant through `new_at` and drives the controller at instants derived
/// from it: the interval either holds or has expired by arithmetic, never by
/// how long the test's own setup took. That is what lets them run under an
/// interpreter hundreds of times slower than the machine, and it is stricter
/// besides - the retry deadline is asserted exactly rather than slept through.
#[kithara::test(tokio)]
async fn min_interval_ticks_without_another_bandwidth_sample() {
    const INTERVAL: Duration = Duration::from_millis(20);

    let settings = AbrSettings::builder()
        .min_switch_interval(INTERVAL)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let controller = AbrController::new(settings);
    let session = Instant::now();
    let state = Arc::new(AbrState::new_at(
        AbrMode::Auto(Some(VariantIndex::new(0))),
        session,
    ));
    let wake = Arc::new(Notify::default());
    let peer: Arc<dyn Abr> = Arc::new(TickPeer {
        cancel: CancelToken::never(),
        state: Arc::clone(&state),
        wake: Arc::clone(&wake),
    });
    let handle = controller.register(&peer);

    handle.reevaluate();
    assert!(poll_controller_at(&controller, session, false).await);
    assert_eq!(
        state.pending_target(),
        None,
        "the anti-oscillation interval must hold the first switch"
    );
    assert!(
        !was_woken(&wake).await,
        "a held switch must not wake its peer"
    );
    assert_eq!(
        controller.next_tick_deadline(),
        Some(session + INTERVAL),
        "the held switch must schedule its own retry at the interval"
    );

    assert!(poll_controller_at(&controller, session + INTERVAL, true).await);
    assert!(
        was_woken(&wake).await,
        "the interval-gated tick must wake its peer"
    );
    assert_eq!(state.pending_target(), Some(VariantIndex::new(3)));
}

#[kithara::test(tokio)]
async fn peer_cancel_stops_the_scheduled_min_interval_tick() {
    const INTERVAL: Duration = Duration::from_millis(100);

    let settings = AbrSettings::builder()
        .min_switch_interval(INTERVAL)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let controller = AbrController::new(settings);
    let session = Instant::now();
    let state = Arc::new(AbrState::new_at(
        AbrMode::Auto(Some(VariantIndex::new(0))),
        session,
    ));
    let wake = Arc::new(Notify::default());
    let peer_cancel = CancelToken::never();
    let peer: Arc<dyn Abr> = Arc::new(TickPeer {
        cancel: peer_cancel.clone(),
        state: Arc::clone(&state),
        wake: Arc::clone(&wake),
    });
    let handle = controller.register(&peer);

    handle.reevaluate();
    assert!(poll_controller_at(&controller, session, false).await);
    assert_eq!(
        controller.next_tick_deadline(),
        Some(session + INTERVAL),
        "the held switch must schedule its own retry at the interval"
    );
    peer_cancel.cancel();
    assert!(controller.peer_entry(handle.peer_id()).is_none());

    assert!(!poll_controller_at(&controller, session + INTERVAL, true).await);
    assert!(
        !was_woken(&wake).await,
        "cancelling the peer scope must suppress its delayed wake"
    );
    assert_eq!(state.pending_target(), None);
}

#[kithara::test(tokio)]
async fn controller_cancel_stops_the_scheduled_min_interval_tick() {
    const INTERVAL: Duration = Duration::from_millis(100);

    let controller_cancel = CancelToken::never();
    let settings = AbrSettings::builder()
        .cancel(controller_cancel.clone())
        .min_switch_interval(INTERVAL)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let controller = AbrController::new(settings);
    let session = Instant::now();
    let state = Arc::new(AbrState::new_at(
        AbrMode::Auto(Some(VariantIndex::new(0))),
        session,
    ));
    let wake = Arc::new(Notify::default());
    let peer: Arc<dyn Abr> = Arc::new(TickPeer {
        cancel: CancelToken::never(),
        state: Arc::clone(&state),
        wake: Arc::clone(&wake),
    });
    let handle = controller.register(&peer);

    handle.reevaluate();
    assert!(poll_controller_at(&controller, session, false).await);
    assert_eq!(
        controller.next_tick_deadline(),
        Some(session + INTERVAL),
        "the held switch must schedule its own retry at the interval"
    );
    controller_cancel.cancel();
    assert!(controller.peer_entry(handle.peer_id()).is_none());

    assert!(!poll_controller_at(&controller, session + INTERVAL, true).await);
    assert!(
        !was_woken(&wake).await,
        "cancelling the controller parent must suppress delayed peer wakes"
    );
    assert_eq!(state.pending_target(), None);
}

#[kithara::test(tokio)]
async fn peer_cancel_does_not_stop_a_sibling_tick() {
    const INTERVAL: Duration = Duration::from_millis(100);

    let settings = AbrSettings::builder()
        .min_switch_interval(INTERVAL)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let controller = AbrController::new(settings);
    let session = Instant::now();
    let first_state = Arc::new(AbrState::new_at(
        AbrMode::Auto(Some(VariantIndex::new(0))),
        session,
    ));
    let second_state = Arc::new(AbrState::new_at(
        AbrMode::Auto(Some(VariantIndex::new(0))),
        session,
    ));
    let first_wake = Arc::new(Notify::default());
    let second_wake = Arc::new(Notify::default());
    let first_cancel = CancelToken::never();
    let first_peer: Arc<dyn Abr> = Arc::new(TickPeer {
        cancel: first_cancel.clone(),
        state: Arc::clone(&first_state),
        wake: Arc::clone(&first_wake),
    });
    let second_peer: Arc<dyn Abr> = Arc::new(TickPeer {
        cancel: CancelToken::never(),
        state: Arc::clone(&second_state),
        wake: Arc::clone(&second_wake),
    });
    let first_handle = controller.register(&first_peer);
    let second_handle = controller.register(&second_peer);

    first_handle.reevaluate();
    second_handle.reevaluate();
    assert!(poll_controller_at(&controller, session, false).await);
    assert_eq!(
        controller.next_tick_deadline(),
        Some(session + INTERVAL),
        "both held switches must schedule their retry at the interval"
    );
    first_cancel.cancel();
    assert!(controller.peer_entry(first_handle.peer_id()).is_none());
    assert!(controller.peer_entry(second_handle.peer_id()).is_some());

    assert!(poll_controller_at(&controller, session + INTERVAL, true).await);
    assert!(
        was_woken(&second_wake).await,
        "the surviving peer's interval-gated tick must still wake it"
    );
    assert!(
        !was_woken(&first_wake).await,
        "cancelling one peer must not wake it or cancel a sibling tick"
    );
    assert_eq!(first_state.pending_target(), None);
    assert_eq!(second_state.pending_target(), Some(VariantIndex::new(3)));
}

#[kithara::test(tokio)]
async fn dropping_handle_stops_only_its_scheduled_tick() {
    const INTERVAL: Duration = Duration::from_millis(100);

    let settings = AbrSettings::builder()
        .min_switch_interval(INTERVAL)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let controller = AbrController::new(settings);
    let session = Instant::now();
    let state = Arc::new(AbrState::new_at(
        AbrMode::Auto(Some(VariantIndex::new(0))),
        session,
    ));
    let wake = Arc::new(Notify::default());
    let peer: Arc<dyn Abr> = Arc::new(TickPeer {
        cancel: CancelToken::never(),
        state: Arc::clone(&state),
        wake: Arc::clone(&wake),
    });
    let handle = controller.register(&peer);

    handle.reevaluate();
    assert!(poll_controller_at(&controller, session, false).await);
    assert_eq!(
        controller.next_tick_deadline(),
        Some(session + INTERVAL),
        "the held switch must schedule its own retry at the interval"
    );
    drop(handle);

    assert!(!poll_controller_at(&controller, session + INTERVAL, true).await);
    assert!(
        !was_woken(&wake).await,
        "dropping the ABR handle must suppress its scheduled tick"
    );
    assert_eq!(state.pending_target(), None);
}

#[kithara::test(tokio)]
async fn dropped_peer_does_not_leave_a_due_tick_deadline_spinning() {
    const INTERVAL: Duration = Duration::from_millis(20);

    let settings = AbrSettings::builder()
        .min_switch_interval(INTERVAL)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let controller = AbrController::new(settings);
    let session = Instant::now();
    let state = Arc::new(AbrState::new_at(
        AbrMode::Auto(Some(VariantIndex::new(0))),
        session,
    ));
    let peer: Arc<dyn Abr> = Arc::new(TickPeer {
        state,
        cancel: CancelToken::never(),
        wake: Arc::new(Notify::default()),
    });
    let handle = controller.register(&peer);

    handle.reevaluate();
    assert!(poll_controller_at(&controller, session, false).await);
    let deadline = session + INTERVAL;
    assert_eq!(controller.next_tick_deadline(), Some(deadline));
    drop(peer);

    assert!(poll_controller_at(&controller, deadline, true).await);
    assert_eq!(controller.next_tick_deadline(), None);
    assert!(!poll_controller_at(&controller, deadline, true).await);
}

/// Prod trace (`runtime_manual_switch_works_when_all_segments_cached`): an
/// `UrgentDownSwitch` latched on the initial 2 Mbps seed survived nine
/// `AlreadyOptimal` ticks after the estimate recovered, then committed a
/// quality drop the controller no longer wanted. `AlreadyOptimal` is the
/// live verdict for the current variant, so the tick must retract the
/// stale throughput-driven pending.
#[kithara::test(tokio)]
async fn tick_already_optimal_retracts_stale_throughput_pending() {
    let controller = AbrController::new(settings_fast());
    let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(2)))));
    let peer = abr_peer(&state, test_variants_3());
    let handle = controller.register(&peer);

    // A quality down-switch, not a rescue: a rescue answers a variant that
    // stopped delivering, and a recovered estimate is no evidence about that.
    state.request_target(VariantIndex::new(0), AbrReason::DownSwitch);

    // A fast real sample recovers the estimate far above every tier, so the
    // tick inside `record_bandwidth` reports `AlreadyOptimal` at variant 2.
    controller.record_bandwidth(
        handle.peer_id(),
        30_000_000,
        Duration::from_secs(1),
        BandwidthSource::Network,
    );
    assert_eq!(
        state.pending_target(),
        None,
        "an AlreadyOptimal verdict must drop the stale throughput pending"
    );
    assert_eq!(
        state.current_variant_index(),
        VariantIndex::new(2),
        "retraction drops the intent, never the audible variant"
    );
    assert!(!poll_controller(&controller, false).await);
    drop(handle);
}

/// Wall-clock interval measured against the instant `AbrState` captured at
/// construction: both must come from the same clock, so this one opts out
#[kithara::test(flash(false))]
fn tick_min_interval_hold_preserves_pending() {
    let settings = AbrSettings::builder()
        .min_switch_interval(Duration::from_secs(30))
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let controller = AbrController::new(settings);
    let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
    let peer = abr_peer(&state, audio_variants_4tier());
    let handle = controller.register(&peer);
    state.request_target(VariantIndex::new(3), AbrReason::UpSwitch);

    // The 2 Mbps seed still wants the up-switch; only the interval holds it,
    // so `evaluate()` reports `Stay { MinInterval }` on this tick.
    controller.run_tick(handle.peer_id(), Instant::now());

    assert_eq!(
        state.pending_target(),
        Some(VariantIndex::new(3)),
        "a MinInterval hold wraps a switch evaluate() still wants — the \
         pending must survive it"
    );
    drop(handle);
}

/// Full production path of the #107 ping-pong: pinning to 2 queues a
/// boundary switch via the synchronous tick inside `set_mode`; re-pinning
/// back to the current variant must kill that queued switch instead of
/// leaving it to commit at the next boundary against the user's latest
/// choice.
#[kithara::test(tokio)]
async fn set_mode_back_to_current_kills_queued_switch() {
    let controller = AbrController::new(settings_fast());
    let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
    let peer = abr_peer(&state, test_variants_3());
    let handle = controller.register(&peer);
    handle
        .set_mode(AbrMode::Manual(VariantIndex::new(2)))
        .expect("variant 2 exists");
    assert!(poll_controller(&controller, false).await);
    assert_eq!(
        state.pending_target(),
        Some(VariantIndex::new(2)),
        "manual pin must queue a boundary switch"
    );
    handle
        .set_mode(AbrMode::Manual(VariantIndex::new(0)))
        .expect("variant 0 exists");
    assert_eq!(
        state.pending_target(),
        None,
        "re-pin to the current variant must supersede the queued switch"
    );
    assert!(handle.peek_pending_decision().is_none());
    drop(handle);
}

#[kithara::test(tokio)]
async fn lock_refcount_holds_across_record_bandwidth() {
    let settings = settings_fast();
    let controller =
        AbrController::with_estimator(settings, Arc::new(ThroughputEstimator::new()) as Arc<_>);

    let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
    state.lock();
    let peer = abr_peer(&state, test_variants_3());
    let handle = controller.register(&peer);

    for _ in 0..20 {
        controller.record_bandwidth(
            handle.peer_id(),
            128 * 1024,
            Duration::from_millis(50),
            BandwidthSource::Network,
        );
    }
    assert_eq!(state.current_variant_index(), VariantIndex::new(0));
    assert!(state.is_locked());

    state.unlock();
    assert!(!state.is_locked());
    drop(handle);
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Lock,
    PushBandwidth { bps: u64 },
    SetMode(ModeOp),
    Tick,
    Unlock,
}

#[derive(Clone, Copy, Debug)]
enum ModeOp {
    Auto,
    ManualOne,
    ManualTwo,
    ManualZero,
}

fn mode_from(op: ModeOp) -> AbrMode {
    match op {
        ModeOp::Auto => AbrMode::Auto(None),
        ModeOp::ManualOne => AbrMode::Manual(VariantIndex::new(1)),
        ModeOp::ManualTwo => AbrMode::Manual(VariantIndex::new(2)),
        ModeOp::ManualZero => AbrMode::Manual(VariantIndex::new(0)),
    }
}

/// Drive one tick the way `controller::tick` drives it: a changed decision
/// becomes a pending request, and a separate boundary commit publishes it.
/// Publication is the step [`AbrState::lock`] withholds, so a model that
/// stores the variant straight from the decision never reaches the gate that
/// enforces SEEK-NO-SWITCH.
fn drive_tick(state: &AbrState, view: &AbrView<'_>, now: Instant) {
    let decision = state.decide(view, now);
    if decision.changed() {
        state.request_target(decision.target(), decision.reason());
    }
    if let Some(claim) = state.claim_pending_decision(state.current_variant_index()) {
        assert!(state.commit_pending(claim, now));
    }
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (1_000u64..20_000_000u64).prop_map(|bps| Op::PushBandwidth { bps }),
        prop_oneof![
            Just(ModeOp::Auto),
            Just(ModeOp::ManualZero),
            Just(ModeOp::ManualOne),
            Just(ModeOp::ManualTwo),
        ]
        .prop_map(Op::SetMode),
        Just(Op::Lock),
        Just(Op::Unlock),
        Just(Op::Tick),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        // Measured under Miri: one case of this sequence costs the interpreter
        // about a minute, so sixty-four of them are seventy minutes of a lane
        // whose whole budget is four hours and which has four more crates to
        // interpret. That lane is here for the atomics' memory ordering, and a
        // handful of sequences reaches the same stores and loads; the full
        // sweep still runs on every lane that executes rather than interprets.
        cases: if cfg!(miri) { 8 } else { 64 },
        ..ProptestConfig::default()
    })]

    /// SEEK-NO-SWITCH holds under any random op sequence, and manual mode
    /// always targets its requested index on the next decision.
    #[test]
    fn abr_state_respects_invariants(ops in proptest::collection::vec(arb_op(), 1..80)) {
        let variants = test_variants_3();
        let settings = settings_fast();
        let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));

        let mut lock_depth = 0usize;
        let mut variant_at_lock = None;
        let mut current_bps: Option<u64> = None;
        let base_now = Instant::now();
        let mut tick = 0u64;

        for op in ops {
            tick = tick.saturating_add(1);
            let now = base_now + StdDuration::from_millis(tick * 10);

            match op {
                Op::PushBandwidth { bps } => {
                    current_bps = Some(bps);
                }
                Op::SetMode(mode_op) => {
                    state.set_mode(mode_from(mode_op));
                }
                Op::Lock => {
                    if lock_depth == 0 {
                        variant_at_lock = Some(state.current_variant_index());
                    }
                    state.lock();
                    lock_depth = lock_depth.saturating_add(1);
                }
                Op::Unlock => {
                    if lock_depth > 0 {
                        state.unlock();
                        lock_depth -= 1;
                        if lock_depth == 0 {
                            variant_at_lock = None;
                        }
                    }
                }
                Op::Tick => {
                    let view = view_with_bw(current_bps, &variants, &settings);
                    drive_tick(&state, &view, now);
                }
            }

            prop_assert_eq!(state.lock_count(), lock_depth);

            if lock_depth > 0 {
                prop_assert_eq!(
                    state.current_variant_index(),
                    variant_at_lock.expect("variant snapshot at first lock"),
                    "SEEK-NO-SWITCH violated while locked"
                );
            }

            if let AbrMode::Manual(idx) = state.mode()
                && idx.get() < variants.len()
                && lock_depth == 0
            {
                let view = view_with_bw(current_bps, &variants, &settings);
                drive_tick(&state, &view, now);
                prop_assert_eq!(
                    state.current_variant_index(),
                    idx,
                    "manual(idx) must pin variant_index to idx after a tick"
                );
            }
        }
    }

    /// Monotonically increasing bandwidth (unlocked, Auto) never causes
    /// a down-switch.
    #[test]
    fn monotonic_bandwidth_never_down_switches(
        steps in proptest::collection::vec(200_000u64..10_000_000u64, 1..40),
    ) {
        let variants = test_variants_3();
        let settings = settings_fast();
        let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));

        let mut cumulative_bps: u64 = 0;
        let mut prev_variant: Option<VariantIndex> = None;
        let base_now = Instant::now();

        for (tick, delta) in steps.into_iter().enumerate() {
            cumulative_bps = cumulative_bps.saturating_add(delta);
            let now = base_now + StdDuration::from_millis((tick as u64).saturating_add(1) * 10);
            let view = view_with_bw(Some(cumulative_bps), &variants, &settings);
            let d = state.decide(&view, now);
            if d.changed() && d.reason() == AbrReason::DownSwitch {
                prop_assert!(
                    false,
                    "DownSwitch must not fire under monotonic bandwidth: bps={cumulative_bps}",
                );
            }
            if d.changed() {
                state.apply_decision(&d, now);
            }
            let current = state.current_variant_index();
            if let Some(prev) = prev_variant {
                prop_assert!(
                    current >= prev,
                    "variant_index must not regress under monotonic bandwidth"
                );
            }
            prev_variant = Some(current);
        }
    }
}
