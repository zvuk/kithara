use std::num::NonZeroU32;

use kithara_platform::time::Duration;
use kithara_play::{
    DeckMixerConfig, DeckPass, EqBandConfig, HostedDeck, Outbox, TrackReceipt, mock,
};
use kithara_render::rt::StreamShape;
use kithara_signal::{FrameCount, SessionFrame};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use crate::{
    Queue, QueueConfig, QueueControl, QueueError,
    mock::{TestPools, answer_loaded, finish, pending_selection, pools},
};

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
fn drive_call(
    queue: &mut Queue<TestPools>,
    control: QueueControl<TestPools>,
    call: impl FnOnce(QueueControl<TestPools>) -> Result<(), QueueError> + Send + 'static,
    mut drain: impl FnMut(&mut Queue<TestPools>),
) -> Result<(), QueueError> {
    let _real_io = kithara_platform::flash::real_io();
    let caller = std::thread::spawn(move || call(control));
    while !caller.is_finished() {
        hang_tick!();
        drain(queue);
        hang_park!(|remaining: Duration| {
            std::thread::park_timeout(remaining.min(Duration::from_millis(1)));
        });
    }
    hang_reset!();
    caller.join().expect("control caller")
}

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
fn eq_call(
    queue: &mut Queue<TestPools>,
    mixer: &mut mock::MixerRig,
    loads: &mut mock::DeckRig<TestPools>,
    control: QueueControl<TestPools>,
    call: impl FnOnce(QueueControl<TestPools>) -> Result<(), QueueError> + Send + 'static,
) {
    drive_call(queue, control, call, |queue| {
        let output = mock::output(None).get();
        let deck = mixer.ends.snapshot.read();
        let pass = DeckPass {
            mix: queue.deck.mix,
            suspended: false,
            now: SessionFrame::new(0),
            delivery: FrameCount::new(128),
            output: &output,
            deck,
        };
        let mut scope = mixer.ring.scope(mixer.scope).expect("mixer scope");
        let mut out = Outbox::new(&mut scope, &mut loads.dispatcher).in_pass(pass);
        HostedDeck::drain(queue, pass, &mut out);
    })
    .expect("EQ command accepted");
    let mut left = [0.0; 128];
    let mut right = [0.0; 128];
    mixer
        .block(SessionFrame::new(0), [&mut left, &mut right])
        .expect("real EQ executor block");
    let output = mock::output(None).get();
    let deck = mixer.ends.snapshot.read();
    let pass = DeckPass {
        mix: queue.deck.mix,
        suspended: false,
        now: SessionFrame::new(0),
        delivery: FrameCount::new(128),
        output: &output,
        deck,
    };
    while let Some(kithara_command::ScopedReceipt::Scope(_, receipt)) = mixer.ring.receipt() {
        hang_tick!();
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        let mut scope = mixer.ring.scope(mixer.scope).expect("mixer scope");
        let mut out = Outbox::new(&mut scope, &mut loads.dispatcher).in_pass(pass);
        HostedDeck::settle(
            queue,
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            pass,
            &mut out,
        );
        hang_reset!();
    }
    let mut scope = mixer.ring.scope(mixer.scope).expect("mixer scope");
    let mut out = Outbox::new(&mut scope, &mut loads.dispatcher).in_pass(pass);
    HostedDeck::tick(queue, pass, &mut out);
}

#[kithara::test(native)]
fn a_handle_clone_sees_the_replacement_band_array() {
    let region = pools();
    let config = DeckMixerConfig::builder().eq_bands(4).build();
    let prep = kithara_play::ResourcePrep::builder()
        .worker(kithara_play::PlayWorker::new(
            kithara_play::PlayWorkerConfig::builder(region.clone()).build(),
        ))
        .build();
    let mut queue = Queue::new(QueueConfig::builder().prep(prep).mixer(config).build());
    let shape = StreamShape::new(NonZeroU32::new(128).expect("block size"), mock::SAMPLE_RATE);
    let mut mixer = mock::MixerRig::new(config, shape, &region).expect("real EQ mixer");
    let mut loads = mock::DeckRig::new(config).expect("dispatcher");
    let eq = queue.control();
    let handle = eq.clone();
    eq_call(&mut queue, &mut mixer, &mut loads, eq.clone(), |control| {
        control.set_eq_layout(
            [0.0; 3]
                .map(|gain| EqBandConfig::builder().gain_db(gain.into()).build())
                .to_vec(),
        )
    });
    eq_call(&mut queue, &mut mixer, &mut loads, eq.clone(), |control| {
        control.set_eq_gain(1, 4.0)
    });
    assert_eq!(
        (0..handle.eq_band_count())
            .map(|band| handle.eq_gain(band).expect("band"))
            .collect::<Vec<_>>(),
        vec![0.0, 4.0, 0.0]
    );
    let rendered = mixer.ends.snapshot.read();
    assert_eq!(
        (0..rendered.eq.bands())
            .map(|band| f32::from(rendered.eq.gain(band).expect("band")))
            .collect::<Vec<_>>(),
        vec![0.0, 4.0, 0.0]
    );
    eq_call(&mut queue, &mut mixer, &mut loads, eq.clone(), |control| {
        control.set_eq_layout(
            [-6.0, -2.0, 2.0, 5.0]
                .map(|gain| EqBandConfig::builder().gain_db(gain.into()).build())
                .to_vec(),
        )
    });
    assert_eq!(handle.eq_band_count(), 4);
    assert_eq!(handle.eq_gain(2), Some(2.0));
    eq_call(&mut queue, &mut mixer, &mut loads, handle, |control| {
        control.set_eq_gain(3, 1.0)
    });
    assert_eq!(
        (0..eq.eq_band_count())
            .map(|band| eq.eq_gain(band).expect("band"))
            .collect::<Vec<_>>(),
        vec![-6.0, -2.0, 2.0, 1.0]
    );
    let rendered = mixer.ends.snapshot.read();
    assert_eq!(
        (0..rendered.eq.bands())
            .map(|band| f32::from(rendered.eq.gain(band).expect("band")))
            .collect::<Vec<_>>(),
        vec![-6.0, -2.0, 2.0, 1.0]
    );
}

#[kithara::test(tokio)]
#[case(f64::INFINITY, Some(Duration::from_secs(10)), 441_000)]
#[case(f64::INFINITY, None, 0)]
#[case(f64::NAN, Some(Duration::from_secs(10)), 0)]
async fn seek_frame_index_clamps_unrepresentable_targets(
    #[case] target: f64,
    #[case] duration: Option<Duration>,
    #[case] expected_frame: u64,
) {
    let (mut queue, _, _, mut rig, _dir) = pending_selection();
    let seq = queue
        .active
        .iter()
        .find_map(|active| active.load)
        .expect("selected load")
        .seq();
    let mut loaded =
        crate::mock::loaded_request(queue.worker().expect("fixture worker"), &mut rig, seq).await;
    let source = loaded
        .opened
        .pcm
        .next_chunk_info()
        .expect("prepared fixture source");
    let (opened, source_packets) = crate::mock::opened_with_source(
        loaded.opened,
        kithara_signal::AudioChunkInfo {
            media_end: duration,
            ..source
        },
    );
    loaded.opened = opened;
    answer_loaded(&mut queue, &mut rig, loaded);
    finish(&mut queue, &mut rig);
    let control = queue.control();
    assert_eq!(
        control.duration_seconds(),
        duration.map(|duration| duration.as_secs_f64())
    );
    let result = drive_call(
        &mut queue,
        control.clone(),
        move |control| control.seek(target),
        |queue| {
            let output = mock::output(None).get();
            let deck = queue.deck.mixer.clone();
            let pass = DeckPass {
                mix: queue.deck.mix,
                suspended: false,
                now: SessionFrame::new(0),
                delivery: FrameCount::new(128),
                output: &output,
                deck: &deck,
            };
            let mut scope = rig.ring.scope(rig.scope).expect("deck scope");
            let mut out = Outbox::new(&mut scope, &mut rig.dispatcher).in_pass(pass);
            HostedDeck::drain(queue, pass, &mut out);
        },
    );
    assert!(
        result.is_ok(),
        "unrepresentable seek must clamp successfully: {result:?}"
    );
    let position = control.position_seconds().expect("clamped position");
    assert_eq!(
        (position * 44_100.0)
            .round()
            .to_u64()
            .expect("clamped position frames fit u64"),
        expected_frame
    );
    drop(source_packets);
}
