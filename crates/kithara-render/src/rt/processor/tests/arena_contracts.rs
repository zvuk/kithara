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
    let input = pcm_samples(1.0, &[0.0; 2], false);
    let original = std::ptr::from_ref(input.as_ref());
    let attach = send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: slot_id,
            pcm: input,
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
        [DeckPart::Returned(Returned::Pcm { slot, pcm, .. })]
            if *slot == slot_id && std::ptr::eq(pcm.as_ref(), original)
    ));
    assert_eq!(occupied(&mixer).len(), 0);
}

#[kithara::test]
fn engine_arena_full_error() {
    let config = DeckMixerConfig::builder()
        .slots(NonZeroUsize::new(1).expect("one slot"))
        .build();
    let (mut mixer, mut ends) = mixer_with_config(config, 8);
    let first_pcm = pcm_samples(1.0, &[0.0; 2], false);
    let first_ptr = std::ptr::from_ref(first_pcm.as_ref());
    let first = send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: first_pcm,
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
            pcm: pcm_samples(1.0, &[0.0; 2], false),
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
    assert_eq!(
        mixer
            .track(A)
            .and_then(|track| track.slot_binding(SessionEpoch::new(0)))
            .map(|binding| binding.attachment()),
        Some(first)
    );
    assert_eq!(
        mixer.track(A).map(PlayerTrack::segment),
        Some(SegmentId::FIRST)
    );
    assert!(mixer.track(B).is_none());

    let segment = SegmentId::FIRST.next();
    let second_pcm = pcm_samples(1.0, &[0.0; 2], false);
    let second_ptr = std::ptr::from_ref(second_pcm.as_ref());
    let second = send(
        &mut ends,
        When::Next,
        vec![DeckPart::Replace {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: second_pcm,
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
        [DeckPart::Returned(Returned::Pcm { slot, pcm, .. })]
            if *slot == A && std::ptr::eq(pcm.as_ref(), first_ptr)
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
        [DeckPart::Returned(Returned::Pcm { slot, pcm, .. })]
            if *slot == A && std::ptr::eq(pcm.as_ref(), second_ptr)
    ));
    assert_eq!(occupied(&mixer).len(), 0);
}
