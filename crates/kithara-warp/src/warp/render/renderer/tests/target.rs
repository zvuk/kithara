use std::num::NonZero;

#[cfg(all(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use kithara_platform::sync::Arc;
use kithara_signal::AudioSpec;
use kithara_stretch::StretchKind;
use kithara_test_utils::kithara;

use super::{StretchControls, WarpConfig, spec};
#[cfg(all(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use super::{chunk, chunk_at, render_serviced, renderer, sine};
use crate::test_pools::pools_with_budget as test_pools;

/// Backend selection changes only at an explicit renderer lifecycle boundary.
#[cfg(all(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[case::bungee_to_signalsmith_active(StretchKind::Bungee, StretchKind::Signalsmith, 0.5)]
#[case::signalsmith_to_bungee_active(StretchKind::Signalsmith, StretchKind::Bungee, 0.5)]
#[case::bungee_to_signalsmith_unity(StretchKind::Bungee, StretchKind::Signalsmith, 1.0)]
#[case::signalsmith_to_bungee_unity(StretchKind::Signalsmith, StretchKind::Bungee, 1.0)]
fn backend_change_waits_for_reset(
    #[case] initial: StretchKind,
    #[case] replacement: StretchKind,
    #[case] swap_speed: f32,
) {
    let controls = StretchControls::new(0.5);
    controls.set_keylock(true);
    controls.set_backend(initial);
    let mut fx = renderer(Arc::clone(&controls));
    let pools = fx.pools.clone();
    let block = sine(4096);
    let _ = render_serviced(&mut fx, chunk(&pools, &block));
    controls.set_speed(swap_speed);
    let _ = render_serviced(&mut fx, chunk_at(&pools, &block, 4096));
    let admitted = fx.source_frames_admitted;

    controls.set_backend(replacement);
    fx.prepare(spec());

    assert_eq!(fx.current_kind, initial);
    assert!(fx.active);
    assert_eq!(fx.source_frames_admitted, admitted);

    fx.reset();
    fx.prepare(spec());
    assert_eq!(fx.current_kind, replacement);
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn target_rebuild_reuses_one_target_pool_budget(#[case] backend: StretchKind) {
    let initial = spec();
    let rebuilt = AudioSpec {
        sample_rate: NonZero::new(48_000).unwrap(),
        ..initial
    };
    let target_bytes = [initial, rebuilt]
        .map(|target_spec| {
            let pools = test_pools(usize::MAX);
            let controls = StretchControls::new(0.5);
            controls.set_keylock(true);
            controls.set_backend(backend);
            let config = WarpConfig::builder().stretch(controls).build();
            let target = crate::Warp::new((), &config).renderer(target_spec, pools.clone());
            assert!(target.engine.is_some());
            pools.stats().allocated_bytes
        })
        .into_iter()
        .max()
        .expect("the target matrix is non-empty");

    let pools = test_pools(target_bytes);
    let controls = StretchControls::new(0.5);
    controls.set_keylock(true);
    controls.set_backend(backend);
    let config = WarpConfig::builder().stretch(controls).build();
    let mut fx = crate::Warp::new((), &config).renderer(initial, pools.clone());
    assert!(fx.engine.is_some());
    assert!(fx.pending_source.is_some());
    assert!(fx.scratch.is_some());

    fx.prepare(rebuilt);

    assert_eq!(fx.spec, rebuilt);
    assert!(fx.engine.is_some());
    assert!(fx.pending_source.is_some());
    assert!(fx.scratch.is_some());
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn failed_target_rebuild_is_not_retried_without_a_new_revision(#[case] backend: StretchKind) {
    let pools = test_pools(0);
    let controls = StretchControls::new(0.5);
    controls.set_keylock(true);
    controls.set_backend(backend);
    let config = WarpConfig::builder().stretch(controls).build();
    let mut fx = crate::Warp::new((), &config).renderer(spec(), pools.clone());
    assert!(fx.engine.is_none());

    fx.rebuild_pending = true;
    fx.prepare(spec());
    assert!(!fx.rebuild_pending);

    for _ in 0..8 {
        fx.prepare(spec());
    }
    assert!(fx.engine.is_none());
    assert!(!fx.rebuild_pending);
}
