use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    assets::AssetStore,
    host::Tap,
    play::{CurveShape, ParameterKnot, ResourceConfig, ResourceSrc, SlotGainProgram},
    queue::TrackSource,
    signal::SessionFrame,
    warp::WarpConfig,
};
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions, asset_source};
use kithara_test_fixtures::assets;
use num_traits::ToPrimitive;

use super::cases::{BLOCK_FRAMES, ROOMY_CAPACITY, SAMPLE_RATE, load_source, render_effect_window};

#[kithara::test(tokio)]
async fn public_queue_output_cubic_reaches_the_host_at_exact_frames() {
    let options = OfflinePlayerOptions::builder()
        .block_on_underrun(false)
        .output_block_frames(NonZeroU32::new(64).expect("nonzero callback size"))
        .warp(
            WarpConfig::builder()
                .render_quantum_frames(NonZeroUsize::new(64).expect("nonzero quantum"))
                .build(),
        )
        .build();
    let harness = OfflinePlayer::with_sample_rate(options, SAMPLE_RATE).await;
    let config = ResourceConfig::for_src(
        ResourceSrc::parse(asset_source(&assets::fixed_constant_wav_cd_half_44100()))
            .expect("fixture URI parses"),
    )
    .store(AssetStore::builder(harness.worker().pools().clone()).build())
    .preload_chunks(NonZeroUsize::MIN)
    .audio_buffer_chunks(NonZeroUsize::new(2).expect("nonzero packet count"))
    .build();
    let harness = load_source(harness, TrackSource::Config(Box::new(config))).await;
    let _ = render_effect_window(&harness, BLOCK_FRAMES).await;
    let warm = render_effect_window(&harness, 4096).await;
    assert!(warm[warm.len() - 128..].iter().all(|sample| *sample == 0.5));
    let item = harness
        .with_queue(|queue| queue.current().expect("loaded current item").id)
        .await;
    let start = harness.host().position();
    let target = start + 512 + 17;
    let at = SessionFrame::new(i64::try_from(target).expect("target frame"));
    let end = SessionFrame::new(i64::try_from(target + 128).expect("terminal frame"));
    let program = SlotGainProgram::new(vec![
        ParameterKnot::new(at, 1.0, CurveShape::cubic(0.0, 1.0).expect("S handles"))
            .expect("unity endpoint"),
        ParameterKnot::new(end, 0.25, CurveShape::HOLD).expect("held endpoint"),
    ])
    .expect("prepared amplitude program");
    harness
        .with_queue(move |queue| queue.gain_program(item, program))
        .await
        .expect("future complete-output program");
    let sounding = harness
        .deck_snapshot()
        .slots
        .into_iter()
        .find(|slot| slot.binding.is_some_and(|binding| binding.item() == item))
        .expect("sounding item binding");
    assert_eq!(sounding.output.gain(), 1.0);
    assert_eq!(sounding.output.program(), None);
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("master tap");
    let take = render_effect_window(&harness, 1024).await;
    assert_eq!(tap.drain(), take);
    assert_eq!(tap.drops(), 0);
    for (index, &channels) in take.as_chunks::<2>().0.iter().enumerate() {
        let frame = start + u64::try_from(index).expect("capture offset");
        let gain = if frame < target {
            1.0
        } else if frame >= target + 128 {
            0.25
        } else {
            let time = (frame - target).to_f64().expect("local frame") / 128.0;
            1.0 - 0.75 * (3.0 * time * time - 2.0 * time * time * time)
        };
        let expected = 0.5 * gain.to_f32().expect("bounded amplitude");
        assert_eq!(channels, [expected, expected], "addressed frame {frame}");
    }
    let sounding = harness
        .deck_snapshot()
        .slots
        .into_iter()
        .find(|slot| slot.binding.is_some_and(|binding| binding.item() == item))
        .expect("same sounding attachment");
    assert_eq!(sounding.output.gain(), 0.25);
    assert!(sounding.output.program().is_some());
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}
