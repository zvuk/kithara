use super::*;

#[kithara::test]
fn eof_during_a_crossfade_answers_the_transition_batch_once() {
    const EOF: usize = BLOCK + 88;
    let (mut mixer, mut ends) = mixer();
    let attached = send_on(
        &mut ends,
        When::Next,
        &[(A, None), (B, None)],
        vec![
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm_samples(1.0, &vec![LEVEL; EOF * 2], true),
                segment: SegmentId::FIRST,
            },
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: B,
                pcm: pcm_samples(1.0, &vec![LEVEL; BLOCK * 8], false),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    render(&mut mixer, 0);
    drop(receipts(&mut ends));
    let settings =
        CrossfadeSettings::new(1.0, CrossfadeCurve::Linear, 1.0, 0.5).expect("valid crossfade");
    let transition = send_on(
        &mut ends,
        at(BLOCK),
        &[(A, Some(attached)), (B, Some(attached))],
        vec![
            DeckPart::Start {
                slot: B,
                fade: Fade::Crossfade(settings),
            },
            DeckPart::Fade {
                slot: A,
                settings,
                dir: FadeDir::Out,
            },
        ],
    );
    render_frames(&mut mixer, BLOCK, 1);
    let replies = receipts(&mut ends);
    assert_eq!(
        replies.len(),
        1,
        "one verdict before the outgoing track ends"
    );
    let (_, outcome, parts) = replies
        .iter()
        .find(|(seq, ..)| *seq == transition)
        .expect("the crossfade applies before its outgoing track ends");
    assert_applied_at(outcome, BLOCK);
    assert!(
        matches!(
            parts.as_slice(),
            [
                DeckPart::Start {
                    slot: incoming,
                    fade: Fade::Crossfade(applied_in),
                },
                DeckPart::Fade {
                    slot: outgoing,
                    settings: applied_out,
                    dir: FadeDir::Out,
                },
            ] if *incoming == B && *outgoing == A
                && *applied_in == settings && *applied_out == settings
        ),
        "the transition returns both slot commands without an implicit Stop: {parts:?}"
    );
    for slot in [A, B] {
        assert_eq!(
            mixer.track(slot).map(PlayerTrack::state),
            Some(SlotState::Playing)
        );
    }
    assert!(ends.events.drain().next().is_none());
    render_frames(&mut mixer, BLOCK + 1, BLOCK - 1);
    assert!(
        receipts(&mut ends).is_empty(),
        "EOF cannot answer the fade twice"
    );
    assert_eq!(
        mixer.track(A).map(PlayerTrack::state),
        Some(SlotState::Ended)
    );
    assert_eq!(
        ends.events.drain().collect::<Vec<_>>(),
        [DeckEvent::Ended {
            slot: A,
            at: frame_at(EOF)
        }],
    );
    render(&mut mixer, BLOCK * 2);
    assert!(receipts(&mut ends).is_empty(), "one verdict per transition");
}

#[kithara::test]
fn eof_during_an_explicit_stop_returns_its_resume_mark_once() {
    const EOF: usize = BLOCK + 88;
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        When::Next,
        vec![
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm_samples(1.0, &vec![LEVEL; EOF * 2], true),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    render(&mut mixer, 0);
    drop(receipts(&mut ends));
    let settings =
        CrossfadeSettings::new(1.0, CrossfadeCurve::Linear, 1.0, 0.5).expect("valid stop fade");
    let stop = send(
        &mut ends,
        at(BLOCK),
        vec![DeckPart::Stop {
            slot: A,
            fade: Fade::Crossfade(settings),
        }],
    );

    render(&mut mixer, BLOCK);

    assert_stopped_once(&receipts(&mut ends), stop, BLOCK, A, EOF);
    assert_eq!(
        mixer.track(A).map(PlayerTrack::state),
        Some(SlotState::Ended)
    );
    assert_eq!(
        ends.events.drain().collect::<Vec<_>>(),
        [DeckEvent::Ended {
            slot: A,
            at: frame_at(EOF)
        }],
    );
    render(&mut mixer, BLOCK * 2);
    assert!(
        receipts(&mut ends).is_empty(),
        "one verdict per explicit stop"
    );
}

/// A slot started on a frame inside a block is silent before that frame and sounds after it.
#[kithara::test(tokio)]
async fn a_started_slot_sounds_from_its_frame(constant_half: &'static [u8]) {
    const START: usize = 100;
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: pcm(constant_half, 60.0),
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
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm(constant_half, 60.0),
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

    const FADE_AT: usize = BLOCK + 100;
    const FADE_FRAMES: usize = 4 * RATE as usize;
    const END: usize = FADE_AT + FADE_FRAMES + BLOCK;
    let settings =
        CrossfadeSettings::new(4.0, CrossfadeCurve::EqualPower, 0.7, 0.3).expect("four seconds");
    let outgoing = kithara_test_fixtures::unit_fixtures::render_crossfade_outgoing();
    let incoming = kithara_test_fixtures::unit_fixtures::render_crossfade_incoming();
    assert_eq!(outgoing.len(), END * 2);
    assert_eq!(incoming.len(), END * 2);
    let (mut mixer, mut ends) = mixer_without_declick();
    let attached = send_on(
        &mut ends,
        When::Next,
        &[(A, None), (B, None)],
        vec![
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm_samples(5.0, &outgoing, true),
                segment: SegmentId::FIRST,
            },
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: B,
                pcm: pcm_samples(5.0, &incoming, true),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    let before = render(&mut mixer, 0);
    assert_eq!(
        before,
        outgoing
            .iter()
            .step_by(2)
            .take(BLOCK)
            .copied()
            .collect::<Vec<_>>()
    );
    let initial = receipts(&mut ends);
    assert_eq!(initial.len(), 1);
    assert_applied_at(outcome_of(&initial, attached), 0);
    assert_eq!(
        mixer.track(A).map(PlayerTrack::state),
        Some(SlotState::Playing)
    );
    assert_eq!(
        mixer.track(B).map(PlayerTrack::state),
        Some(SlotState::Stopped)
    );
    assert!(ends.events.drain().next().is_none());

    let transition = send_on(
        &mut ends,
        at(FADE_AT),
        &[(A, Some(attached)), (B, Some(attached))],
        vec![
            DeckPart::Start {
                slot: B,
                fade: Fade::Crossfade(settings),
            },
            DeckPart::Fade {
                slot: A,
                settings,
                dir: FadeDir::Out,
            },
        ],
    );
    let mut cursor = BLOCK;
    let mut answered = false;
    let mut events = Vec::new();
    while cursor < END {
        let count = BLOCK.min(END - cursor);
        let rendered = render_frames(&mut mixer, cursor, count);
        for (offset, sample) in rendered.into_iter().enumerate() {
            let frame = cursor + offset;
            let expected = if frame < FADE_AT {
                outgoing[frame * 2]
            } else {
                let elapsed = frame - FADE_AT;
                let progress = cast::<usize, f32>(elapsed).expect("fixture frame")
                    / cast::<usize, f32>(FADE_FRAMES - 1).expect("fixture duration");
                let (out, into) = settings.gains(progress);
                outgoing[frame * 2] * out + incoming[elapsed * 2] * into
            };
            assert!(
                (sample - expected).abs() < 1e-6,
                "four-second frame {frame}: {sample} against {expected}"
            );
        }
        let replies = receipts(&mut ends);
        if answered {
            assert!(replies.is_empty(), "one answer for the manual transition");
        } else {
            assert_eq!(replies.len(), 1);
            let (seq, outcome, parts) = &replies[0];
            assert_eq!(*seq, transition);
            assert_applied_at(outcome, FADE_AT);
            assert!(matches!(parts.as_slice(), [
                DeckPart::Start { slot: incoming, fade: Fade::Crossfade(applied_in) },
                DeckPart::Fade { slot: outgoing, settings: applied_out, dir: FadeDir::Out },
            ] if *incoming == B && *outgoing == A
                && *applied_in == settings && *applied_out == settings));
            answered = true;
        }
        events.extend(ends.events.drain());
        cursor += count;
    }
    assert!(answered);
    assert_eq!(
        events,
        [DeckEvent::Faded {
            slot: A,
            at: frame_at(FADE_AT + FADE_FRAMES)
        }]
    );
    assert_eq!(
        mixer.track(A).map(PlayerTrack::state),
        Some(SlotState::Stopped)
    );
    assert_eq!(
        mixer.track(B).map(PlayerTrack::state),
        Some(SlotState::Playing)
    );
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.metrics.underruns(), 0);
    assert_eq!(snapshot.metrics.decode_errors(), 0);
    assert_eq!(
        snapshot.slots[usize::from(A.get())]
            .mark
            .expect("outgoing mark")
            .lane
            .frame,
        (FADE_AT + FADE_FRAMES) as u64
    );
    assert_eq!(
        snapshot.slots[usize::from(B.get())]
            .mark
            .expect("incoming mark")
            .lane
            .frame,
        (END - FADE_AT) as u64
    );
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
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm(constant_half, 60.0),
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
#[case::different_item(false)]
#[case::same_item(true)]
async fn a_replace_plays_the_old_consumer_out_of_the_tail(
    constant_half: &'static [u8],
    #[case] same_item: bool,
) {
    const REPLACE_AT: usize = BLOCK + 100;
    let (mut mixer, mut ends) = mixer_without_declick();
    let old_item = kithara_events::TrackId::allocate();
    let old_pcm = pcm(constant_half, 60.0);
    let old_ptr = std::ptr::from_ref(old_pcm.as_ref());
    send(
        &mut ends,
        When::Next,
        vec![
            DeckPart::Attach {
                item_id: old_item,
                slot: A,
                pcm: old_pcm,
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
        vec![
            DeckPart::Replace {
                item_id: if same_item {
                    old_item
                } else {
                    kithara_events::TrackId::allocate()
                },
                slot: A,
                pcm: pcm(constant_half, 60.0),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
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
    let mark = mixer.track(A).unwrap().mark(frame_at(BLOCK * 2)).unwrap();
    assert_eq!(
        mark.lane.frame,
        (2 * BLOCK - REPLACE_AT) as u64,
        "an unmarked Replace starts the new receiver at its head"
    );
    let receipts = receipts(&mut ends);
    let (_, outcome, parts) = receipts
        .iter()
        .find(|(seq, ..)| *seq == replace)
        .expect("the replace is answered");
    assert_applied_at(outcome, REPLACE_AT);
    assert!(
        matches!(parts.as_slice(), [DeckPart::Returned(Returned::Pcm { slot, pcm: old, .. }), DeckPart::Start { .. }] if *slot == A && std::ptr::eq(old.as_ref(), old_ptr)),
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
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm(constant_half, 0.005),
                segment: SegmentId::FIRST,
            },
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: B,
                pcm: pcm(constant_half, 60.0),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
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

#[kithara::test]
fn a_deferred_adopt_restarts_an_ended_slot_after_repeated_eof_observation() {
    let (mut mixer, mut ends) = mixer_without_declick();
    let spec = AudioSpec::new(2, shape().sample_rate);
    let old = SegmentId::FIRST;
    let current = old.next();
    let mut packets = PacketRing::new(spec, Duration::from_secs(1), 4);
    packets.push(PcmPacket::Chunk(Box::new(chunk(
        spec,
        old,
        0,
        0,
        &[LEVEL; 8],
    ))));
    let mut end = chunk(spec, old, 4, 4, &[]);
    end.meta.end_of_track = true;
    packets.push(PcmPacket::Chunk(Box::new(end)));
    let resource = Box::new(packets.receiver.take().expect("receiver"));
    let attached = send_on(
        &mut ends,
        When::Next,
        &[(A, None)],
        vec![
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: resource,
                segment: old,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    assert_eq!(
        render_frames(&mut mixer, 0, 8),
        [LEVEL, LEVEL, LEVEL, LEVEL, 0.0, 0.0, 0.0, 0.0]
    );
    assert_applied_at(outcome_of(&receipts(&mut ends), attached), 0);
    assert_eq!(
        mixer.track(A).map(PlayerTrack::state),
        Some(SlotState::Ended)
    );
    assert_eq!(
        ends.events.drain().collect::<Vec<_>>(),
        [DeckEvent::Ended {
            slot: A,
            at: frame_at(4)
        }]
    );
    assert_eq!(render_frames(&mut mixer, 8, 4), [0.0; 4]);
    assert_eq!(
        mixer.track(A).map(PlayerTrack::state),
        Some(SlotState::Ended)
    );
    assert!(receipts(&mut ends).is_empty());
    assert!(ends.events.drain().next().is_none());

    packets.push(PcmPacket::Chunk(Box::new(chunk(
        spec,
        current,
        100,
        1_000,
        &[0.25; 16],
    ))));
    let adopt = send_on(
        &mut ends,
        When::Deferred,
        &[(A, Some(attached))],
        vec![DeckPart::Adopt {
            slot: A,
            segment: current,
        }],
    );
    assert_eq!(render_frames(&mut mixer, 12, 4), [0.25; 4]);
    let replies = receipts(&mut ends);
    assert_eq!(replies.len(), 1);
    let (_, outcome, parts) = replies
        .iter()
        .find(|(seq, ..)| *seq == adopt)
        .expect("deferred Adopt");
    assert_applied_at(outcome, 12);
    assert!(
        matches!(parts.as_slice(), [DeckPart::Adopt { slot, segment }] if *slot == A && *segment == current)
    );
    assert_eq!(
        mixer.track(A).map(PlayerTrack::state),
        Some(SlotState::Playing)
    );
    assert_eq!(
        mixer.track(A).expect("restarted slot").mark(frame_at(16)),
        Some(crate::bridge::SlotMark {
            session: frame_at(16),
            lane: crate::LaneFrame {
                segment: current,
                frame: 104
            },
            position: spec.duration_for(1_004).expect("new source position"),
        })
    );
    assert!(ends.events.drain().next().is_none());
    assert_eq!(render_frames(&mut mixer, 16, 4), [0.25; 4]);
    assert!(receipts(&mut ends).is_empty());
    assert!(
        ends.events
            .drain()
            .all(|event| !matches!(event, DeckEvent::Ended { .. }))
    );
}

#[kithara::test]
fn a_late_chain_enters_after_repeated_eof_observation() {
    let (mut mixer, mut ends) = mixer_without_declick();
    let attached = send_on(
        &mut ends,
        When::Next,
        &[(A, None), (B, None)],
        vec![
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm_samples(1.0, &[LEVEL; 8], true),
                segment: SegmentId::FIRST,
            },
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: B,
                pcm: pcm_samples(1.0, &[0.25; 32], false),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    assert_eq!(
        render_frames(&mut mixer, 0, 8),
        [LEVEL, LEVEL, LEVEL, LEVEL, 0.0, 0.0, 0.0, 0.0]
    );
    assert_applied_at(outcome_of(&receipts(&mut ends), attached), 0);
    assert_eq!(
        mixer.track(A).map(PlayerTrack::state),
        Some(SlotState::Ended)
    );
    assert_eq!(
        mixer.track(B).map(PlayerTrack::state),
        Some(SlotState::Stopped)
    );
    assert_eq!(
        ends.events.drain().collect::<Vec<_>>(),
        [DeckEvent::Ended {
            slot: A,
            at: frame_at(4)
        }]
    );
    assert_eq!(render_frames(&mut mixer, 8, 4), [0.0; 4]);
    assert!(receipts(&mut ends).is_empty());
    assert!(ends.events.drain().next().is_none());
    let chain = send_on(
        &mut ends,
        When::Deferred,
        &[(A, Some(attached)), (B, Some(attached))],
        vec![DeckPart::Chain { from: A, to: B }],
    );
    assert_eq!(render_frames(&mut mixer, 12, 4), [0.25; 4]);
    let replies = receipts(&mut ends);
    assert_eq!(replies.len(), 1);
    let (_, outcome, parts) = replies
        .iter()
        .find(|(seq, ..)| *seq == chain)
        .expect("late Chain");
    assert_applied_at(outcome, 12);
    assert!(matches!(parts.as_slice(), [DeckPart::Chain { from, to }] if *from == A && *to == B));
    assert_eq!(
        mixer.track(A).map(PlayerTrack::state),
        Some(SlotState::Ended)
    );
    assert_eq!(
        mixer.track(B).map(PlayerTrack::state),
        Some(SlotState::Playing)
    );
    assert!(ends.events.drain().next().is_none());
    assert_eq!(render_frames(&mut mixer, 16, 4), [0.25; 4]);
    assert!(receipts(&mut ends).is_empty());
    assert!(
        ends.events
            .drain()
            .all(|event| !matches!(event, DeckEvent::Ended { .. }))
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
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm(constant_half, 0.005),
                segment: SegmentId::FIRST,
            },
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: B,
                pcm: pcm(constant_half, 60.0),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
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
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm(constant_half, 0.005),
                segment: SegmentId::FIRST,
            },
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: B,
                pcm: pcm(constant_half, 60.0),
                segment: SegmentId::FIRST,
            },
        ],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
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
                item_id: kithara_events::TrackId::allocate(),
                slot: B,
                pcm: pcm(constant_half, 60.0),
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
    let first = send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: pcm(constant_half, 60.0),
            segment: SegmentId::FIRST,
        }],
    );
    let second = send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: pcm(constant_half, 60.0),
            segment: SegmentId::FIRST,
        }],
    );

    render(&mut mixer, 0);

    let receipts = receipts(&mut ends);
    assert!(matches!(
        outcome_of(&receipts, second),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::Occupied { slot: A }))
    ));
    assert_eq!(
        mixer
            .track(A)
            .and_then(|track| track.slot_binding(SessionEpoch::new(0)))
            .map(|binding| binding.attachment()),
        Some(first)
    );
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
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm(constant_half, 60.0),
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
                    item_id: kithara_events::TrackId::allocate(),
                    slot: A,
                    pcm: pcm(constant_half, 0.001),
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

#[kithara::test(native, tokio)]
#[case::pause(DeckPart::Stop { slot: A, fade: Fade::Declick })]
#[case::seek(DeckPart::Adopt { slot: A, segment: SegmentId::FIRST.next() })]
#[case::detach(DeckPart::Detach { slot: A })]
async fn a_transport_reset_finishes_the_active_underrun_once(#[case] reset: DeckPart) {
    let mut fixture = crate::mock::pcm_fixture::PcmFixture::new(8, false).await;
    let mut prepared = crate::mock::pcm_fixture::chunk(SegmentId::FIRST, &[0.25; BLOCK]);
    prepared.meta.media_origin = Some(Duration::ZERO);
    fixture
        .push(PcmPacket::Chunk(Box::new(prepared)))
        .expect("prepared PCM");
    let resource = Box::new(fixture.receiver.take().expect("receiver"));
    let item = kithara_events::TrackId::allocate();
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        When::Next,
        vec![
            DeckPart::Attach {
                slot: A,
                item_id: item,
                pcm: resource,
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    render(&mut mixer, 0);
    assert!(ends.events.drain().next().is_none());
    render(&mut mixer, BLOCK);
    let position = AudioSpec::new(1, NonZeroU32::new(48_000).expect("rate"))
        .duration_for(u64::try_from(BLOCK).expect("block frames"))
        .expect("block duration");
    assert!(matches!(
        ends.events.drain().collect::<Vec<_>>().as_slice(),
        [DeckEvent::UnderrunStarted { slot, item: actual, at, position: Some(actual_position) }]
            if *slot == A && *actual == item && *at == frame_at(BLOCK) && *actual_position == position
    ));
    drop(receipts(&mut ends));
    let seeking = matches!(reset, DeckPart::Adopt { .. });
    send(&mut ends, When::Next, vec![reset]);
    render(&mut mixer, 2 * BLOCK);
    let events = ends.events.drain().collect::<Vec<_>>();
    assert!(
        matches!(
            events.first(),
            Some(DeckEvent::UnderrunEnded { slot, item: actual, at, frames, position: Some(actual_position), recovered: false })
                if *slot == A && *actual == item && *at == frame_at(2 * BLOCK)
                    && usize::try_from(*frames).expect("frame count") == BLOCK && *actual_position == position
        ),
        "the original window finishes with its complete missing-frame count: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, DeckEvent::UnderrunEnded { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, DeckEvent::UnderrunStarted { .. }))
            .count(),
        usize::from(seeking)
    );
    if seeking {
        let pending = mixer.deck.tracks.at(A).expect("adopted track");
        assert_eq!(
            pending.source_info(),
            None,
            "old geometry is not published for the adopted segment"
        );
        assert!(pending.mark(frame_at(2 * BLOCK)).is_none());
        assert!(matches!(events.last(),
            Some(DeckEvent::UnderrunStarted { item: actual, position: Some(actual_position), .. })
                if *actual == item && *actual_position == position));
        send(&mut ends, When::Next, vec![DeckPart::Detach { slot: A }]);
        render(&mut mixer, 3 * BLOCK);
        assert!(matches!(ends.events.drain().collect::<Vec<_>>().as_slice(),
            [DeckEvent::UnderrunEnded { item: actual, position: Some(actual_position), frames, recovered: false, .. }]
                if *actual == item && *actual_position == position && usize::try_from(*frames).expect("frame count") == BLOCK));
    }
}

enum ContinuationOutcome {
    Cut,
    Ahead,
    Pause,
    Hole,
    Rate,
    Closed,
    Axis,
    Owed,
}

#[kithara::test]
#[case::silent_slot(false, ContinuationOutcome::Cut)]
#[case::replace(true, ContinuationOutcome::Cut)]
#[case::waits_for_a_cue_ahead_of_the_consumed_mark(false, ContinuationOutcome::Ahead)]
#[case::replace_waits_for_a_cue_ahead_of_the_consumed_mark(true, ContinuationOutcome::Ahead)]
#[case::paused_while_parked(false, ContinuationOutcome::Pause)]
#[case::rejects_a_hole_in_a_private_receiver(true, ContinuationOutcome::Hole)]
#[case::rejects_a_later_rate_change(true, ContinuationOutcome::Rate)]
#[case::refuses_a_staged_receiver_closed_while_parked(false, ContinuationOutcome::Closed)]
#[case::refuses_a_private_receiver_closed_while_parked(true, ContinuationOutcome::Closed)]
#[case::refuses_an_axis_restart_while_parked(false, ContinuationOutcome::Axis)]
#[case::waits_for_a_session_dropout_to_be_repaid(false, ContinuationOutcome::Owed)]
fn a_source_continuation_catches_up_to_the_executed_pcm_cut(
    #[case] replace: bool,
    #[case] expected: ContinuationOutcome,
) {
    use kithara_events::TrackId;
    use kithara_signal::SourceSpan;

    let (mut mixer, mut ends) = mixer_without_declick();
    let spec = AudioSpec::new(2, NonZeroU32::new(RATE).unwrap());
    let duration = Duration::from_secs(12);
    let mut outgoing = PacketRing::new(spec, duration, 2);
    let mut incoming = PacketRing::new(spec, duration, 3);
    let incoming_spec = AudioSpec::new(2, NonZeroU32::new(RATE * 2).unwrap());
    let packet = |lane, origin, frames, value, source: AudioSpec, advance| {
        let scale = u64::from(source.sample_rate.get() / RATE);
        let raw = origin + 2 * scale * lane;
        let samples = vec![value; usize::try_from(frames * 2).unwrap()];
        let mut packet = chunk(spec, SegmentId::FIRST, lane, raw, &samples);
        packet.meta.media_origin = Some(source.duration_for(origin).unwrap());
        packet.meta.source_span = SourceSpan::new(
            raw,
            raw + advance * scale * frames,
            source.sample_rate,
            frames,
        );
        PcmPacket::Chunk(Box::new(packet))
    };
    let owed = matches!(expected, ContinuationOutcome::Owed);
    let delivered = if owed {
        u64::try_from(BLOCK).unwrap()
    } else {
        4096
    };
    outgoing.push(packet(0, 500, delivered, 0.125, spec, 2));
    let ahead = matches!(expected, ContinuationOutcome::Ahead);
    if ahead {
        incoming.push(packet(640, 2048, 128, 0.375, incoming_spec, 2));
    } else {
        incoming.push(packet(0, 2048, 128, -0.25, incoming_spec, 2));
    }
    let item = TrackId::allocate();
    let to = if replace { A } else { B };
    let mut initial = vec![
        DeckPart::Attach {
            slot: A,
            item_id: item,
            pcm: Box::new(outgoing.receiver.take().unwrap()),
            segment: SegmentId::FIRST,
        },
        DeckPart::Start {
            slot: A,
            fade: Fade::Declick,
        },
    ];
    if owed {
        initial.push(DeckPart::Clock {
            slot: A,
            clock: crate::bridge::TrackClock::Session,
        });
    }
    if !replace {
        initial.push(DeckPart::Attach {
            slot: B,
            item_id: item,
            pcm: Box::new(incoming.receiver.take().unwrap()),
            segment: SegmentId::FIRST,
        });
    }
    let initial_basis = if replace {
        vec![(A, None)]
    } else {
        vec![(A, None), (B, None)]
    };
    let initial = send_on(&mut ends, When::Next, &initial_basis, initial);
    let _ = render(&mut mixer, 0);
    drop(receipts(&mut ends));
    let basis = if replace {
        vec![(A, Some(initial))]
    } else {
        vec![(A, Some(initial)), (B, Some(initial))]
    };
    let mut parts = vec![DeckPart::ContinueSource {
        from: A,
        to,
        window: kithara_signal::FrameCount::new(128),
    }];
    if replace {
        parts.push(DeckPart::Replace {
            slot: A,
            item_id: item,
            pcm: Box::new(incoming.receiver.take().unwrap()),
            segment: SegmentId::FIRST,
        });
    } else {
        parts.push(DeckPart::Fade {
            slot: A,
            dir: FadeDir::Out,
            settings: CrossfadeSettings {
                duration: 0.0,
                ..Default::default()
            },
        });
    }
    parts.push(DeckPart::Start {
        slot: to,
        fade: Fade::Declick,
    });
    let transition = send_on(&mut ends, at(BLOCK), &basis, parts);

    let held = render_frames(&mut mixer, BLOCK, 128);
    let missed = if owed { 0.0 } else { 0.125 };
    assert!(held.iter().all(|sample| (*sample - missed).abs() < 1e-6));
    let premature = receipts(&mut ends);
    assert!(
        premature.is_empty(),
        "the incomplete cut retains its owned batch: {premature:?}"
    );
    assert_eq!(mixer.track(A).unwrap().state(), SlotState::Playing);

    let applied_at = BLOCK + 128;
    if matches!(expected, ContinuationOutcome::Pause) {
        let pause = send_on(
            &mut ends,
            at(applied_at),
            &[(A, Some(initial))],
            vec![DeckPart::Stop {
                slot: A,
                fade: Fade::Declick,
            }],
        );
        let _ = render_frames(&mut mixer, applied_at, 1);
        let replies = receipts(&mut ends);
        assert_applied_at(outcome_of(&replies, pause), applied_at);
        assert!(matches!(
            outcome_of(&replies, transition),
            Outcome::Rejected(Rejection::Stale)
        ));
        assert_eq!(mixer.track(A).unwrap().state(), SlotState::Stopped);
        assert_eq!(mixer.track(B).unwrap().state(), SlotState::Stopped);
        return;
    }
    if matches!(expected, ContinuationOutcome::Closed) {
        drop(incoming);
        let still_playing = render_frames(&mut mixer, applied_at, 1);
        assert!((still_playing[0] - 0.125).abs() < 1e-6);
        let replies = receipts(&mut ends);
        assert!(matches!(
            outcome_of(&replies, transition),
            Outcome::Rejected(Rejection::Refused(DeckRefusal::SourceContinuation { slot }))
                if *slot == to
        ));
        assert_eq!(mixer.track(A).unwrap().state(), SlotState::Playing);
        return;
    }
    if matches!(expected, ContinuationOutcome::Axis) {
        incoming.push(packet(128, 2048, 512, -0.25, incoming_spec, 2));
        incoming.push(packet(640, 2048, 128, 0.375, incoming_spec, 2));
        let _ = render_frames_in(&mut mixer, SessionEpoch::new(1), applied_at, 1);
        let replies = receipts(&mut ends);
        assert!(matches!(
            outcome_of(&replies, transition),
            Outcome::Rejected(Rejection::Refused(DeckRefusal::AxisRestarted))
        ));
        assert_eq!(mixer.track(B).unwrap().state(), SlotState::Stopped);
        return;
    }
    if owed {
        outgoing.push(packet(delivered, 500, 4096, 0.125, spec, 2));
        incoming.push(packet(128, 2048, 512, -0.25, incoming_spec, 2));
        // The cut lands one frame later than the plain cut, so the motion
        // window needs PCM past lane 768.
        incoming.push(packet(640, 2048, 256, 0.375, incoming_spec, 2));
        let repaid = render_frames(&mut mixer, applied_at, 1);
        assert!(
            (repaid[0] - 0.125).abs() < 1e-6,
            "the outgoing slot repays its dropout before the cut: {repaid:?}"
        );
        assert!(
            receipts(&mut ends)
                .iter()
                .all(|(seq, ..)| *seq != transition),
            "the same parked batch waits for the repaid mark"
        );
        let first = render_frames(&mut mixer, applied_at + 1, 1);
        assert!(
            (first[0] - 0.375).abs() < 1e-6,
            "actual first PCM frame: {first:?}"
        );
        let replies = receipts(&mut ends);
        let (_, result, parts) = replies.iter().find(|(seq, ..)| *seq == transition).unwrap();
        assert_applied_at(result, applied_at + 1);
        let mark = parts
            .iter()
            .find_map(|part| match part {
                DeckPart::Returned(Returned::SourceContinued { mark, .. }) => Some(mark),
                _ => None,
            })
            .expect("the applied cut carries the first incoming mark");
        assert_eq!(mark.session, frame_at(applied_at + 1));
        assert_eq!(mark.lane.frame, 641);
        return;
    }
    if matches!(expected, ContinuationOutcome::Hole) {
        incoming.push(packet(129, 2048, 512, -0.25, incoming_spec, 2));
        let still_playing = render_frames(&mut mixer, applied_at, 1);
        assert!((still_playing[0] - 0.125).abs() < 1e-6);
        let replies = receipts(&mut ends);
        assert!(matches!(
            outcome_of(&replies, transition),
            Outcome::Rejected(Rejection::Refused(DeckRefusal::SourceRange { slot: A }))
        ));
        assert_eq!(mixer.track(A).unwrap().state(), SlotState::Playing);
        return;
    }
    if !ahead {
        incoming.push(packet(128, 2048, 512, -0.25, incoming_spec, 2));
    }
    if matches!(expected, ContinuationOutcome::Rate) {
        incoming.push(packet(640, 2048, 64, 0.375, incoming_spec, 2));
        incoming.push(packet(704, 2048, 64, 0.375, incoming_spec, 3));
        let still_playing = render_frames(&mut mixer, applied_at, 1);
        assert!((still_playing[0] - 0.125).abs() < 1e-6);
        let replies = receipts(&mut ends);
        assert!(matches!(
            outcome_of(&replies, transition),
            Outcome::Rejected(Rejection::Refused(DeckRefusal::SourceMotion { slot: A }))
        ));
        assert_eq!(mixer.track(A).unwrap().state(), SlotState::Playing);
        return;
    }
    if !ahead {
        incoming.push(packet(640, 2048, 128, 0.375, incoming_spec, 2));
    }
    let first = render_frames(&mut mixer, applied_at, 2);
    assert!(
        first.iter().all(|sample| (*sample - 0.375).abs() < 1e-6),
        "the continued slot sounds its aligned signal once: {first:?}"
    );
    let replies = receipts(&mut ends);
    let (_, result, parts) = replies.iter().find(|(seq, ..)| *seq == transition).unwrap();
    assert_applied_at(result, applied_at);
    let mark = parts
        .iter()
        .find_map(|part| match part {
            DeckPart::Returned(Returned::SourceContinued {
                to: slot,
                mark,
                media_origin,
            }) => {
                assert_eq!(*slot, to);
                assert_eq!(*media_origin, incoming_spec.duration_for(2048).unwrap());
                Some(mark)
            }
            _ => None,
        })
        .expect("the applied cut carries the first incoming mark");
    assert_eq!(mark.session, frame_at(applied_at));
    assert_eq!(mark.lane.frame, 640);
    assert_eq!(
        mark.position,
        incoming_spec.duration_for(2048 + 4 * 640).unwrap()
    );
}

#[kithara::test]
#[case::start_before_replace(A)]
#[case::replace_another_sounding_slot(B)]
fn a_continuation_rejects_invalid_receiver_ownership(#[case] to: Slot) {
    let (mut mixer, mut ends) = mixer_without_declick();
    let item = kithara_events::TrackId::allocate();
    let pcm = |level| pcm_samples(1.0, &vec![level; BLOCK * 8], false);
    let start = DeckPart::Start {
        slot: to,
        fade: Fade::Declick,
    };
    let mut initial = vec![
        DeckPart::Attach {
            slot: A,
            item_id: item,
            pcm: pcm(0.125),
            segment: SegmentId::FIRST,
        },
        DeckPart::Start {
            slot: A,
            fade: Fade::Declick,
        },
    ];
    let mut basis = vec![(A, None)];
    if to == B {
        initial.extend([
            DeckPart::Attach {
                slot: B,
                item_id: kithara_events::TrackId::allocate(),
                pcm: pcm(0.25),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: B,
                fade: Fade::Declick,
            },
        ]);
        basis.push((B, None));
    }
    let attached = send_on(&mut ends, When::Next, &basis, initial);
    render(&mut mixer, 0);
    drop(receipts(&mut ends));
    for (_, authority) in &mut basis {
        *authority = Some(attached);
    }

    let mut parts = vec![
        DeckPart::ContinueSource {
            from: A,
            to,
            window: kithara_signal::FrameCount::new(128),
        },
        DeckPart::Replace {
            slot: to,
            item_id: item,
            pcm: pcm(-0.25),
            segment: SegmentId::FIRST,
        },
    ];
    if to == A {
        parts.insert(1, start);
    } else {
        parts.extend([
            start,
            DeckPart::Fade {
                slot: A,
                dir: FadeDir::Out,
                settings: CrossfadeSettings {
                    duration: 0.0,
                    ..Default::default()
                },
            },
        ]);
    }
    let transition = send_on(&mut ends, at(BLOCK), &basis, parts);
    let frame = render_frames(&mut mixer, BLOCK, 1);
    let replies = receipts(&mut ends);
    let outcome = outcome_of(&replies, transition);
    assert!(
        matches!(
            outcome,
            Outcome::Rejected(Rejection::Refused(DeckRefusal::SourceContinuation { slot }))
                if *slot == to
        ),
        "invalid continuation changed the deck: {outcome:?}",
    );
    for (slot, _) in basis {
        assert_eq!(mixer.track(slot).unwrap().state(), SlotState::Playing);
    }
    let old = if to == A { 0.125 } else { 0.375 };
    assert!((frame[0] - old).abs() < 1e-6, "outgoing PCM: {frame:?}");
}
