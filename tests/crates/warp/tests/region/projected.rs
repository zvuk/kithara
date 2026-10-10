use super::*;

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn rendered_clicks_follow_the_integral_of_the_tempo_ramp(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    use kithara::warp::{BeatGridId, BeatGridRevision, BeatGridSnapshot};

    let source_clicks = click_positions(&mono(&warp_nominal_clicks));
    assert_eq!(
        source_clicks.len(),
        BARS,
        "the fixture contains eight delayed attacks"
    );
    for target_bps in [1.0, 3.0] {
        let anchor = SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::default(),
            2.0,
            spec().sample_rate,
        )
        .expect("anchor")
        .retarget(SessionFrame::new(0), target_bps, 0.5)
        .expect("tempo ramp");
        let target = BeatGridSnapshot::session(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            SessionEpoch::new(0),
            anchor,
            None,
        );
        let plan = plan_over(
            asset_grid_over(
                &[(0.0, f64_of(NOMINAL), i64_of(BARS))],
                None,
                spec().sample_rate,
            ),
            target,
        );
        for keylock in [false, true] {
            let mut partitions = Vec::new();
            for quantum in [64, 257] {
                let config = WarpConfig::builder()
                    .speed(2.0)
                    .keylock(keylock)
                    .backend(backend)
                    .render_quantum_frames(NonZero::new(quantum).expect("quantum"))
                    .build();
                let output = render_configured_grid(
                    config,
                    Some(plan.clone()),
                    &warp_nominal_clicks,
                    Timeline::Anchored(anchor),
                    None,
                );
                let clicks = click_positions(&mono(&output));
                assert_eq!(
                    clicks.len(),
                    BARS,
                    "every beat survives {target_bps}, keylock={keylock}"
                );
                for (ordinal, (actual, source)) in clicks.iter().zip(&source_clicks).enumerate() {
                    let expected = anchor
                        .frame_at(
                            SessionBeat::new(f64_of(*source) / f64_of(NOMINAL)).expect("beat"),
                        )
                        .expect("beat frame");
                    let expected = usize::try_from(i64::from(expected)).expect("positive frame");
                    assert!(
                        actual.abs_diff(expected) <= NOMINAL / 20,
                        "ramp {target_bps}, keylock={keylock}, quantum={quantum}, beat {ordinal}: {actual} vs {expected}"
                    );
                }
                partitions.push((output.len(), clicks));
            }
            assert_eq!(
                partitions[0].0, partitions[1].0,
                "partition-independent output duration"
            );
            for (left, right) in partitions[0].1.iter().zip(&partitions[1].1) {
                assert!(
                    left.abs_diff(*right) <= NOMINAL / 20,
                    "partition-independent beat phase"
                );
            }
        }
    }
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn rendered_beats_follow_deck_tempo_and_ignore_manual_speed(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    let source = warp_nominal_clicks;
    for (bps, interval) in [(2.0, NOMINAL), (1.5, NOMINAL * 4 / 3)] {
        let plan = spaced_plan(
            &[(0.0, f64_of(NOMINAL), i64_of(BARS))],
            f64_of(interval),
            spec().sample_rate,
        );
        let output = render_on_grid(backend, 0.5, Some(plan), &source, bps, None);
        let clicks = click_positions(&mono(&output));
        assert_eq!(
            clicks.len(),
            BARS,
            "every source beat survives {bps} beats/s"
        );
        for pair in clicks.windows(2) {
            let actual = pair[1] - pair[0];
            assert!(
                actual.abs_diff(interval) <= interval / 20,
                "{bps} beats/s: beat interval {actual}, expected {interval}"
            );
        }
    }
}

/// A span the beat pass never marked renders at the rate the projection
/// prescribes, not at the listener's manual speed.
///
/// A pass that stops half way leaves a grid whose segments end before the axis
/// the track declares, and the set answers the rest by extending them. The
/// renderer must take that answer: reading its own target instead would let
/// the recording change tempo at the frontier the pass happened to reach, and
/// a manual speed that disagrees with the projection makes that audible.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn an_unmarked_span_renders_at_the_projected_rate(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    const MARKED: usize = BARS / 2;
    let interval = NOMINAL * 4 / 3;
    let projection = plan_over(
        asset_grid_over(
            &[(0.0, f64_of(NOMINAL), i64_of(MARKED))],
            Some(u64_of(NOMINAL * BARS) + 1),
            spec().sample_rate,
        ),
        session_grid_spaced(f64_of(interval), spec().sample_rate),
    );
    let output = render_on_grid(
        backend,
        2.0,
        Some(projection),
        &warp_nominal_clicks,
        1.5,
        None,
    );
    let clicks = click_positions(&mono(&output));
    assert_eq!(
        clicks.len(),
        BARS,
        "every source beat survives, marked by the pass or extended from it"
    );
    for (index, pair) in clicks.windows(2).enumerate() {
        let actual = pair[1] - pair[0];
        assert!(
            actual.abs_diff(interval) <= interval / 20,
            "beat {index} spans {actual} frames; the projection prescribes {interval}"
        );
    }
}

/// Each queue tempo reaches the Host grid: an asset declared at its own tempo
/// renders beats at the Host beat interval scaled by `asset_bps / host_bps`,
/// whatever the manual speed asks for.
///
/// The Host grid stands at `HOST_BPS`, and every case names an asset tempo the
/// acceptance queue carries, so a rate derived from the wrong member's tempo
/// moves the interval away from its case by more than the shared tolerance.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn host_sync_stretches_each_asset_tempo_onto_the_host_grid(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    const HOST_BPM: f64 = 100.0;
    const HOST_BPS: f64 = 2.0;
    const QUEUE_BPM: [f64; 5] = [124.0, 96.0, 132.0, 74.0, 140.0];

    let source = warp_nominal_clicks;
    for bpm in QUEUE_BPM {
        let host_spacing = f64_of(NOMINAL) * bpm / HOST_BPM;
        let plan = spaced_plan(
            &[(0.0, f64_of(NOMINAL), i64_of(BARS))],
            host_spacing,
            spec().sample_rate,
        );
        let output = render_on_grid(backend, 0.5, Some(plan), &source, HOST_BPS, None);
        let clicks = click_positions(&mono(&output));
        assert_eq!(clicks.len(), BARS, "every source beat survives {bpm} BPM");
        let interval = f64_of(NOMINAL) * bpm / HOST_BPM;
        let tolerance = interval / 20.0;
        for pair in clicks.windows(2) {
            let actual = f64_of(pair[1] - pair[0]);
            assert!(
                (actual - interval).abs() <= tolerance,
                "{bpm} BPM asset on a {HOST_BPM} BPM Host: beat interval {actual}, expected {interval}"
            );
        }
    }
}

/// A renderer that takes a second plan renders the incoming tempo, not the one
/// it held before: the beats fed after the swap keep the second plan's interval.
///
/// A slot outlives the item loaded into it, so the tempo of a departed item must
/// not survive in the renderer that served it.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn a_second_plan_through_one_renderer_keeps_only_its_own_tempo(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    const HOST_BPM: f64 = 100.0;
    const HOST_BPS: f64 = 2.0;
    const LEAVING_BPM: f64 = 124.0;
    const ARRIVING_BPM: f64 = 140.0;

    let source = warp_nominal_clicks;
    let switch_at = NOMINAL * BARS / 2;
    let plan_of = |bpm: f64| {
        spaced_plan(
            &[(0.0, f64_of(NOMINAL), i64_of(BARS))],
            f64_of(NOMINAL) * bpm / HOST_BPM,
            spec().sample_rate,
        )
    };
    let output = render_on_grid(
        backend,
        1.0,
        Some(plan_of(LEAVING_BPM)),
        &source,
        HOST_BPS,
        Some((switch_at, |source_frame, output_frame| {
            let spacing = f64_of(NOMINAL) * ARRIVING_BPM / HOST_BPM;
            plan_over_at(
                asset_grid_over(
                    &[(0.0, f64_of(NOMINAL), i64_of(BARS))],
                    None,
                    spec().sample_rate,
                ),
                session_grid_spaced(spacing, spec().sample_rate),
                f64_of(usize::try_from(source_frame).expect("fixture source frame"))
                    / f64_of(NOMINAL),
                f64_of(output_frame) / spacing,
                SessionFrame::new(i64_of(output_frame)),
            )
        })),
    );
    let clicks = click_positions(&mono(&output));
    assert_eq!(
        clicks.len(),
        BARS,
        "every source beat survives the plan swap"
    );

    let arriving = f64_of(NOMINAL) * ARRIVING_BPM / HOST_BPM;
    let leaving = f64_of(NOMINAL) * LEAVING_BPM / HOST_BPM;
    let tolerance = arriving / 20.0;
    let fed_before_swap = switch_at / NOMINAL;
    for (index, pair) in clicks.windows(2).enumerate().skip(fed_before_swap) {
        let actual = f64_of(pair[1] - pair[0]);
        assert!(
            (actual - arriving).abs() <= tolerance,
            "beat interval {index} after the swap is {actual}, expected {arriving} \
             (the departed {LEAVING_BPM} BPM plan renders {leaving})"
        );
    }
}
