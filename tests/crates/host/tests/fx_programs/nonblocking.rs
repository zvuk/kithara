use std::panic::{AssertUnwindSafe, panic_any, resume_unwind};

use futures::FutureExt;
use kithara::{
    abr::AbrMode,
    assets::AssetStore,
    download::{Downloader, DownloaderConfig},
    net::NetOptions,
    platform::{CancelToken, tokio::runtime::Handle},
    play::{ResourceConfig, ResourceSrc},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, SegmentGateHandle, TestServerHelper, usdt_trace,
};
use kithara_test_utils::cancel_token;

use super::{fixture::*, *};
use crate::{
    mix_tap::{ProgramPlan, run_withheld_windows},
    underrun::Underruns,
};

/// A program moving `parameter` from `from` at the next withheld window to
/// `to` [`SEGMENT`] frames later, issued while the deck's input is still
/// withheld.
fn window_program(
    harness: &OfflinePlayer,
    parameter: DeckParameter,
    from: f64,
    to: f64,
) -> DeckProgram {
    let start = harness.host().position();
    let first = start + u64::try_from(FIRST).expect("first offset fits u64");
    let last = first + u64::try_from(SEGMENT).expect("segment fits u64");
    DeckProgram::new(
        parameter,
        vec![
            ParameterKnot::new(frame(first), from, CurveShape::LINEAR).expect("first knot"),
            ParameterKnot::new(frame(last), to, CurveShape::HOLD).expect("last knot"),
        ],
    )
    .expect("a window program")
}

/// A factory producing the effect a false-mode deck installs on FX unit 0.
type EffectFactory = fn() -> Box<dyn DeckEffect>;

/// Build a false-mode starvation deck, install `install` on FX unit 0 when
/// present, and run the three withheld-segment windows, issuing a program on
/// whichever window `on_window` returns one for.
async fn false_mode(
    cancel_token: CancelToken,
    install: Option<(EffectFactory, FxMix)>,
    on_window: impl FnMut(&OfflinePlayer, usize) -> Option<ProgramPlan>,
) {
    let server = TestServerHelper::new().await;
    let hls = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(8)
                .segment_duration_secs(2.0)
                .packaged_audio_aac_lc(44_100, 2),
        )
        .await
        .expect("packaged audio source");
    let gates = [1, 3, 5].map(|segment| server.register_segment_gate(hls.token(), 0, segment));
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(false)
            .fx_units(2)
            .build(),
        44_100,
    )
    .await;
    let pools = harness.worker().pools().clone();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(TestServerHelper::shared_client(
            NetOptions::default(),
            cancel_token.child(),
        ))
        .runtime(Handle::current())
        .build(),
    );
    let config = ResourceConfig::for_src(ResourceSrc::Url(hls.master_url()))
        .downloader(downloader)
        .store(AssetStore::builder(pools).build())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let mut events = harness.subscribe_underruns();
    let item = harness.load_config(config).await;
    let mut raw = Underruns::new(&harness);
    if let Some((effect, mix)) = install {
        harness
            .with_queue(move |queue| {
                queue.fx(
                    When::Next,
                    vec![
                        DeckFxChange::Install {
                            unit: 0,
                            effect: effect(),
                        },
                        DeckFxChange::Mix {
                            unit: 0,
                            value: mix,
                        },
                        DeckFxChange::Power { unit: 0, on: true },
                    ],
                )
            })
            .await
            .expect("the deck installs the effect on unit 0");
    }
    run_withheld_windows(&harness, item, &gates, &mut events, &mut raw, on_window).await;
    harness.close().await;
}

#[kithara::test(tokio, multi_thread)]
async fn a_dry_wet_program_callback_returns_while_its_segment_gate_is_closed(
    cancel_token: CancelToken,
) {
    false_mode(
        cancel_token,
        Some((halving, FxMix::MIN)),
        |harness, index| {
            if index != 0 {
                return None;
            }
            let layout = harness.deck_snapshot().fx[0]
                .layout
                .expect("unit 0 publishes its layout");
            let parameter = DeckParameter::Fx {
                unit: 0,
                layout,
                target: FxTarget::Mix,
            };
            let program = window_program(harness, parameter, 0.0, 0.5);
            Some(ProgramPlan {
                program,
                parameter,
                endpoint: 0.5,
            })
        },
    )
    .await;
}

#[kithara::test(tokio, multi_thread)]
async fn an_effect_parameter_program_callback_returns_while_its_segment_gate_is_closed(
    cancel_token: CancelToken,
) {
    false_mode(cancel_token, Some((tone, FxMix::MAX)), |harness, index| {
        if index != 0 {
            return None;
        }
        let layout = harness.deck_snapshot().fx[0]
            .layout
            .expect("unit 0 publishes its layout");
        let parameter = DeckParameter::Fx {
            unit: 0,
            layout,
            target: FxTarget::Param(0),
        };
        let program = window_program(harness, parameter, 1.0, 1.5);
        Some(ProgramPlan {
            program,
            parameter,
            endpoint: 1.5,
        })
    })
    .await;
}

#[kithara::test(tokio, multi_thread)]
async fn a_trim_program_callback_returns_while_its_segment_gate_is_closed(
    cancel_token: CancelToken,
) {
    false_mode(cancel_token, None, |harness, index| {
        if index != 0 {
            return None;
        }
        let parameter = DeckParameter::Trim;
        let program = window_program(harness, parameter, 1.0, 0.5);
        Some(ProgramPlan {
            program,
            parameter,
            endpoint: 0.5,
        })
    })
    .await;
}

struct ReleaseSegment(SegmentGateHandle);

impl Drop for ReleaseSegment {
    fn drop(&mut self) {
        self.0.release();
    }
}

async fn observe_pcm_wait(
    harness: OfflinePlayer,
    release: ReleaseSegment,
    holds: impl FnMut(&[usdt_trace::ProbeEvent]) -> bool,
) -> std::thread::Result<bool> {
    let gate = release.0.clone();
    let trace = usdt_trace::scope();
    let (observed, rendered) = {
        // Three seconds reaches past segment 0 into the withheld segment 1.
        let render = AssertUnwindSafe(harness.render(44_100 * 3)).catch_unwind();
        kithara::platform::tokio::pin!(render);
        let first = kithara::platform::tokio::select! {
            rendered = &mut render => Err(rendered),
            observed = AssertUnwindSafe(trace.wait_for(holds)).catch_unwind() => Ok(observed),
        };
        drop(release);
        match first {
            Ok(observed) => (observed.map(|()| true), render.await),
            Err(rendered) => (Ok(false), rendered),
        }
    };
    let truncated = usdt_trace::recorded().1;
    drop(trace);
    let closed = AssertUnwindSafe(harness.close()).catch_unwind().await;
    // Preserve a render panic if closing its failed owner also panics.
    let output = rendered.unwrap_or_else(|failure| resume_unwind(failure));
    closed.unwrap_or_else(|failure| resume_unwind(failure));
    assert_eq!(
        output.len(),
        44_100 * 3 * 2,
        "the released render joins fully"
    );
    assert!(
        gate.requested() > 0,
        "the observed PCM wait owns the withheld segment"
    );
    assert!(!truncated, "the observed wait trace is complete");
    observed
}

async fn blocked_control(
    cancel_token: CancelToken,
    holds: impl FnMut(&[usdt_trace::ProbeEvent]) -> bool,
) -> std::thread::Result<bool> {
    let server = TestServerHelper::new().await;
    let hls = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(8)
                .segment_duration_secs(2.0)
                .packaged_audio_aac_lc(44_100, 2),
        )
        .await
        .expect("packaged audio source");
    let release = ReleaseSegment(server.register_segment_gate(hls.token(), 0, 1));
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .build(),
        44_100,
    )
    .await;
    let pools = harness.worker().pools().clone();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(TestServerHelper::shared_client(
            NetOptions::default(),
            cancel_token.child(),
        ))
        .runtime(Handle::current())
        .build(),
    );
    let config = ResourceConfig::for_src(ResourceSrc::Url(hls.master_url()))
        .downloader(downloader)
        .store(AssetStore::builder(pools).build())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    harness.load_config(config).await;
    observe_pcm_wait(harness, release, holds).await
}

/// `Scope::wait_for` requires real time while the render owner is blocked.
#[kithara::test(tokio, multi_thread, flash(false))]
async fn blocked_underrun_observes_its_pcm_wait(cancel_token: CancelToken) {
    let observed = blocked_control(cancel_token, |events| {
        events.iter().any(|event| event.probe == "pcm_wait_started")
    })
    .await
    .unwrap_or_else(|failure| resume_unwind(failure));
    assert!(
        observed,
        "the blocked deck fires its PCM wait before render completes"
    );
}

/// An observer panic still releases input and joins the blocked render.
#[kithara::test(tokio, multi_thread, flash(false))]
async fn a_failed_pcm_wait_observer_still_releases_and_joins_render(cancel_token: CancelToken) {
    struct ObservationFailure;

    let observed = blocked_control(cancel_token, |events| {
        if events.iter().any(|event| event.probe == "pcm_wait_started") {
            panic_any(ObservationFailure);
        }
        false
    })
    .await
    .expect_err("the observer panics while render is blocked");
    assert!(
        observed.is::<ObservationFailure>(),
        "preserve the observer failure"
    );
}
