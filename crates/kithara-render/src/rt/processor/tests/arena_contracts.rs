use super::*;

fn occupied(mixer: &TestMixer) -> Vec<Slot> {
    mixer
        .mixer
        .deck
        .tracks
        .slots()
        .filter(|slot| mixer.track(*slot).is_some())
        .collect()
}

#[kithara::test]
fn engine_allocate_and_release_slot() {
    let config = DeckMixerConfig::builder()
        .slots(NonZeroUsize::new(4).expect("four slots"))
        .build();
    let (mut mixer, mut ends) = mixer_with_config(config, 8);
    let slot_id = A;
    let attach = send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: slot_id,
            pcm: pcm_samples("allocated", 1.0, &[0.0; 2], false),
            segment: SegmentId::FIRST,
        }],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
    let answered = receipts(&mut ends);
    assert_applied_at(outcome_of(&answered, attach), 0);
    let active_slots = occupied(&mixer);
    assert_eq!(active_slots.len(), 1);
    assert!(active_slots.contains(&slot_id));

    let release = send(
        &mut ends,
        When::Next,
        vec![DeckPart::Detach { slot: slot_id }],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
    let answered = receipts(&mut ends);
    assert_applied_at(outcome_of(&answered, release), 0);
    assert!(matches!(
        answered[0].2.as_slice(),
        [DeckPart::Returned(Returned::Pcm { slot, pcm })]
            if *slot == slot_id && &**pcm.src() == "allocated"
    ));
    assert_eq!(occupied(&mixer).len(), 0);
}

#[kithara::test]
fn engine_arena_full_error() {
    let config = DeckMixerConfig::builder()
        .slots(NonZeroUsize::new(1).expect("one slot"))
        .build();
    let (mut mixer, mut ends) = mixer_with_config(config, 8);
    let first = send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: pcm_samples("first", 1.0, &[0.0; 2], false),
            segment: SegmentId::FIRST,
        }],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
    let answered = receipts(&mut ends);
    assert_applied_at(outcome_of(&answered, first), 0);
    assert_eq!(occupied(&mixer), vec![A]);
    assert!(mixer.track(B).is_none());

    let refused = send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: pcm_samples("refused", 1.0, &[0.0; 2], false),
            segment: SegmentId::FIRST.next(),
        }],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
    let answered = receipts(&mut ends);
    assert!(matches!(
        outcome_of(&answered, refused),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::Occupied { slot: A }))
    ));
    assert_eq!(occupied(&mixer), vec![A]);
    assert_eq!(mixer.track(A).map(|track| &**track.src()), Some("first"));
    assert_eq!(
        mixer.track(A).map(PlayerTrack::segment),
        Some(SegmentId::FIRST)
    );
    assert!(mixer.track(B).is_none());

    let segment = SegmentId::FIRST.next();
    let second = send(
        &mut ends,
        When::Next,
        vec![DeckPart::Replace {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: pcm_samples("second", 1.0, &[0.0; 2], false),
            segment,
        }],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
    let answered = receipts(&mut ends);
    assert_applied_at(outcome_of(&answered, second), 0);
    assert!(matches!(
        answered[0].2.as_slice(),
        [DeckPart::Returned(Returned::Pcm { slot, pcm })]
            if *slot == A && &**pcm.src() == "first"
    ));
    assert_eq!(occupied(&mixer), vec![A]);
    assert_eq!(mixer.track(A).map(PlayerTrack::segment), Some(segment));
    assert_eq!(
        mixer.track(A).map(PlayerTrack::state),
        Some(SlotState::Playing)
    );
    assert!(mixer.track(B).is_none());

    let release = send(&mut ends, When::Next, vec![DeckPart::Detach { slot: A }]);
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
    let answered = receipts(&mut ends);
    assert_applied_at(outcome_of(&answered, release), 0);
    assert!(matches!(
        answered[0].2.as_slice(),
        [DeckPart::Returned(Returned::Pcm { slot, pcm })]
            if *slot == A && &**pcm.src() == "second"
    ));
    assert_eq!(occupied(&mixer).len(), 0);
}
