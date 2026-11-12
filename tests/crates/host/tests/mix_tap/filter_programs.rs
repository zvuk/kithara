use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    assets::AssetStore,
    effects::filter::{
        Cutoff, FilterConfig, FilterEffect, FilterSettings, FilterSettingsChange, Kind, Q,
    },
    host::Tap,
    play::{
        CurveShape, DeckFilterChange, FilterParameter, FilterProgram, GainDb, ParameterKnot,
        PlayError, ResourceConfig, ResourceSrc,
    },
    queue::{QueueError, TrackSource},
    signal::{AudioSpec, SessionFrame},
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

fn cutoff_at(at: u64, start: u64) -> f64 {
    if at >= start + 512 {
        return 800.0;
    }
    let t = f64::from(u32::try_from(at - start).expect("local programme frame")) / 512.0;
    200.0 * 4.0_f64.powf(3.0 * t * t - 2.0 * t * t * t)
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
    )
)]
#[case::inside_callback(0)]
#[case::callback_edge(47)]
async fn public_filter_curves_takeover_reset_and_clear_reach_the_host(#[case] phase: u64) {
    let (harness, mut underruns) =
        loaded_filter_source(asset_source(&assets::fixed_constant_wav_cd_half_44100())).await;
    let first = render_effect_window(&harness, 64).await;
    assert_eq!(first.len(), 128);
    underruns.checkpoint(harness.metrics(), true, "Filter first callback");
    assert_eq!(harness.metrics().underruns(), 0);
    let warm = render_effect_window(&harness, 4096).await;
    assert_eq!(&warm[warm.len() - 128..], &[0.5; 128]);
    underruns.checkpoint(harness.metrics(), true, "Filter warmup before construction");
    assert_eq!(harness.metrics().underruns(), 0);
    let actual_rate = harness.host().spec().await.sample_rate;
    assert_eq!(actual_rate.get(), SAMPLE_RATE);
    let settings = FilterSettings::builder()
        .kind(Kind::LowPass)
        .cutoff_hz(Cutoff::checked(1_000.0).expect("cutoff"))
        .build()
        .expect("settings");
    let mut reference = FilterEffect::new(
        AudioSpec::new(2, actual_rate),
        FilterConfig::from((harness.worker().pools().clone(), settings)),
    )
    .expect("independent reference state");
    let start = harness.host().position();
    let install = start + 529 + phase;
    harness
        .with_queue(move |queue| queue.install_filter(When::At(frame(install)), settings))
        .await
        .expect("public cold preparation and install");
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("master tap");
    let mut take = render_effect_window(&harness, 1024).await;
    assert_eq!(tap.drain(), take);
    assert_eq!(tap.drops(), 0);
    let installed = harness.deck_snapshot().filter;
    assert_eq!(installed.settings(), Some(settings));
    underruns.checkpoint(harness.metrics(), true, "Filter install");

    let curve = harness.host().position() + 529 + phase;
    let manual = curve + 129;
    let cutoff = FilterProgram::new(
        FilterParameter::CutoffHz,
        vec![
            point(curve, 200.0, CurveShape::cubic(0.0, 1.0).expect("S shape")),
            point(curve + 512, 800.0, CurveShape::HOLD),
        ],
    )
    .expect("cutoff trajectory");
    let q = FilterProgram::new(
        FilterParameter::Q,
        vec![
            point(curve, 0.75, CurveShape::LINEAR),
            point(curve + 512, 0.8, CurveShape::HOLD),
        ],
    )
    .expect("Q trajectory");
    harness
        .with_queue(move |queue| queue.filter_program(cutoff))
        .await
        .expect("public cubic cutoff");
    harness
        .with_queue(move |queue| queue.filter_program(q))
        .await
        .expect("public independent Q");
    harness
        .with_queue(move |queue| {
            queue.filter(
                When::At(frame(curve + 65)),
                DeckFilterChange::Configure(FilterSettingsChange::GainDb(GainDb::MAX)),
            )
        })
        .await
        .expect("contextually refused gain submitted");
    harness
        .with_queue(move |queue| {
            queue.filter(
                When::At(frame(manual)),
                DeckFilterChange::Configure(FilterSettingsChange::CutoffHz(
                    Cutoff::checked(1_200.0).expect("manual cutoff"),
                )),
            )
        })
        .await
        .expect("public manual takeover");
    let stale = FilterProgram::new(
        FilterParameter::CutoffHz,
        vec![point(curve + 256, 4_000.0, CurveShape::HOLD)],
    )
    .expect("queued stale sweep");
    harness
        .with_queue(move |queue| queue.filter_program(stale))
        .await
        .expect("stale request is queued before manual authority applies");
    let late = FilterProgram::new(
        FilterParameter::CutoffHz,
        vec![point(start, 2_000.0, CurveShape::HOLD)],
    )
    .expect("late sweep");
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
    assert_eq!(
        f64::from(snapshot.settings().expect("settings").cutoff_hz()),
        1_200.0
    );
    assert_eq!(f64::from(snapshot.settings().expect("settings").q()), 0.8);
    assert_eq!(
        snapshot.settings().expect("settings").gain_db(),
        GainDb::DEFAULT
    );
    assert!(snapshot.authority(FilterParameter::CutoffHz).is_some());
    assert_eq!(snapshot.program(FilterParameter::CutoffHz), None);
    assert!(snapshot.program(FilterParameter::Q).is_some());
    assert_eq!(snapshot.authority(FilterParameter::Q), None);
    underruns.checkpoint(harness.metrics(), true, "Filter curves and manual takeover");

    let reset = harness.host().position() + 529 + phase;
    harness
        .with_queue(move |queue| queue.filter(When::At(frame(reset)), DeckFilterChange::Reset))
        .await
        .expect("public reset");
    let restarted = render_effect_window(&harness, 1024).await;
    assert_eq!(tap.drain(), restarted);
    take.extend(restarted);
    assert_eq!(
        harness.deck_snapshot().filter.binding(),
        installed.binding()
    );
    assert_eq!(
        harness.deck_snapshot().filter.program(FilterParameter::Q),
        None
    );
    let clear = harness.host().position() + 529 + phase;
    harness
        .with_queue(move |queue| queue.filter(When::At(frame(clear)), DeckFilterChange::Clear))
        .await
        .expect("public clear");
    let cleared = render_effect_window(&harness, 1024).await;
    assert_eq!(tap.drain(), cleared);
    take.extend(cleared);
    assert_eq!(harness.deck_snapshot().filter.settings(), None);
    assert_ne!(
        harness.deck_snapshot().filter.binding(),
        installed.binding()
    );
    assert_eq!(harness.host().position(), start + 5120);

    let mut expected = Vec::with_capacity(take.len());
    for offset in 0_u64..5120 {
        let at = start + offset;
        let mut left = [0.5];
        let mut right = [0.5];
        if (install..clear).contains(&at) {
            if (curve..=manual).contains(&at) {
                reference
                    .set_explicit(FilterSettingsChange::CutoffHz(
                        Cutoff::checked(cutoff_at(at, curve)).expect("curve reference cutoff"),
                    ))
                    .expect("curve reference");
            }
            if at == manual {
                reference
                    .configure(FilterSettingsChange::CutoffHz(
                        Cutoff::checked(1_200.0).expect("manual reference"),
                    ))
                    .expect("manual reference law");
            }
            if (curve..=curve + 512).contains(&at) {
                let t = f64::from(u32::try_from(at - curve).expect("local Q frame")) / 512.0;
                let q = if at == curve + 512 {
                    0.8
                } else {
                    0.75 + (0.8 - 0.75) * t
                };
                reference
                    .set_explicit(FilterSettingsChange::Q(Q::checked(q).expect("Q reference")))
                    .expect("independent Q reference");
            }
            if at == reset {
                reference.reset();
            }
            reference
                .process(&mut [&mut left[..], &mut right[..]], 0..1)
                .expect("reference frame");
        }
        expected.extend([left[0], right[0]]);
    }
    assert_ne!(
        expected,
        vec![0.5; expected.len()],
        "the oracle hears causal history, curves and reset"
    );
    assert_eq!(
        take.iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>()
    );
    assert_eq!(tap.drops(), 0);
    underruns.checkpoint(harness.metrics(), true, "Filter final complete edges");
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn filter_tail_and_programme_outlive_source_eof_and_close_while_owned() {
    let (harness, mut underruns) =
        loaded_filter_source(asset_source(&assets::fixed_constant_wav_cd_half_4410())).await;
    let first = render_effect_window(&harness, 64).await;
    assert_eq!(first.len(), 128);
    underruns.checkpoint(harness.metrics(), true, "Filter EOF initial callback");
    assert_eq!(harness.metrics().underruns(), 0);
    let warm = render_effect_window(&harness, 2048).await;
    assert_eq!(&warm[warm.len() - 128..], &[0.5; 128]);
    let settings = FilterSettings::builder()
        .kind(Kind::LowPass)
        .cutoff_hz(Cutoff::checked(1_000.0).expect("cutoff"))
        .build()
        .expect("settings");
    let start = harness.host().position();
    let install = start + 529;
    harness
        .with_queue(move |queue| queue.install_filter(When::At(frame(install)), settings))
        .await
        .expect("install resident filter");
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
    let end = curve + 8192;
    let program = FilterProgram::new(
        FilterParameter::CutoffHz,
        vec![
            point(curve, 200.0, CurveShape::cubic(0.0, 1.0).expect("S shape")),
            point(end, 800.0, CurveShape::HOLD),
        ],
    )
    .expect("programme beyond EOF");
    harness
        .with_queue(move |queue| queue.filter_program(program))
        .await
        .expect("public programme");
    let drained = render_effect_window(&harness, 10_240).await;
    assert_eq!(tap.drain(), drained);
    take.extend(drained);
    let rate = harness.host().spec().await.sample_rate;
    let mut reference = FilterEffect::new(
        AudioSpec::new(2, rate),
        FilterConfig::from((harness.worker().pools().clone(), settings)),
    )
    .expect("reference");
    let mut expected = Vec::with_capacity(take.len());
    for offset in 0..take.len() / 2 {
        let at = start + u64::try_from(offset).expect("capture frame");
        let input = if (source_start..source_end).contains(&at) {
            0.5
        } else {
            0.0
        };
        let (mut left, mut right) = ([input], [input]);
        if at >= install {
            if at >= curve {
                let value = if at >= end {
                    800.0
                } else {
                    let t = f64::from(u32::try_from(at - curve).expect("programme local frame"))
                        / 8192.0;
                    200.0 * 4.0_f64.powf(3.0 * t * t - 2.0 * t * t * t)
                };
                reference
                    .set_explicit(FilterSettingsChange::CutoffHz(
                        Cutoff::checked(value).expect("positive cutoff"),
                    ))
                    .expect("reference point");
            }
            reference
                .process(&mut [&mut left[..], &mut right[..]], 0..1)
                .expect("reference tail");
        }
        expected.extend([left[0], right[0]]);
    }
    assert_eq!(
        take.iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>()
    );
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
    assert_eq!(
        f64::from(
            snapshot
                .filter
                .settings()
                .expect("resident filter")
                .cutoff_hz()
        ),
        800.0
    );
    assert!(snapshot.filter.program(FilterParameter::CutoffHz).is_some());
    let future = harness.host().position() + 529;
    let held = FilterProgram::new(
        FilterParameter::CutoffHz,
        vec![
            point(future, 800.0, CurveShape::HOLD),
            point(future + 8192, 800.0, CurveShape::HOLD),
        ],
    )
    .expect("owned future programme");
    harness
        .with_queue(move |queue| queue.filter_program(held))
        .await
        .expect("programme on empty deck");
    let empty = render_effect_window(&harness, 640).await;
    assert_eq!(tap.drain(), empty);
    assert!(empty.iter().all(|sample| sample.to_bits() == 0));
    assert!(
        harness
            .deck_snapshot()
            .filter
            .program(FilterParameter::CutoffHz)
            .is_some()
    );
    assert!(harness.deck_snapshot().filter.settings().is_some());
    assert_eq!(tap.drops(), 0);
    underruns.checkpoint(harness.metrics(), true, "Filter EOF tail and owned close");
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}
