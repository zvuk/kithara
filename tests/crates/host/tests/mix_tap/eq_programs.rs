use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    assets::AssetStore,
    host::Tap,
    play::{
        CurveShape, DeckParameter, DeckProgram, EqBandConfig, GainDb, ParameterKnot, PlayError,
        ResourceConfig, ResourceSrc, SlotGainProgram,
    },
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
async fn public_deck_eq_cubic_reaches_the_host_without_second_smoothing(#[case] muted: bool) {
    let options = OfflinePlayerOptions::builder()
        .block_on_underrun(false)
        .eq_layout(vec![EqBandConfig::builder().build()])
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
    .build();
    let harness = load_source(harness, TrackSource::Config(Box::new(config))).await;
    let _ = render_effect_window(&harness, BLOCK_FRAMES).await;
    let warm = render_effect_window(&harness, 4096).await;
    assert!(
        warm.get(warm.len().saturating_sub(128)..)
            .expect("warm suffix")
            .iter()
            .all(|sample| *sample == 0.5)
    );
    let item = harness
        .with_queue(|queue| queue.current().expect("loaded item").id)
        .await;
    let start = harness.host().position();
    let target = start.saturating_add(529);
    let terminal = target.saturating_add(128);
    let program = DeckProgram::new(
        DeckParameter::EqBand {
            band: 0,
            layout: harness
                .deck_snapshot()
                .eq
                .layout()
                .expect("observed EQ layout"),
        },
        vec![
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(target).expect("target frame")),
                0.0,
                CurveShape::cubic(0.0, 1.0).expect("S handles"),
            )
            .expect("left endpoint"),
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(terminal).expect("terminal frame")),
                -12.0,
                CurveShape::HOLD,
            )
            .expect("right endpoint"),
        ],
    )
    .expect("prepared EQ gain program");
    harness
        .with_queue(move |queue| queue.deck_programs(vec![program]))
        .await
        .expect("public EQ control");
    assert_eq!(harness.deck_snapshot().eq.gain(0), Some(GainDb::DEFAULT));
    assert_eq!(harness.deck_snapshot().eq.program(0), None);
    let late = DeckProgram::new(
        DeckParameter::EqBand {
            band: 0,
            layout: harness
                .deck_snapshot()
                .eq
                .layout()
                .expect("observed EQ layout"),
        },
        vec![
            ParameterKnot::new(
                SessionFrame::new(i64::from(harness.deck_snapshot().at).saturating_sub(1)),
                6.0,
                CurveShape::HOLD,
            )
            .expect("finite late knot"),
        ],
    )
    .expect("prepared late program");
    let late_result = harness
        .with_queue(move |queue| queue.deck_programs(vec![late]))
        .await;
    assert!(
        matches!(late_result, Err(QueueError::Play(PlayError::Late))),
        "result {late_result:?}, first knot {start}, mixer frontier {:?}",
        harness.deck_snapshot().at
    );
    if muted {
        let output = SlotGainProgram::new(vec![
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(target + 63).expect("mute frame")),
                0.0,
                CurveShape::HOLD,
            )
            .expect("exact mute"),
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
    for (index, &channels) in take.as_chunks::<2>().0.iter().enumerate() {
        let frame = start.saturating_add(u64::try_from(index).expect("capture offset"));
        let db = if frame < target {
            0.0
        } else if frame >= terminal {
            -12.0
        } else {
            let t = frame.saturating_sub(target).to_f64().expect("local frame") / 128.0;
            -12.0 * (3.0 * t * t - 2.0 * t * t * t)
        };
        let gain = GainDb::checked(db.to_f32().expect("canonical dB")).expect("admitted dB");
        let expected = if muted && frame >= target + 63 {
            0.0
        } else {
            0.5 * 10.0_f32.powf(f32::from(gain) / 20.0)
        };
        assert_eq!(channels, [expected; 2], "addressed frame {frame}");
    }
    let snapshot = harness.deck_snapshot();
    assert_eq!(snapshot.eq.gain(0), GainDb::checked(-12.0));
    assert!(snapshot.eq.program(0).is_some());
    if muted {
        let slot = snapshot
            .slots
            .into_iter()
            .find(|slot| slot.binding.is_some_and(|binding| binding.item() == item))
            .expect("same sounding attachment");
        assert_eq!(slot.output.gain(), 0.0);
        assert!(slot.output.program().is_some());
    }
    let future = harness.host().position() + 529;
    let held = DeckProgram::new(
        DeckParameter::EqBand {
            band: 0,
            layout: harness
                .deck_snapshot()
                .eq
                .layout()
                .expect("observed EQ layout"),
        },
        [future, future + 1024]
            .into_iter()
            .map(|at| {
                ParameterKnot::new(
                    SessionFrame::new(i64::try_from(at).expect("held frame")),
                    -12.0,
                    CurveShape::HOLD,
                )
                .expect("held gain")
            })
            .collect(),
    )
    .expect("prepared held curve");
    harness
        .with_queue(move |queue| queue.deck_programs(vec![held]))
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
    assert_eq!(harness.deck_snapshot().eq.gain(0), GainDb::checked(-12.0));
    harness
        .with_queue(|queue| queue.set_eq_layout(Vec::new()))
        .await
        .expect("remove EQ layout");
    let _ = render_effect_window(&harness, 4096).await;
    let unavailable = DeckProgram::new(
        DeckParameter::EqBand {
            band: 0,
            layout: harness
                .deck_snapshot()
                .eq
                .layout()
                .expect("observed EQ layout"),
        },
        vec![
            ParameterKnot::new(
                SessionFrame::new(
                    i64::try_from(harness.host().position() + 1024).expect("future frame"),
                ),
                6.0,
                CurveShape::HOLD,
            )
            .expect("finite unavailable knot"),
        ],
    )
    .expect("prepared unavailable program");
    assert!(matches!(
        harness
            .with_queue(move |queue| queue.deck_programs(vec![unavailable]))
            .await,
        Err(QueueError::Play(PlayError::EqBandOutOfRange {
            band: 0,
            bands: 0
        }))
    ));
    assert_eq!(harness.deck_snapshot().eq.gain(0), None);
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}
