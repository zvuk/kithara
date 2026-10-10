use super::*;

/// A slot started on a frame inside a block is silent before that frame and sounds after it.
#[kithara::test(tokio)]
async fn a_started_slot_sounds_from_its_frame(constant_half: &'static [u8]) {
    const START: usize = 100;
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            slot: A,
            pcm: pcm(constant_half, "a", 60.0),
            segment: SegmentId::FIRST,
        }],
    );
    let start = send_on(
        &mut ends,
        at(START),
        &[(A, None)],
        vec![DeckPart::Start {
            slot: A,
            fade: Fade::Declick,
        }],
    );

    let left = render(&mut mixer, 0);

    assert!(left[..START].iter().all(|sample| *sample == 0.0));
    assert!(left[BLOCK - 1] > 0.0, "the slot sounds after its start");
    assert_applied_at(outcome_of(&receipts(&mut ends), start), START);
}

/// A slot that enters with a crossfade sounds the incoming gain of `gains` frame by frame from
/// the frame it started on.
#[kithara::test(tokio)]
async fn a_crossfade_start_follows_the_incoming_gains(constant_half: &'static [u8]) {
    let settings = CrossfadeSettings::new(0.016, CrossfadeCurve::EqualPower, 0.7, 0.3)
        .expect("valid settings");
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        When::Next,
        vec![
            DeckPart::Attach {
                slot: A,
                pcm: pcm(constant_half, "a", 60.0),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Crossfade(settings),
            },
        ],
    );

    let left = render(&mut mixer, 0);

    let frames = (0.016_f32 * 44_100.0).round();
    for (frame, sample) in left.iter().enumerate() {
        let progress = cast::<usize, f32>(frame).unwrap_or(f32::MAX) / (frames - 1.0);
        let (_, into) = settings.gains(progress);
        assert!(
            (sample - LEVEL * into).abs() < 1e-6,
            "frame {frame}: {sample} against {}",
            LEVEL * into
        );
    }
}

/// A fade down to silence stops its slot and reports the frame it went silent on.
#[kithara::test(tokio)]
async fn a_fade_to_silence_stops_the_slot_and_reports_faded(constant_half: &'static [u8]) {
    const FADE_AT: usize = BLOCK + 10;
    let settings =
        CrossfadeSettings::new(0.004, CrossfadeCurve::Linear, 1.0, 0.5).expect("valid settings");
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        When::Next,
        vec![
            DeckPart::Attach {
                slot: A,
                pcm: pcm(constant_half, "a", 60.0),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    render(&mut mixer, 0);
    send(
        &mut ends,
        at(FADE_AT),
        vec![DeckPart::Fade {
            slot: A,
            settings,
            dir: FadeDir::Out,
        }],
    );

    render(&mut mixer, BLOCK);

    let frames = 176;
    assert_eq!(
        ends.events.drain().collect::<Vec<_>>(),
        [DeckEvent::Faded {
            slot: A,
            at: frame_at(FADE_AT + frames),
        }]
    );
    assert_eq!(
        mixer.track(A).map(PlayerTrack::state),
        Some(SlotState::Stopped)
    );
}

/// A replace on a frame plays the old consumer's next frames out of the slot's tail, ramped down
/// to silence under the new consumer that starts on the same frame, and returns the old one.
#[kithara::test(tokio)]
async fn a_replace_plays_the_old_consumer_out_of_the_tail(constant_half: &'static [u8]) {
    const REPLACE_AT: usize = BLOCK + 100;
    let (mut mixer, mut ends) = mixer_without_declick();
    send(
        &mut ends,
        When::Next,
        vec![
            DeckPart::Attach {
                slot: A,
                pcm: pcm(constant_half, "old", 60.0),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    render(&mut mixer, 0);
    let replace = send_on(
        &mut ends,
        at(REPLACE_AT),
        &[(A, None)],
        vec![DeckPart::Replace {
            slot: A,
            pcm: pcm(constant_half, "new", 60.0),
            segment: SegmentId::FIRST,
        }],
    );

    let left = render(&mut mixer, BLOCK);

    let offset = REPLACE_AT - BLOCK;
    assert!((left[offset - 1] - LEVEL).abs() < 1e-6);
    assert!(
        (left[offset] - 2.0 * LEVEL).abs() < 1e-6,
        "the tail starts at full gain under the new consumer: {}",
        left[offset]
    );
    assert!(
        left[offset..].windows(2).all(|pair| pair[1] <= pair[0]),
        "the tail ramps down"
    );
    let receipts = receipts(&mut ends);
    let (_, outcome, parts) = receipts
        .iter()
        .find(|(seq, ..)| *seq == replace)
        .expect("the replace is answered");
    assert_applied_at(outcome, REPLACE_AT);
    assert!(
        matches!(parts.as_slice(), [DeckPart::Returned(Returned::Pcm { slot, pcm: old })] if *slot == A && &**old.src() == "old"),
        "the old consumer comes back: {parts:?}"
    );
}

/// A chain starts its slot on the frame after the other slot's last, sample for sample, and
/// applies on that frame.
#[kithara::test(tokio)]
async fn a_chain_starts_its_slot_on_the_frame_after_the_end(constant_half: &'static [u8]) {
    let (mut mixer, mut ends) = mixer_without_declick();
    let attached = send_on(
        &mut ends,
        When::Next,
        &[(A, None), (B, None)],
        vec![
            DeckPart::Attach {
                slot: A,
                pcm: pcm(constant_half, "ending", 0.005),
                segment: SegmentId::FIRST,
            },
            DeckPart::Attach {
                slot: B,
                pcm: pcm(constant_half, "next", 60.0),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    apply_initial_batch(&mut mixer);
    let chain = send_on(
        &mut ends,
        When::Deferred,
        &[(B, Some(attached))],
        vec![DeckPart::Chain { from: A, to: B }],
    );

    let left = render(&mut mixer, 0);

    let ended = ends
        .events
        .drain()
        .find_map(|event| match event {
            DeckEvent::Ended { slot, at } if slot == A => Some(at),
            _ => None,
        })
        .expect("the first slot ends inside the block");
    let end = usize::try_from(i64::from(ended)).expect("the end is in the block");
    assert!(
        left.iter().all(|sample| (sample - LEVEL).abs() < 1e-6),
        "no frame is lost at the seam"
    );
    assert_applied_at(outcome_of(&receipts(&mut ends), chain), end);
    assert_eq!(
        mixer.track(B).map(PlayerTrack::state),
        Some(SlotState::Playing)
    );
}

/// A chain whose slot another batch shifted while it waited comes back stale.
#[kithara::test(tokio)]
async fn a_chain_whose_slot_shifted_comes_back_stale(constant_half: &'static [u8]) {
    let (mut mixer, mut ends) = mixer();
    let attached = send(
        &mut ends,
        When::Next,
        vec![
            DeckPart::Attach {
                slot: A,
                pcm: pcm(constant_half, "ending", 0.005),
                segment: SegmentId::FIRST,
            },
            DeckPart::Attach {
                slot: B,
                pcm: pcm(constant_half, "next", 60.0),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    apply_initial_batch(&mut mixer);
    let chain = send_on(
        &mut ends,
        When::Deferred,
        &[(B, Some(attached))],
        vec![DeckPart::Chain { from: A, to: B }],
    );
    send_on(
        &mut ends,
        at(10),
        &[(B, Some(attached))],
        vec![DeckPart::Adopt {
            slot: B,
            segment: SegmentId::FIRST.next(),
        }],
    );

    render(&mut mixer, 0);

    assert!(matches!(
        outcome_of(&receipts(&mut ends), chain),
        Outcome::Rejected(Rejection::Stale)
    ));
    assert_eq!(
        mixer.track(B).map(PlayerTrack::state),
        Some(SlotState::Stopped)
    );
}

/// Detaching a chained slot refuses the chain behind it: attached again, the slot waits for its
/// own start instead of being started by the chain.
#[kithara::test(tokio)]
async fn a_detached_slot_refuses_the_chain_behind_it(constant_half: &'static [u8]) {
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        When::Next,
        vec![
            DeckPart::Attach {
                slot: A,
                pcm: pcm(constant_half, "ending", 0.005),
                segment: SegmentId::FIRST,
            },
            DeckPart::Attach {
                slot: B,
                pcm: pcm(constant_half, "next", 60.0),
                segment: SegmentId::FIRST,
            },
        ],
    );
    apply_initial_batch(&mut mixer);
    let chain = send(
        &mut ends,
        When::Deferred,
        vec![DeckPart::Chain { from: A, to: B }],
    );
    send(&mut ends, When::Next, vec![DeckPart::Detach { slot: B }]);
    send(
        &mut ends,
        When::Next,
        vec![
            DeckPart::Attach {
                slot: B,
                pcm: pcm(constant_half, "next", 60.0),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );

    render(&mut mixer, 0);
    render(&mut mixer, BLOCK);

    assert!(matches!(
        outcome_of(&receipts(&mut ends), chain),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::Empty { slot: B }))
    ));
    assert_eq!(
        mixer.track(B).map(PlayerTrack::state),
        Some(SlotState::Stopped)
    );
}

/// Attaching to a slot that holds a track refuses the whole batch.
#[kithara::test(tokio)]
async fn an_attach_to_an_occupied_slot_is_refused(constant_half: &'static [u8]) {
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            slot: A,
            pcm: pcm(constant_half, "held", 60.0),
            segment: SegmentId::FIRST,
        }],
    );
    let second = send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            slot: A,
            pcm: pcm(constant_half, "refused", 60.0),
            segment: SegmentId::FIRST,
        }],
    );

    render(&mut mixer, 0);

    let receipts = receipts(&mut ends);
    assert!(matches!(
        outcome_of(&receipts, second),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::Occupied { slot: A }))
    ));
    assert_eq!(mixer.track(A).map(|track| &**track.src()), Some("held"));
}

/// A band cut sent to a playing deck rides the deck's own ring: the block after it renders the
/// deck lowered by the cut.
#[kithara::test(tokio)]
async fn a_band_cut_lowers_the_deck_output_in_the_next_block(constant_half: &'static [u8]) {
    const CUT_DB: f32 = -12.0;
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        When::Next,
        vec![
            DeckPart::Attach {
                slot: A,
                pcm: pcm(constant_half, "eq", 60.0),
                segment: SegmentId::FIRST,
            },
            DeckPart::Eq(DeckEqChange::Layout(eq_layout(&[GainDb::DEFAULT]))),
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    render(&mut mixer, 0);
    let open = render(&mut mixer, BLOCK)[BLOCK - 1];

    send(
        &mut ends,
        When::Next,
        vec![DeckPart::Eq(DeckEqChange::Gain {
            band: 0,
            gain: GainDb::from(CUT_DB),
        })],
    );
    let cut = render(&mut mixer, 2 * BLOCK)[BLOCK - 1];

    let expected = open * 10f32.powf(CUT_DB / 20.0);
    assert!(open > 0.1, "the deck sounds before the cut: {open}");
    assert!(
        (cut - expected).abs() < 1e-3,
        "cut {cut}, expected {expected} from {open}"
    );
}

/// Events the owner does not drain are dropped once the ring is full and counted in the
/// snapshot, instead of blocking the audio thread.
#[kithara::test(tokio)]
async fn a_full_event_ring_counts_its_overflows(constant_half: &'static [u8]) {
    let (mut mixer, mut ends) = mixer();
    let slots = DeckMixerConfig::default().slots().get();
    let capacity = slots * 16;
    for _ in 0..=capacity {
        send(
            &mut ends,
            When::Next,
            vec![
                DeckPart::Attach {
                    slot: A,
                    pcm: pcm(constant_half, "short", 0.001),
                    segment: SegmentId::FIRST,
                },
                DeckPart::Start {
                    slot: A,
                    fade: Fade::Declick,
                },
            ],
        );
        render(&mut mixer, 0);
        send(&mut ends, When::Next, vec![DeckPart::Detach { slot: A }]);
        render(&mut mixer, 0);
        drop(receipts(&mut ends));
    }

    let snapshot = ends.snapshot.read();
    assert_eq!(snapshot.metrics.event_overflows(), 1);
    assert_eq!(ends.events.drain().count(), capacity);
}
