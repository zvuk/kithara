pub(super) use firewheel::{
    clock::InstantSamples,
    mask::{ConnectedMask, ConstantMask, SilenceMask},
    node::{ProcStore, StreamStatus},
};
pub(super) use kithara_command::{Batch, Outcome, Port, Rejection, When};
pub(super) use kithara_effects::{
    GainDb,
    eq::{EqBandConfig, EqConfig, EqLayout},
};
pub(super) use kithara_platform::{sync::Arc, time::Duration};
pub(super) use kithara_signal::{
    AudioSpec, OutputContext, SessionEpoch, SessionFrame, TransportRevision,
};
pub(super) use kithara_test_fixtures::integration_fixtures::constant_half;
pub(super) use kithara_warp::RenderContext;
pub(super) use num_traits::{ToPrimitive, cast};

use super::*;
pub(super) use crate::{
    CrossfadeCurve, CrossfadeSettings,
    bridge::{DeckEqChange, DeckPart, DeckRefusal, Fade, FadeDir, Returned},
    rt::{
        DeckMixerConfig,
        track::{PcmConsumer, PlayerResource},
    },
    test_pools::pools,
    worker::{
        PcmPacket,
        packet_tests::{PacketRing, chunk},
    },
};

pub(super) const BLOCK: usize = 512;
pub(super) const RATE: u32 = 44_100;
pub(super) const LEVEL: f32 = 0.5;
pub(super) const A: Slot = Slot::new(0);
pub(super) const B: Slot = Slot::new(1);

pub(super) use crate::mock::DeckReply;

#[kithara::test]
fn application_deadline_is_optional_but_explicit_geometry_is_enforced() {
    let shape = StreamShape::new(
        NonZeroU32::new(512).expect("fixture block"),
        NonZeroU32::new(48_000).expect("fixture rate"),
    );
    let quantum = NonZeroUsize::new(32).expect("fixture quantum");
    let (preload, ring) = shape
        .playback_buffers(quantum, None)
        .expect("unbounded deadline");
    assert_eq!((preload.get(), ring.get()), (16, 17));
    assert!(matches!(
        shape.playback_buffers(quantum, NonZeroUsize::new(448)),
        Err(BufferGeometryError::BudgetExceeded {
            required_frames: 575,
            max_block_frames: 512,
            render_quantum_frames: 32,
            budget_frames: 448,
        })
    ));
}

pub(super) use crate::mock::{TestEnds, TestMixer, mixer, shape};

pub(super) fn mixer_without_declick() -> (TestMixer, TestEnds) {
    mixer_with_config(
        DeckMixerConfig::builder()
            .declick(SmootherConfig {
                smooth_seconds: 0.0,
                ..SmootherConfig::default()
            })
            .build(),
        64,
    )
}

#[kithara::test]
fn a_constructed_deck_publishes_its_shape_before_any_rendered_block() {
    let (_mixer, mut ends) = mixer();
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.sample_rate, RATE);
    assert_eq!(snapshot.blocks, 0);
    assert!(
        snapshot
            .slots
            .iter()
            .all(|slot| slot.state == SlotState::Empty)
    );
}

#[kithara::test]
fn an_empty_deck_counts_every_block_it_renders() {
    let (mut mixer, mut ends) = mixer();
    for start in [0, 128, 256] {
        render(&mut mixer, start);
    }
    assert_eq!(ends.deck.snapshot.read().blocks, 3);
}

pub(super) use crate::mock::{mixer_with_config, mixer_with_shape};

pub(super) fn proc_info() -> ProcInfo {
    ProcInfo {
        sample_rate: NonZeroU32::new(RATE).expect("static sample rate"),
        frames: 512,
        in_silence_mask: SilenceMask::default(),
        out_silence_mask: SilenceMask::default(),
        in_constant_mask: ConstantMask::default(),
        out_constant_mask: ConstantMask::default(),
        in_connected_mask: ConnectedMask::default(),
        out_connected_mask: ConnectedMask::default(),
        total_cpu_seconds_recip: 1.0,
        process_to_playback_delay: None,
        did_just_unbypass: false,
        last_marker_instant: InstantSamples(0),
        sample_rate_recip: f64::from(RATE).recip(),
        clock_samples: InstantSamples(0),
        duration_since_stream_start: Duration::ZERO,
        stream_status: StreamStatus::empty(),
        dropped_frames: 0,
    }
}

#[kithara::test]
fn session_processors_read_the_same_host_context() {
    let mut store = ProcStore::with_capacity(1);
    super::super::super::install_render_context(&mut store)
        .expect("invariant: fixture installs one context slot");
    super::super::super::publish_render_context(
        &mut store,
        RenderContext::new_linear(
            OutputContext::new(
                SessionFrame::new(0)..SessionFrame::new(512),
                NonZeroU32::new(RATE).expect("static sample rate"),
                SessionEpoch::new(3),
                Some(TransportRevision::first()),
            )
            .expect("invariant: fixture output range is ordered"),
            None,
        )
        .expect("invariant: fixture context is valid"),
    )
    .expect("invariant: fixture context slot exists");
    let info = proc_info();
    let left = read_render_context(&store, &info).expect("session context");
    let right = read_render_context(&store, &info).expect("session context");

    assert!(std::ptr::eq(left, right));
    assert_eq!(left.output().session_epoch(), SessionEpoch::new(3));
    assert_eq!(
        left.output().transport_revision(),
        Some(TransportRevision::first())
    );
}

pub(super) fn pcm(constant_half: &'static [u8], src: &str, seconds: f64) -> Box<PlayerResource> {
    let total = (seconds * f64::from(RATE))
        .floor()
        .to_usize()
        .expect("fixture frame count fits usize");
    let frames = total.min(4096);
    let samples = constant_half
        .as_chunks::<4>()
        .0
        .iter()
        .take(frames * 2)
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect::<Vec<_>>();
    pcm_samples(src, seconds, &samples, frames == total)
}

pub(super) use crate::mock::{pcm_samples, send, send_on};

pub(super) const fn at(frame: usize) -> When<SessionFrame> {
    When::At(frame_at(frame))
}

pub(super) const fn frame_at(frame: usize) -> SessionFrame {
    SessionFrame::new(frame as i64)
}

/// Renders the block starting at session frame `start` and answers its left channel.
pub(super) fn render(mixer: &mut TestMixer, start: usize) -> Vec<f32> {
    render_frames(mixer, start, BLOCK)
}

pub(super) fn render_frames(mixer: &mut TestMixer, start: usize, frames: usize) -> Vec<f32> {
    let mut left = vec![0.0f32; frames];
    let mut right = vec![0.0f32; frames];
    let inputs: [&[f32]; 0] = [];
    let mut outputs = [&mut left[..], &mut right[..]];
    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    mixer.inbox.0.drain();
    let level = mixer.inbox.scope(mixer.mixer.scope).expect("live scope");
    let context = RenderContext::new_linear(
        OutputContext::new(
            frame_at(start)..frame_at(start + frames),
            shape().sample_rate,
            SessionEpoch::new(0),
            None,
        )
        .expect("output range"),
        None,
    )
    .expect("context");
    mixer.mixer.render_block(
        Some(level),
        Some(&context),
        frame_at(start),
        &mut buffers,
        frames,
    );
    left
}

pub(super) use crate::mock::{assert_applied_at, outcome_of, receipts};

pub(super) fn eq_layout(gains: &[GainDb]) -> Box<EqLayout> {
    let bands: Vec<_> = gains
        .iter()
        .map(|gain| EqBandConfig::builder().gain_db(*gain).build())
        .collect();
    Box::new(
        EqLayout::new(
            &EqConfig::builder(pools()).build(),
            &bands,
            NonZeroU32::new(RATE).expect("static sample rate"),
        )
        .expect("an EQ layout fits the test pool budget"),
    )
}

pub(super) fn start_dc(mixer: &mut TestMixer, ends: &mut TestEnds, slots: &[Slot]) {
    let mut parts = Vec::new();
    for slot in slots {
        parts.push(DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: *slot,
            pcm: pcm_samples("dc", 1.0, &vec![1.0; 8192], false),
            segment: SegmentId::FIRST,
        });
        parts.push(DeckPart::Start {
            slot: *slot,
            fade: Fade::Crossfade(
                CrossfadeSettings::new(0.0, CrossfadeCurve::Linear, 1.0, 0.5)
                    .expect("instant start"),
            ),
        });
    }
    send(ends, When::Next, parts);
    render(mixer, 0);
    assert_eq!(receipts(ends).len(), 1);
}

pub(super) fn assert_stopped_once(
    replies: &[DeckReply],
    seq: Seq,
    committed_at: usize,
    slot: Slot,
    interrupt_at: usize,
) {
    let matches = replies
        .iter()
        .filter(|(answered, ..)| *answered == seq)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "every committed batch has exactly one verdict"
    );
    let (_, outcome, parts) = matches[0];
    assert_applied_at(outcome, committed_at);
    let marks = parts
        .iter()
        .filter_map(|part| match part {
            DeckPart::Returned(Returned::Stopped {
                slot: stopped,
                resume,
            }) if *stopped == slot => Some(*resume),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        marks,
        [Some(crate::bridge::SlotMark {
            session: frame_at(interrupt_at),
            lane: crate::LaneFrame {
                segment: SegmentId::FIRST,
                frame: interrupt_at as u64
            },
            position: AudioSpec::new(2, shape().sample_rate)
                .duration_for(interrupt_at as u64)
                .expect("position"),
        })]
    );
}

#[kithara::test]
#[case::stop_then_start(false)]
#[case::stop_then_replace(true)]
fn an_interrupted_stop_still_answers_its_batch(#[case] replace: bool) {
    let (mut mixer, mut ends) = mixer();
    start_dc(&mut mixer, &mut ends, &[A]);
    let settings =
        CrossfadeSettings::new(2.0, CrossfadeCurve::Linear, 1.0, 0.5).expect("long ramp");
    let stop = send(
        &mut ends,
        at(BLOCK),
        vec![DeckPart::Stop {
            slot: A,
            fade: Fade::Crossfade(settings),
        }],
    );
    let interrupted_at = BLOCK + 32;
    let interrupt = send(
        &mut ends,
        at(interrupted_at),
        vec![if replace {
            DeckPart::Replace {
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm_samples("replacement", 1.0, &vec![1.0; 8192], false),
                segment: SegmentId::FIRST,
            }
        } else {
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            }
        }],
    );
    render(&mut mixer, BLOCK);
    let replies = receipts(&mut ends);
    assert_stopped_once(&replies, stop, BLOCK, A, interrupted_at);
    assert_eq!(
        replies.iter().filter(|(seq, ..)| *seq == interrupt).count(),
        1
    );
    assert_applied_at(outcome_of(&replies, interrupt), interrupted_at);
    render(&mut mixer, 2 * BLOCK);
    assert!(
        receipts(&mut ends).is_empty(),
        "neither batch is answered twice"
    );
}

#[kithara::test]
fn a_stop_followed_by_start_in_one_batch_returns_its_interrupt_mark() {
    let (mut mixer, mut ends) = mixer();
    start_dc(&mut mixer, &mut ends, &[A]);
    let seq = send(
        &mut ends,
        at(BLOCK),
        vec![
            DeckPart::Stop {
                slot: A,
                fade: Fade::Declick,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Declick,
            },
        ],
    );
    render(&mut mixer, BLOCK);
    assert_stopped_once(&receipts(&mut ends), seq, BLOCK, A, BLOCK);
    render(&mut mixer, 2 * BLOCK);
    assert!(receipts(&mut ends).is_empty(), "one verdict per batch");
}

#[kithara::test]
fn a_multislot_stop_completes_when_one_slot_is_interrupted() {
    let (mut mixer, mut ends) = mixer();
    start_dc(&mut mixer, &mut ends, &[A, B]);
    let seq = send(
        &mut ends,
        at(BLOCK),
        vec![
            DeckPart::Stop {
                slot: A,
                fade: Fade::Declick,
            },
            DeckPart::Stop {
                slot: B,
                fade: Fade::Declick,
            },
        ],
    );
    send(
        &mut ends,
        at(BLOCK + 32),
        vec![DeckPart::Start {
            slot: A,
            fade: Fade::Declick,
        }],
    );
    render(&mut mixer, BLOCK);
    let replies = receipts(&mut ends);
    assert_stopped_once(&replies, seq, BLOCK, A, BLOCK + 32);
    assert_stopped_once(&replies, seq, BLOCK, B, BLOCK + 221);
    render(&mut mixer, 2 * BLOCK);
    assert!(receipts(&mut ends).is_empty());
}

#[kithara::test]
fn interrupted_stops_release_credit_beyond_the_deck_capacity() {
    const CAPACITY: usize = 4;
    let (mut mixer, mut ends) = mixer_with_config(DeckMixerConfig::default(), CAPACITY);
    start_dc(&mut mixer, &mut ends, &[A]);
    let settings =
        CrossfadeSettings::new(2.0, CrossfadeCurve::Linear, 1.0, 0.5).expect("long ramp");
    for iteration in 0..2 * CAPACITY {
        let frame = BLOCK + iteration * 32;
        let scope = ends.deck.scope;
        for (at, command) in [
            (
                frame,
                DeckPart::Stop {
                    slot: A,
                    fade: Fade::Crossfade(settings),
                },
            ),
            (
                frame + 16,
                DeckPart::Start {
                    slot: A,
                    fade: Fade::Declick,
                },
            ),
        ] {
            let sent = ends.ring.scope(scope).expect("scope").send(
                self::at(at),
                Batch {
                    basis: Vec::new(),
                    commands: vec![command],
                },
            );
            assert!(
                sent.is_ok(),
                "interrupt {iteration} at {at} must not leak credit or return Full: {sent:?}"
            );
        }
        ends.ring.publish().expect("publish");
        render_frames(&mut mixer, frame, 32);
        drop(receipts(&mut ends));
    }
}

#[kithara::test]
#[case::samples(false)]
#[case::mark(true)]
fn adopt_never_mixes_a_blocked_older_packet_or_publishes_its_mark(#[case] check_mark: bool) {
    let config = DeckMixerConfig::builder()
        .recycle_per_block(NonZeroUsize::new(1).expect("one recycle per block"))
        .build();
    let (mut mixer, mut ends) = mixer_with_config(config, 8);
    let spec = AudioSpec::new(2, shape().sample_rate);
    let old = SegmentId::FIRST;
    let current = old.next();
    let mut packets = PacketRing::new(spec, Duration::from_secs(1), 2);
    let mut receiver = packets.receiver.take().expect("receiver");
    for _ in 0..2 {
        receiver
            .recycle(PcmPacket::Chunk(Box::new(chunk(
                spec, old, 0, 0, &[1.0; 2],
            ))))
            .expect("fill reverse ring");
    }
    assert!(
        receiver
            .recycle(PcmPacket::Chunk(Box::new(chunk(
                spec, old, 0, 0, &[1.0; 2]
            ))))
            .is_err()
    );
    packets.push(PcmPacket::Chunk(Box::new(chunk(
        spec, old, 0, 0, &[1.0; 32],
    ))));
    let resource = Box::new(
        PlayerResource::new(PcmConsumer::new(receiver), Arc::from("segments"), &pools())
            .expect("resource"),
    );
    let instant = Fade::Crossfade(
        CrossfadeSettings::new(0.0, CrossfadeCurve::Linear, 1.0, 0.5).expect("instant envelope"),
    );
    send(
        &mut ends,
        at(0),
        vec![
            DeckPart::Attach {
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: resource,
                segment: old,
            },
            DeckPart::Start {
                slot: A,
                fade: instant,
            },
        ],
    );
    assert_eq!(render_frames(&mut mixer, 0, 1), [1.0]);
    drop(receipts(&mut ends));
    packets.push(PcmPacket::Chunk(Box::new(chunk(
        spec, current, 100, 1000, &[2.0; 16],
    ))));
    packets.push(PcmPacket::Chunk(Box::new(chunk(
        spec, current, 108, 1008, &[2.0; 16],
    ))));
    let adopt = send(
        &mut ends,
        at(1),
        vec![
            DeckPart::Stop {
                slot: A,
                fade: instant,
            },
            DeckPart::Adopt {
                slot: A,
                segment: current,
            },
            DeckPart::Start {
                slot: A,
                fade: instant,
            },
        ],
    );
    let mixed = render_frames(&mut mixer, 1, 4);
    assert_applied_at(outcome_of(&receipts(&mut ends), adopt), 1);
    if check_mark {
        assert_eq!(
            mixer.track(A).expect("track").mark(frame_at(5)),
            None,
            "the old held packet cannot map the adopted slot"
        );
    } else {
        assert_eq!(mixed, [0.0; 4], "no old packet reaches the mix after Adopt");
    }
    while packets.returned().is_some() {}
    assert_eq!(render_frames(&mut mixer, 5, 4), [2.0; 4]);
    assert_eq!(
        mixer.track(A).expect("track").mark(frame_at(9)),
        Some(crate::bridge::SlotMark {
            session: frame_at(9),
            lane: crate::LaneFrame {
                segment: current,
                frame: 104
            },
            position: spec.duration_for(1004).expect("new source position"),
        })
    );
}

#[kithara::test]
#[case::detach(false)]
#[case::replace(true)]
fn removing_a_slot_while_its_tail_sounds_preserves_gain_continuity(#[case] replace: bool) {
    let (mut mixer, mut ends) = mixer();
    start_dc(&mut mixer, &mut ends, &[A]);
    send(
        &mut ends,
        at(BLOCK),
        vec![DeckPart::Replace {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: pcm_samples("second dc", 1.0, &vec![1.0; 8192], false),
            segment: SegmentId::FIRST,
        }],
    );
    const CUT: usize = 100;
    send(
        &mut ends,
        at(BLOCK + CUT),
        vec![if replace {
            DeckPart::Replace {
                item_id: kithara_events::TrackId::allocate(),
                slot: A,
                pcm: pcm_samples("silence", 1.0, &vec![0.0; 8192], false),
                segment: SegmentId::FIRST,
            }
        } else {
            DeckPart::Detach { slot: A }
        }],
    );
    let left = render(&mut mixer, BLOCK);
    assert!(
        left[CUT - 1] > 1.1,
        "both the existing tail and the new DC are sounding before removal"
    );
    let step = left[CUT - 1..CUT + 8]
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        step < 0.02,
        "a sounding tail must not hard-cut its outgoing full-scale DC: step {step}"
    );
}
