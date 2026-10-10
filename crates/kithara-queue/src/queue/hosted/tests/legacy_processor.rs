use kithara_render::bridge::SlotState;
use num_traits::AsPrimitive;

use super::*;
use crate::{AdvanceReason, Transition};

fn real_outbox<Value>(
    queue: &mut Queue<TestPools>,
    mixer: &mut mock::MixerRig,
    loads: &mut mock::DeckRig<TestPools>,
    at: SessionFrame,
    frames: u32,
    run: impl FnOnce(&mut Queue<TestPools>, &mut Outbox<'_, TestPools>) -> Value,
) -> Value {
    let shape = kithara_render::rt::StreamShape::new(
        NonZeroU32::new(1024).expect("block size"),
        mock::SAMPLE_RATE,
    );
    let output = mock::output(Some(shape)).get();
    let deck = mixer.ends.snapshot.read().clone();
    let pass = DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: at,
        delivery: FrameCount::new(frames as usize),
        output: &output,
        deck: &deck,
    };
    let mut scope = mixer.ring.scope(mixer.scope).expect("real deck scope");
    run(
        queue,
        &mut Outbox::new(&mut scope, &mut loads.dispatcher).in_pass(pass),
    )
}

fn real_block(
    queue: &mut Queue<TestPools>,
    mixer: &mut mock::MixerRig,
    loads: &mut mock::DeckRig<TestPools>,
    at: i64,
    frames: u32,
) -> [Vec<f32>; 2] {
    let at = SessionFrame::new(at);
    let mut pcm = [vec![0.0; frames as usize], vec![0.0; frames as usize]];
    let [left, right] = &mut pcm;
    mixer
        .block_frames(at, [left, right])
        .expect("real mixer callback");
    while let Some(ScopedReceipt::Scope(_, receipt)) = mixer.ring.receipt() {
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        real_outbox(queue, mixer, loads, at, frames, |queue, out| {
            Player::settle(
                queue,
                TrackReceipt::Deck {
                    seq,
                    outcome: &outcome,
                    batch: &mut batch,
                },
                out,
            )
        });
    }
    let events = mixer.ends.events.drain().collect::<Vec<_>>();
    for event in events {
        real_outbox(queue, mixer, loads, at, frames, |queue, out| {
            Player::settle(queue, TrackReceipt::Event(event), out)
        });
    }
    pcm
}

async fn answer_duration(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    dir: &TestTempDir,
    duration: f64,
) {
    let mut loaded = loaded_fixture(rig, dir).await;
    loaded.opened.duration = Some(Duration::from_secs_f64(duration));
    let receipt = rig.open(Ok(loaded)).expect("duration load receipt");
    with_outbox(queue, rig, |queue, out| {
        Player::settle(queue, TrackReceipt::Loaded(receipt), out)
    });
}

fn block(queue: &mut Queue<TestPools>, rig: &mut mock::DeckRig<TestPools>, at: i64) {
    player_internal::settle(queue, rig, SessionFrame::new(at));
}

#[kithara::test(tokio)]
async fn a_fade_in_makes_its_track_leading_and_a_preload_does_not() {
    let (mut queue, _, second, mut rig, dir) = pending_selection();
    answer_duration(&mut queue, &mut rig, &dir, 64.0).await;
    player_internal::finish(&mut queue, &mut rig);
    assert_eq!(queue.control().duration_seconds(), Some(64.0));
    with_outbox(&mut queue, &mut rig, |queue, out| {
        queue.apply_command(
            QueueCommand::Select {
                id: second,
                transition: Transition::None,
            },
            Some(&mock::output(None).get()),
            out,
        )
    })
    .expect("prepare successor");
    answer_duration(&mut queue, &mut rig, &dir, 162.0).await;
    block(&mut queue, &mut rig, 0);
    assert_eq!(
        queue.control().duration_seconds(),
        Some(64.0),
        "preload must not publish the next track duration"
    );
    block(&mut queue, &mut rig, 128);
    block(&mut queue, &mut rig, 256);
    assert_eq!(queue.control().position_seconds(), Some(0.0));
    assert_eq!(queue.control().duration_seconds(), Some(162.0));
}

#[kithara::test(tokio)]
#[case::stitched_in(0.01, Some(SlotState::Playing))]
#[case::still_preloading(60.0, None)]
async fn cancel_preload_unloads_a_successor_only_while_it_preloads(
    #[case] leading_secs: f64,
    #[case] after_cancel: Option<SlotState>,
) {
    let dir = TestTempDir::new();
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build();
    let mut queue = Queue::new(
        QueueConfig::builder()
            .prep(prep)
            .store(
                AssetStore::builder(pools())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .build(),
    );
    queue.clock = Some((SessionFrame::new(0), FrameCount::new(1)));
    queue.deck.mixer.sample_rate = mock::SAMPLE_RATE.get();
    let mut rig = mock::DeckRig::new(DeckMixerConfig::default()).expect("load dispatcher");
    let first = TrackId::allocate();
    let second = TrackId::allocate();
    let shape = kithara_render::rt::StreamShape::new(
        NonZeroU32::new(1024).expect("block size"),
        mock::SAMPLE_RATE,
    );
    let mut mixer =
        mock::MixerRig::new(DeckMixerConfig::default(), shape, &pools()).expect("real deck");
    let frames: usize = (leading_secs * f64::from(mock::SAMPLE_RATE.get()))
        .floor()
        .as_();
    let samples = vec![0.5; frames * 2];
    mock::write_pcm_wav(
        &dir.path().join("entry.wav"),
        &samples,
        AudioSpec::new(2, mock::SAMPLE_RATE),
    )
    .expect("leading WAV");
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(0),
        1,
        |queue, out| {
            for id in [first, second] {
                Player::apply(
                    queue,
                    QueueCommand::Append {
                        id,
                        source: TrackSource::Uri(
                            dir.path()
                                .join("entry.wav")
                                .to_str()
                                .expect("fixture path")
                                .to_owned(),
                        ),
                    },
                    out,
                )
                .expect("append");
            }
            Player::apply(
                queue,
                QueueCommand::Select {
                    id: first,
                    transition: Transition::None,
                },
                out,
            )
            .expect("select leading");
            Player::apply(queue, QueueCommand::Pause { at: When::Next }, out)
                .expect("prepare paused leading");
        },
    );
    let leading = loaded_fixture(&mut rig, &dir).await;
    let receipt = rig.open(Ok(leading)).expect("leading load");
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(0),
        1,
        |queue, out| Player::settle(queue, TrackReceipt::Loaded(receipt), out),
    );
    real_block(&mut queue, &mut mixer, &mut rig, 0, 1);
    real_block(&mut queue, &mut mixer, &mut rig, 1, 1);
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(2),
        1,
        |queue, out| Player::apply(queue, QueueCommand::Play { at: When::Next }, out),
    )
    .expect("start leading");
    real_block(&mut queue, &mut mixer, &mut rig, 2, 1);
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(3),
        1,
        |queue, out| {
            queue.request_transition(
                crate::queue::transition::TransitionRequest {
                    id: second,
                    transition: Transition::None,
                    reason: AdvanceReason::NaturalEof,
                    auto: true,
                    playing: true,
                },
                Some(&mock::output(Some(shape)).get()),
                out,
            )
        },
    )
    .expect("gapless successor");
    mock::write_pcm_wav(
        &dir.path().join("entry.wav"),
        &vec![0.5; 8192],
        AudioSpec::new(2, mock::SAMPLE_RATE),
    )
    .expect("successor WAV");
    let successor = loaded_fixture(&mut rig, &dir).await;
    let receipt = rig.open(Ok(successor)).expect("successor load");
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(3),
        1,
        |queue, out| Player::settle(queue, TrackReceipt::Loaded(receipt), out),
    );
    real_block(&mut queue, &mut mixer, &mut rig, 3, 1);
    real_block(&mut queue, &mut mixer, &mut rig, 4, 1);
    let pcm = real_block(&mut queue, &mut mixer, &mut rig, 5, 1024);
    let rendered = pcm.iter().flatten().any(|sample| *sample != 0.0);
    assert!(rendered, "the leading track renders");
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(1029),
        1,
        Queue::cancel_target,
    )
    .expect("withdraw target");
    real_block(&mut queue, &mut mixer, &mut rig, 1029, 1);
    assert_eq!(
        queue
            .active
            .iter()
            .find(|active| active.item == second)
            .filter(|active| active.role != Role::Leaving)
            .map(|active| {
                if matches!(
                    active.track.snapshot().status,
                    PlayingStatus::Playing { .. }
                ) {
                    SlotState::Playing
                } else {
                    SlotState::Stopped
                }
            }),
        after_cancel
    );
    assert_eq!(
        queue.current,
        if after_cancel.is_some() {
            Some(second)
        } else {
            Some(first)
        }
    );
}
