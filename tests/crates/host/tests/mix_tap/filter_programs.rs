use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    assets::AssetStore,
    effects::filter::{FilterEffect, FilterKnob, FilterSettings},
    host::Tap,
    play::{CurveShape, FilterProgram, ParameterKnot, PlayError, ResourceConfig, ResourceSrc},
    queue::{QueueError, TrackSource},
    signal::SessionFrame,
    warp::WarpConfig,
};
use kithara_command::When;
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions, asset_source};
use kithara_test_fixtures::assets;

use super::cases::{ROOMY_CAPACITY, SAMPLE_RATE, load_source, render_effect_window};
use crate::underrun::Underruns;

fn frame(at: u64) -> SessionFrame {
    SessionFrame::new(i64::try_from(at).expect("fixture frame"))
}

fn point(at: u64, value: f64, shape: CurveShape) -> ParameterKnot {
    ParameterKnot::new(frame(at), value, shape).expect("finite fixture point")
}

fn knob(value: f64) -> FilterKnob {
    FilterKnob::checked(value).expect("knob position")
}

/// The knob of an S-shaped program from `left` at `start` to `right` after
/// `frames`, as the deck computes it.
fn s_curve(at: u64, start: u64, frames: u64, left: f64, right: f64) -> f64 {
    if at >= start + frames {
        return right;
    }
    let elapsed = f64::from(u32::try_from(at - start).expect("local program frame"));
    let t = elapsed / f64::from(u32::try_from(frames).expect("program length"));
    left + (right - left) * (3.0 * t * t - 2.0 * t * t * t)
}

fn s_program(start: u64, frames: u64, left: f64, right: f64) -> FilterProgram {
    FilterProgram::new(vec![
        point(start, left, CurveShape::cubic(0.0, 1.0).expect("S shape")),
        point(start + frames, right, CurveShape::HOLD),
    ])
    .expect("knob program")
}

/// One frame of the reference deck filter on a constant stereo input.
fn tick(reference: &mut FilterEffect, input: f32, program: Option<f64>) -> [f32; 2] {
    let (mut left, mut right) = ([input], [input]);
    let planes = &mut [&mut left[..], &mut right[..]];
    match program {
        Some(position) => reference.process_program(planes, 0..1, &[position]),
        None => reference.process(planes, 0..1),
    }
    .expect("reference frame");
    [left[0], right[0]]
}

fn bits(pcm: &[f32]) -> Vec<u32> {
    pcm.iter().map(|sample| sample.to_bits()).collect()
}

async fn loaded_filter_source(source: String) -> (OfflinePlayer, Underruns) {
    let options = OfflinePlayerOptions::builder()
        .block_on_underrun(false)
        .output_block_frames(NonZeroU32::new(64).expect("callback"))
        .warp(
            WarpConfig::builder()
                .render_quantum_frames(NonZeroUsize::new(64).expect("quantum"))
                .build(),
        )
        .build();
    let harness = OfflinePlayer::with_sample_rate(options, SAMPLE_RATE).await;
    let underruns = Underruns::new(&harness);
    let config = ResourceConfig::for_src(ResourceSrc::parse(source).expect("fixture URI"))
        .store(AssetStore::builder(harness.worker().pools().clone()).build())
        .preload_chunks(NonZeroUsize::MIN)
        .audio_buffer_chunks(NonZeroUsize::new(2).expect("packets"))
        .build();
    let harness = load_source(harness, TrackSource::Config(Box::new(config))).await;
    (harness, underruns)
}

#[kithara::test(
    tokio,
    tracing(
        "warn,kithara_render_probe=trace,kithara_worker_probe=trace,kithara_test::host_rate_telemetry=info"
    ),
    timeout(Short)
)]
#[case::inside_callback(0)]
#[case::callback_edge(47)]
async fn public_knob_turns_programs_and_takeover_reach_the_host(#[case] phase: u64) {
    let (harness, mut underruns) =
        loaded_filter_source(asset_source(&assets::fixed_constant_wav_cd_half_44100())).await;
    let first = render_effect_window(&harness, 64).await;
    assert_eq!(first.len(), 128);
    underruns.checkpoint(harness.metrics(), true, "Filter first callback");
    assert_eq!(harness.metrics().underruns(), 0);
    let warm = render_effect_window(&harness, 2048).await;
    assert_eq!(&warm[warm.len() - 128..], &[0.5; 128]);
    underruns.checkpoint(harness.metrics(), true, "Filter warmup at the center");
    assert_eq!(harness.metrics().underruns(), 0);
    let actual_rate = harness.host().spec().await.sample_rate;
    assert_eq!(actual_rate.get(), SAMPLE_RATE);
    let mut reference = FilterEffect::new(actual_rate, FilterSettings::default());
    let start = harness.host().position();
    let turn = start + 529 + phase;
    harness
        .with_queue(move |queue| queue.filter(When::At(frame(turn)), knob(-0.5)))
        .await
        .expect("public knob turn");
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("master tap");
    let mut take = render_effect_window(&harness, 1024).await;
    assert_eq!(tap.drain(), take);
    assert_eq!(tap.drops(), 0);
    let turned = harness.deck_snapshot().filter;
    assert!(turned.binding().is_some());
    assert!(turned.authority().is_some());
    underruns.checkpoint(harness.metrics(), true, "Filter turn");

    let curve = harness.host().position() + 529 + phase;
    let manual = curve + 129;
    harness
        .with_queue(move |queue| queue.filter_program(s_program(curve, 512, -0.2, -0.8)))
        .await
        .expect("public S-shaped program");
    harness
        .with_queue(move |queue| queue.filter(When::At(frame(manual)), knob(0.25)))
        .await
        .expect("public manual takeover");
    let stale = s_program(curve + 256, 64, 1.0, 1.0);
    harness
        .with_queue(move |queue| queue.filter_program(stale))
        .await
        .expect("stale request is queued before manual authority applies");
    let late = s_program(start, 64, 0.5, 0.5);
    assert!(matches!(
        harness
            .with_queue(move |queue| queue.filter_program(late))
            .await,
        Err(QueueError::Play(PlayError::Late))
    ));
    let sweep = render_effect_window(&harness, 2048).await;
    assert_eq!(tap.drain(), sweep);
    take.extend(sweep);
    let snapshot = harness.deck_snapshot().filter;
    assert_ne!(snapshot.authority(), turned.authority());
    assert_eq!(snapshot.program(), None);
    assert_eq!(snapshot.binding(), turned.binding());
    underruns.checkpoint(
        harness.metrics(),
        true,
        "Filter program and manual takeover",
    );

    let center = harness.host().position() + 529 + phase;
    harness
        .with_queue(move |queue| queue.filter(When::At(frame(center)), FilterKnob::DEFAULT))
        .await
        .expect("public return to center");
    // The manual glide to center lasts 62 ms, about 2734 frames.
    let returned = render_effect_window(&harness, 4096).await;
    assert_eq!(tap.drain(), returned);
    assert_eq!(
        &returned[returned.len() - 256..],
        &[0.5; 256],
        "the center passes the source bit for bit"
    );
    take.extend(returned);
    assert_eq!(harness.host().position(), start + 7168);
    assert_eq!(harness.deck_snapshot().filter.knob(), 0.0);

    let mut expected = Vec::with_capacity(take.len());
    for offset in 0_u64..7168 {
        let at = start + offset;
        for (frame, position) in [(turn, -0.5), (manual, 0.25), (center, 0.0)] {
            if at == frame {
                reference.configure(knob(position));
            }
        }
        let program = (curve..manual)
            .contains(&at)
            .then(|| s_curve(at, curve, 512, -0.2, -0.8));
        expected.extend(tick(&mut reference, 0.5, program));
    }
    assert_ne!(
        expected,
        vec![0.5; expected.len()],
        "the oracle hears the turn, the program and the takeover"
    );
    assert_eq!(bits(&take), bits(&expected));
    assert_eq!(tap.drops(), 0);
    underruns.checkpoint(harness.metrics(), true, "Filter final complete edges");
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}

#[kithara::test(tokio, timeout(Short))]
async fn filter_tail_and_programme_outlive_source_eof_and_close_while_owned() {
    let (harness, mut underruns) =
        loaded_filter_source(asset_source(&assets::fixed_constant_wav_cd_half_4410())).await;
    let first = render_effect_window(&harness, 64).await;
    assert_eq!(first.len(), 128);
    underruns.checkpoint(harness.metrics(), true, "Filter EOF initial callback");
    assert_eq!(harness.metrics().underruns(), 0);
    let warm = render_effect_window(&harness, 2048).await;
    assert_eq!(&warm[warm.len() - 128..], &[0.5; 128]);
    let start = harness.host().position();
    let turn = start + 529;
    harness
        .with_queue(move |queue| queue.filter(When::At(frame(turn)), knob(-0.5)))
        .await
        .expect("turn the knob");
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("master tap");
    let mut take = render_effect_window(&harness, 1024).await;
    assert_eq!(tap.drain(), take);
    let snapshot = harness.deck_snapshot();
    let slot = snapshot
        .slots
        .iter()
        .find(|slot| slot.binding.is_some())
        .expect("sounding source");
    let mark = slot.mark.expect("source/session anchor");
    let spec = slot.source.expect("source metadata").spec;
    assert_eq!(spec.sample_rate.get(), SAMPLE_RATE);
    assert_eq!(
        mark.position,
        spec.duration_for(mark.lane.frame)
            .expect("one-to-one source clock")
    );
    let source_start = u64::try_from(i64::from(mark.session))
        .expect("session anchor")
        .checked_sub(mark.lane.frame)
        .expect("initial source origin");
    let source_end = source_start + 4410;
    let curve = harness.host().position() + 529;
    harness
        .with_queue(move |queue| queue.filter_program(s_program(curve, 4096, -0.2, -0.8)))
        .await
        .expect("public programme");
    let drained = render_effect_window(&harness, 8192).await;
    assert_eq!(tap.drain(), drained);
    take.extend(drained);
    let rate = harness.host().spec().await.sample_rate;
    let mut reference = FilterEffect::new(rate, FilterSettings::default());
    let mut expected = Vec::with_capacity(take.len());
    for offset in 0..take.len() / 2 {
        let at = start + u64::try_from(offset).expect("capture frame");
        let input = if (source_start..source_end).contains(&at) {
            0.5
        } else {
            0.0
        };
        if at == turn {
            reference.configure(knob(-0.5));
        }
        let program = (at >= curve).then(|| s_curve(at, curve, 4096, -0.2, -0.8));
        expected.extend(tick(&mut reference, input, program));
    }
    assert_eq!(bits(&take), bits(&expected));
    assert!(
        take[take.len() - 512..]
            .iter()
            .all(|sample| sample.to_bits() == 0)
    );
    let tail_offset = usize::try_from(source_end - start).expect("source endpoint") * 2;
    assert!(
        take[tail_offset..tail_offset + 64]
            .iter()
            .any(|sample| *sample != 0.0),
        "the causal tail survives EOF"
    );
    let snapshot = harness.deck_snapshot();
    assert!(
        snapshot
            .slots
            .iter()
            .all(|slot| slot.state != kithara_render::bridge::SlotState::Playing)
    );
    assert_eq!(snapshot.filter.knob(), -0.8);
    assert!(snapshot.filter.program().is_some());
    let future = harness.host().position() + 529;
    harness
        .with_queue(move |queue| queue.filter_program(s_program(future, 8192, -0.8, -0.8)))
        .await
        .expect("programme on empty deck");
    let empty = render_effect_window(&harness, 640).await;
    assert_eq!(tap.drain(), empty);
    assert!(empty.iter().all(|sample| sample.to_bits() == 0));
    assert!(harness.deck_snapshot().filter.program().is_some());
    assert_eq!(tap.drops(), 0);
    underruns.checkpoint(harness.metrics(), true, "Filter EOF tail and owned close");
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}
