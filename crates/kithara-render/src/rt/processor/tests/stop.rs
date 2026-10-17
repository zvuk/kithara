use super::*;

fn instant() -> Fade {
    Fade::Crossfade(
        CrossfadeSettings::new(0.0, CrossfadeCurve::Linear, 1.0, 0.5).expect("instant envelope"),
    )
}

fn fade_frames(frames: usize) -> CrossfadeSettings {
    CrossfadeSettings::new(
        frames.to_f32().expect("small frame count") / RATE.to_f32().expect("rate"),
        CrossfadeCurve::Linear,
        1.0,
        0.5,
    )
    .expect("frame-based fixture fade")
}

fn empty_slot(mixer: &mut TestMixer, ends: &mut TestEnds, playing: bool) -> PacketRing {
    let spec = AudioSpec::new(2, shape().sample_rate);
    let mut packets = PacketRing::new(spec, Duration::from_secs(1), 2);
    let resource = Box::new(
        PlayerResource::new(
            PcmConsumer::new(packets.receiver.take().expect("receiver")),
            Arc::from("empty live lane"),
            &pools(),
        )
        .expect("resource"),
    );
    let mut commands = vec![DeckPart::Attach {
        item_id: kithara_events::TrackId::allocate(),
        slot: A,
        pcm: resource,
        segment: SegmentId::FIRST,
    }];
    if playing {
        commands.push(DeckPart::Start {
            slot: A,
            fade: instant(),
        });
    }
    send(ends, at(0), commands);
    assert_eq!(render_frames(mixer, 0, 1), [0.0]);
    assert_eq!(receipts(ends).len(), 1);
    assert_eq!(mixer.track(A).expect("attached").mark(frame_at(1)), None);
    packets
}

fn assert_stop_answer(replies: &[DeckReply], seq: Seq, at: usize, slot: Slot) {
    let answers = replies
        .iter()
        .filter(|(answered, ..)| *answered == seq)
        .collect::<Vec<_>>();
    assert_eq!(
        answers.len(),
        1,
        "one terminal answer for the original Stop"
    );
    assert_applied_at(&answers[0].1, at);
    assert_eq!(
        answers[0]
            .2
            .iter()
            .filter(|part| {
                matches!(part, DeckPart::Returned(Returned::Stopped { slot: stopped, .. }) if *stopped == slot)
            })
            .count(),
        1,
        "the Stop answers through its own returned part"
    );
}

fn assert_markless_stop_answer(replies: &[DeckReply], seq: Seq, at: usize, slot: Slot) {
    assert_stop_answer(replies, seq, at, slot);
    let (_, _, parts) = replies
        .iter()
        .find(|(answered, ..)| *answered == seq)
        .expect("Stop answer");
    assert!(parts.iter().any(|part| matches!(part,
        DeckPart::Returned(Returned::Stopped { slot: stopped, resume: None }) if *stopped == slot
    )));
}

#[kithara::test]
#[case::fade_in(FadeDir::In)]
#[case::fade_out(FadeDir::Out)]
fn a_stop_keeps_its_original_deadline_when_an_ordinary_fade_replaces_its_envelope(
    #[case] direction: FadeDir,
) {
    const DEADLINE: usize = BLOCK + 64;
    let (mut mixer, mut ends) = mixer();
    start_dc(&mut mixer, &mut ends, &[A]);
    let stop = send(
        &mut ends,
        at(BLOCK),
        vec![DeckPart::Stop {
            slot: A,
            fade: Fade::Crossfade(fade_frames(64)),
        }],
    );
    let envelope = send(
        &mut ends,
        at(BLOCK + 16),
        vec![DeckPart::Fade {
            slot: A,
            settings: fade_frames(128),
            dir: direction,
        }],
    );
    render_frames(&mut mixer, BLOCK, 63);
    let earlier = receipts(&mut ends);
    assert_applied_at(outcome_of(&earlier, envelope), BLOCK + 16);
    assert!(earlier.iter().all(|(seq, ..)| *seq != stop));
    assert_eq!(mixer.track(A).expect("track").state(), SlotState::Playing);
    render_frames(&mut mixer, DEADLINE - 1, 1);
    assert_stopped_once(&receipts(&mut ends), stop, BLOCK, A, DEADLINE);
    assert_eq!(mixer.track(A).expect("track").state(), SlotState::Stopped);
    assert_eq!(render_frames(&mut mixer, DEADLINE, 32), [0.0; 32]);
    assert!(
        receipts(&mut ends).is_empty(),
        "Stop credit is returned once"
    );
}

#[kithara::test]
#[case::stopped(false)]
#[case::starved(true)]
fn a_stop_without_a_mark_still_returns_its_original_sequence(#[case] playing: bool) {
    let (mut mixer, mut ends) = mixer();
    let _packets = empty_slot(&mut mixer, &mut ends, playing);
    let stop = send(
        &mut ends,
        at(1),
        vec![DeckPart::Stop {
            slot: A,
            fade: Fade::Crossfade(fade_frames(16)),
        }],
    );
    render_frames(&mut mixer, 1, 16);
    assert_markless_stop_answer(&receipts(&mut ends), stop, 1, A);
    assert_eq!(mixer.track(A).expect("track").mark(frame_at(17)), None);
    assert_eq!(mixer.track(A).expect("track").state(), SlotState::Stopped);
    render_frames(&mut mixer, 17, 1);
    assert!(receipts(&mut ends).is_empty());
}

#[kithara::test]
#[case::start(0)]
#[case::detach(1)]
#[case::replace(2)]
#[case::adopt(3)]
fn an_interrupted_stop_without_a_mark_keeps_its_sequence_and_custody(#[case] change: u8) {
    let (mut mixer, mut ends) = mixer();
    let _packets = empty_slot(&mut mixer, &mut ends, true);
    let stop = send(
        &mut ends,
        at(1),
        vec![DeckPart::Stop {
            slot: A,
            fade: Fade::Crossfade(fade_frames(128)),
        }],
    );
    let part = match change {
        0 => DeckPart::Start {
            slot: A,
            fade: instant(),
        },
        1 => DeckPart::Detach { slot: A },
        2 => DeckPart::Replace {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: pcm_samples("new lane", 1.0, &[1.0; 128], false),
            segment: SegmentId::FIRST,
        },
        3 => DeckPart::Adopt {
            slot: A,
            segment: SegmentId::FIRST.next(),
        },
        _ => unreachable!(),
    };
    let changed = send(&mut ends, at(17), vec![part]);
    render_frames(&mut mixer, 1, 32);
    let replies = receipts(&mut ends);
    assert_markless_stop_answer(&replies, stop, 1, A);
    assert_applied_at(outcome_of(&replies, changed), 17);
    if matches!(change, 1 | 2) {
        let (_, _, parts) = replies
            .iter()
            .find(|(seq, ..)| *seq == changed)
            .expect("change");
        assert!(
            matches!(parts.as_slice(), [DeckPart::Returned(Returned::Pcm { slot, pcm })]
            if *slot == A && &**pcm.src() == "empty live lane")
        );
    }
    render_frames(&mut mixer, 33, 1);
    assert!(
        receipts(&mut ends).is_empty(),
        "interruption cannot answer twice"
    );
}

fn render_without_context(mixer: &mut TestMixer, start: usize, frames: usize) {
    let mut left = vec![0.0; frames];
    let mut right = vec![0.0; frames];
    let mut outputs = [&mut left[..], &mut right[..]];
    let mut buffers = ProcBuffers {
        inputs: &[],
        outputs: &mut outputs,
    };
    mixer.inbox.0.drain();
    let level = mixer.inbox.scope(mixer.mixer.scope).expect("live scope");
    mixer
        .mixer
        .render_block(Some(level), None, frame_at(start), &mut buffers, frames);
    assert!(left.iter().all(|sample| *sample == 0.0));
}

#[kithara::test]
#[case::starved(0)]
#[case::gate_shut(1)]
#[case::no_context(2)]
fn a_fade_advances_on_session_time_without_consumed_pcm(#[case] unavailable: u8) {
    let (mut mixer, mut ends) = mixer();
    let _packets = if unavailable == 0 {
        Some(empty_slot(&mut mixer, &mut ends, true))
    } else {
        start_dc(&mut mixer, &mut ends, &[A]);
        None
    };
    let start = if unavailable == 0 { 1 } else { BLOCK };
    if unavailable == 1 {
        mixer.mixer.deck.tracks.at_mut(A).expect("track").shut();
    }
    let fade = send(
        &mut ends,
        at(start),
        vec![DeckPart::Fade {
            slot: A,
            settings: fade_frames(64),
            dir: FadeDir::Out,
        }],
    );
    if unavailable == 2 {
        render_without_context(&mut mixer, start, 63);
    } else {
        render_frames(&mut mixer, start, 63);
    }
    assert_applied_at(outcome_of(&receipts(&mut ends), fade), start);
    assert!(
        ends.events
            .drain()
            .all(|event| !matches!(event, DeckEvent::Faded { .. }))
    );
    if unavailable == 2 {
        render_without_context(&mut mixer, start + 63, 1);
    } else {
        render_frames(&mut mixer, start + 63, 1);
    }
    assert_eq!(
        ends.events.drain().collect::<Vec<_>>(),
        [DeckEvent::Faded {
            slot: A,
            at: frame_at(start + 64),
        }]
    );
    assert_eq!(mixer.track(A).expect("track").state(), SlotState::Stopped);
    render_frames(&mut mixer, start + 64, 1);
    assert!(ends.events.drain().next().is_none());
}

#[kithara::test]
fn a_shorter_fade_does_not_move_the_pending_stop_deadline_or_invent_source_frames() {
    let (mut mixer, mut ends) = mixer();
    start_dc(&mut mixer, &mut ends, &[A]);
    let stop = send(
        &mut ends,
        at(BLOCK),
        vec![DeckPart::Stop {
            slot: A,
            fade: Fade::Crossfade(fade_frames(64)),
        }],
    );
    send(
        &mut ends,
        at(BLOCK + 16),
        vec![DeckPart::Fade {
            slot: A,
            settings: fade_frames(16),
            dir: FadeDir::Out,
        }],
    );
    render_frames(&mut mixer, BLOCK, 63);
    assert!(receipts(&mut ends).iter().all(|(seq, ..)| *seq != stop));
    assert_eq!(mixer.track(A).expect("track").state(), SlotState::Stopped);
    render_frames(&mut mixer, BLOCK + 63, 1);
    let replies = receipts(&mut ends);
    assert_stop_answer(&replies, stop, BLOCK, A);
    let (_, _, parts) = replies.iter().find(|(seq, ..)| *seq == stop).expect("Stop");
    assert!(parts.iter().any(|part| matches!(part,
        DeckPart::Returned(Returned::Stopped { slot: A, resume: Some(mark) })
            if mark.session == frame_at(BLOCK + 64)
                && mark.lane.frame == (BLOCK + 32) as u64
                && mark.position == AudioSpec::new(2, shape().sample_rate)
                    .duration_for((BLOCK + 32) as u64).expect("consumed frontier")
    )));
}

#[kithara::test]
fn a_markless_stop_and_restart_in_the_same_batch_answers_the_stop_once() {
    let (mut mixer, mut ends) = mixer();
    let _packets = empty_slot(&mut mixer, &mut ends, true);
    let seq = send(
        &mut ends,
        at(1),
        vec![
            DeckPart::Stop {
                slot: A,
                fade: Fade::Crossfade(fade_frames(64)),
            },
            DeckPart::Start {
                slot: A,
                fade: instant(),
            },
        ],
    );
    render_frames(&mut mixer, 1, 1);
    assert_markless_stop_answer(&receipts(&mut ends), seq, 1, A);
    assert_eq!(mixer.track(A).expect("track").state(), SlotState::Playing);
    render_frames(&mut mixer, 2, 64);
    assert!(receipts(&mut ends).is_empty());
}

#[kithara::test]
fn a_multislot_stop_waits_for_the_other_slot_after_markless_completion() {
    let (mut mixer, mut ends) = mixer();
    let _packets = empty_slot(&mut mixer, &mut ends, true);
    send(
        &mut ends,
        at(1),
        vec![
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: B,
                pcm: pcm_samples("mapped lane", 1.0, &[1.0; 2 * BLOCK + 256], false),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: B,
                fade: instant(),
            },
        ],
    );
    render_frames(&mut mixer, 1, BLOCK);
    drop(receipts(&mut ends));
    let start = BLOCK + 1;
    let seq = send(
        &mut ends,
        at(start),
        vec![
            DeckPart::Stop {
                slot: A,
                fade: Fade::Crossfade(fade_frames(16)),
            },
            DeckPart::Stop {
                slot: B,
                fade: Fade::Crossfade(fade_frames(64)),
            },
        ],
    );
    render_frames(&mut mixer, start, 63);
    assert!(
        receipts(&mut ends).is_empty(),
        "the whole Stop batch waits for B"
    );
    assert_eq!(mixer.track(A).expect("track").state(), SlotState::Stopped);
    assert_eq!(mixer.track(B).expect("track").state(), SlotState::Playing);
    render_frames(&mut mixer, start + 63, 1);
    let replies = receipts(&mut ends);
    assert_markless_stop_answer(&replies, seq, start, A);
    assert_stop_answer(&replies, seq, start, B);
    render_frames(&mut mixer, start + 64, 1);
    assert!(receipts(&mut ends).is_empty());
}

#[kithara::test]
fn an_ordinary_fade_finishes_on_session_time_after_early_eof() {
    const EOF: usize = BLOCK + 16;
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        at(0),
        vec![
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm_samples("short lane", 1.0, &vec![1.0; 2 * EOF], true),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: instant(),
            },
        ],
    );
    render(&mut mixer, 0);
    drop(receipts(&mut ends));
    send(
        &mut ends,
        at(BLOCK),
        vec![DeckPart::Fade {
            slot: A,
            settings: fade_frames(64),
            dir: FadeDir::Out,
        }],
    );
    render_frames(&mut mixer, BLOCK, 63);
    assert_eq!(
        ends.events.drain().collect::<Vec<_>>(),
        [DeckEvent::Ended {
            slot: A,
            at: frame_at(EOF),
        }]
    );
    render_frames(&mut mixer, BLOCK + 63, 1);
    assert_eq!(
        ends.events.drain().collect::<Vec<_>>(),
        [DeckEvent::Faded {
            slot: A,
            at: frame_at(BLOCK + 64),
        }]
    );
    render_frames(&mut mixer, BLOCK + 64, 1);
    assert!(ends.events.drain().next().is_none());
}
