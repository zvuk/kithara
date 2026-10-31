use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    assets::AssetStore,
    effects::{
        EffectConfig, GainDb,
        eq::{EqBandConfig, EqBandConfigControl},
    },
    host::Tap,
    play::{CurveShape, ParameterKnot, PlayError, ResourceConfig, ResourceSrc},
    queue::{QueueError, TrackSource},
    signal::SessionFrame,
    warp::WarpConfig,
};
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions, asset_source};
use kithara_test_fixtures::assets;
use num_traits::ToPrimitive;

use super::cases::{BLOCK_FRAMES, ROOMY_CAPACITY, SAMPLE_RATE, load_source, render_effect_window};

#[kithara::test(tokio)]
#[case::audible(false)]
#[case::output_muted(true)]
async fn public_effect_cubic_reaches_the_host_without_second_smoothing(#[case] muted: bool) {
    let options = OfflinePlayerOptions::builder()
        .block_on_underrun(false)
        .output_block_frames(NonZeroU32::new(64).expect("callback size"))
        .warp(
            WarpConfig::builder()
                .render_quantum_frames(NonZeroUsize::new(64).expect("quantum"))
                .build(),
        )
        .build();
    let harness = OfflinePlayer::with_sample_rate(options, SAMPLE_RATE).await;
    let config = ResourceConfig::for_src(
        ResourceSrc::parse(asset_source(&assets::fixed_constant_wav_cd_half_44100()))
            .expect("fixture URI"),
    )
    .store(AssetStore::builder(harness.worker().pools().clone()).build())
    .preload_chunks(NonZeroUsize::MIN)
    .audio_buffer_chunks(NonZeroUsize::new(2).expect("packet count"))
    .effects(vec![EffectConfig::Equalizer(vec![EqBandConfig::default()])])
    .build();
    let harness = load_source(harness, TrackSource::Config(Box::new(config))).await;
    let _ = render_effect_window(&harness, BLOCK_FRAMES).await;
    let warm = render_effect_window(&harness, 4096).await;
    assert!(warm[warm.len() - 128..].iter().all(|sample| *sample == 0.5));
    let control = harness
        .with_queue(|queue| queue.eq_band(0, 0))
        .await
        .expect("bound band");
    let item = harness
        .with_queue(|queue| queue.current().expect("loaded item").id)
        .await;
    let start = harness.host().position();
    let target = start + 512 + 17;
    let knots = vec![
        ParameterKnot::new(
            SessionFrame::new(i64::try_from(target).expect("target")),
            0.0,
            CurveShape::cubic(0.0, 1.0).expect("S handles"),
        )
        .expect("first knot"),
        ParameterKnot::new(
            SessionFrame::new(i64::try_from(target + 128).expect("end")),
            -12.0,
            CurveShape::HOLD,
        )
        .expect("last knot"),
    ];
    harness
        .run(&control, move |effect| effect.gain_program(knots))
        .await
        .expect("reachable curve");
    assert_eq!(control.gain_db(), GainDb::DEFAULT);
    let late = ParameterKnot::new(
        SessionFrame::new(i64::try_from(start).expect("past frame")),
        6.0,
        CurveShape::HOLD,
    )
    .expect("finite step");
    assert!(matches!(
        harness
            .run(&control, move |effect| effect.gain_program(vec![late]))
            .await,
        Err(QueueError::Play(PlayError::Late))
    ));
    if muted {
        let at = SessionFrame::new(i64::try_from(target + 63).expect("mute frame"));
        let output = kithara::play::SlotGainProgram::new(vec![
            ParameterKnot::new(at, 0.0, CurveShape::HOLD).expect("exact output mute"),
        ])
        .expect("one-knot output program");
        harness
            .with_queue(move |queue| queue.gain_program(item, output))
            .await
            .expect("reachable output mute");
    }
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("master tap");
    let take = render_effect_window(&harness, 1024).await;
    assert_eq!(tap.drain(), take);
    assert_eq!(tap.drops(), 0);
    for (offset, &channels) in take.as_chunks::<2>().0.iter().enumerate() {
        let at = start + u64::try_from(offset).expect("local frame");
        let db = if at < target {
            0.0
        } else if at >= target + 128 {
            -12.0
        } else {
            let t = (at - target).to_f64().expect("finite frame") / 128.0;
            -12.0 * (3.0 * t * t - 2.0 * t * t * t)
        };
        let db = db.to_f32().expect("canonical EQ dB");
        let expected = if muted && at >= target + 63 {
            0.0
        } else {
            0.5 * 10.0_f32.powf(db / 20.0)
        };
        assert_eq!(channels, [expected; 2], "host frame {at}");
    }
    assert_eq!(control.gain_db(), GainDb::from(-12.0));
    if muted {
        let slot = harness
            .deck_snapshot()
            .slots
            .into_iter()
            .find(|slot| slot.binding.is_some_and(|binding| binding.item() == item))
            .expect("same sounding attachment");
        assert_eq!(slot.output.gain(), 0.0);
        assert!(slot.output.program().is_some());
    }

    let future = harness.host().position() + 529;
    let knots = [future, future + 1024]
        .into_iter()
        .map(|at| {
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(at).expect("held curve frame")),
                -12.0,
                CurveShape::HOLD,
            )
            .expect("held gain")
        })
        .collect();
    harness
        .run(&control, move |effect| effect.gain_program(knots))
        .await
        .expect("reachable held curve");
    let _ = render_effect_window(&harness, 640).await;
    harness
        .with_queue(kithara::queue::QueueControl::clear)
        .await
        .expect("clear queue");
    let stopped = render_effect_window(&harness, 1024).await;
    assert!(
        stopped[stopped.len() - 128..]
            .iter()
            .all(|sample| *sample == 0.0)
    );
    let stale = ParameterKnot::new(
        SessionFrame::new(i64::try_from(harness.host().position() + 1024).expect("future frame")),
        6.0,
        CurveShape::HOLD,
    )
    .expect("finite step");
    assert!(matches!(
        harness
            .run(&control, move |effect| effect.gain_program(vec![stale]))
            .await,
        Err(QueueError::Play(PlayError::NotReady))
    ));
    assert_eq!(control.gain_db(), GainDb::from(-12.0));
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}
