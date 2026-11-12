use std::num::NonZero;

use kithara_signal::AudioSpec;
use kithara_stretch::StretchKind;
use kithara_test_utils::kithara;

use super::{WarpConfig, spec};
use crate::test_pools::pools_with_budget as test_pools;

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide))]
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
            let config = WarpConfig::builder()
                .speed(0.5)
                .keylock(true)
                .backend(backend)
                .build();
            let target = crate::Warp::new((), &config).renderer(target_spec, pools.clone());
            assert!(target.engine.is_some());
            pools.stats().allocated_bytes
        })
        .into_iter()
        .max()
        .expect("the target matrix is non-empty");

    let pools = test_pools(target_bytes);
    let config = WarpConfig::builder()
        .speed(0.5)
        .keylock(true)
        .backend(backend)
        .build();
    let mut fx = crate::Warp::new((), &config).renderer(initial, pools.clone());
    assert!(fx.engine.is_some());
    assert!(fx.pending_source.is_some());
    assert!(fx.scratch.is_some());

    fx.prepare(rebuilt);

    assert_eq!(fx.spec, rebuilt);
    assert!(fx.engine.is_some());
    assert!(fx.pending_source.is_some());
    assert!(fx.scratch.is_some());

    for target_spec in [initial, rebuilt, initial, rebuilt] {
        fx.prepare(target_spec);
        assert_eq!(fx.spec, target_spec);
        assert!(
            fx.engine.is_some(),
            "every format rebuild fits one target budget"
        );
        assert!(fx.pending_source.is_some());
        assert!(fx.scratch.is_some());
        assert!(pools.stats().allocated_bytes <= target_bytes);
    }
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn failed_target_rebuild_is_not_retried_without_a_new_revision(#[case] backend: StretchKind) {
    let pools = test_pools(0);
    let config = WarpConfig::builder()
        .speed(0.5)
        .keylock(true)
        .backend(backend)
        .build();
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
