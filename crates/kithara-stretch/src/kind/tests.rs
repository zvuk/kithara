use kithara_test_utils::kithara;

use crate::{BackendCapabilities, StretchKind};

#[kithara::test(native, flash(false))]
fn roundtrips_compiled_variants_through_u8() {
    for kind in StretchKind::all().iter().copied() {
        assert_eq!(StretchKind::from(u8::from(kind)), kind);
    }
}

#[kithara::test(native, flash(false))]
fn keeps_stable_discriminants_and_default_decode() {
    #[cfg(feature = "stretch-signalsmith")]
    assert_eq!(u8::from(StretchKind::Signalsmith), 1);

    #[cfg(feature = "stretch-bungee")]
    assert_eq!(u8::from(StretchKind::Bungee), 2);

    #[cfg(feature = "stretch-glide")]
    assert_eq!(u8::from(StretchKind::Glide), 3);

    #[cfg(feature = "stretch-identity")]
    {
        assert_eq!(u8::from(StretchKind::Identity), 4);
        assert_eq!(StretchKind::all().last(), Some(&StretchKind::Identity));
    }

    let default = StretchKind::all()[0];
    assert_eq!(StretchKind::from(0), default);
    assert_eq!(StretchKind::from(99), default);
}

#[kithara::test(native, flash(false))]
fn compiled_backends_report_only_renderable_functions() {
    for kind in StretchKind::all().iter().copied() {
        match kind {
            #[cfg(feature = "stretch-identity")]
            StretchKind::Identity => assert!(kind.capabilities().is_empty()),
            #[cfg(feature = "stretch-glide")]
            StretchKind::Glide => assert_eq!(kind.capabilities(), BackendCapabilities::RATE),
            #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
            _ => assert_eq!(
                kind.capabilities(),
                BackendCapabilities::RATE.union(BackendCapabilities::KEYLOCK)
            ),
        }
    }
}

#[kithara::test(native, flash(false))]
fn supporting_selects_backends_that_render_the_functions_in_selector_order() {
    assert_eq!(
        StretchKind::supporting(BackendCapabilities::empty()).collect::<Vec<_>>(),
        StretchKind::all()
    );
    let keylock: &[StretchKind] = &[
        #[cfg(feature = "stretch-signalsmith")]
        StretchKind::Signalsmith,
        #[cfg(feature = "stretch-bungee")]
        StretchKind::Bungee,
    ];
    assert_eq!(
        StretchKind::supporting(BackendCapabilities::KEYLOCK).collect::<Vec<_>>(),
        keylock
    );
}
