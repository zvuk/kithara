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

    let default = StretchKind::all()[0];
    assert_eq!(StretchKind::from(0), default);
    assert_eq!(StretchKind::from(99), default);
}

#[kithara::test(native, flash(false))]
fn compiled_backends_report_only_renderable_functions() {
    for kind in StretchKind::all().iter().copied() {
        assert!(kind.capabilities().contains(BackendCapabilities::RATE));
        match kind {
            #[cfg(feature = "stretch-glide")]
            StretchKind::Glide => {
                assert!(!kind.capabilities().contains(BackendCapabilities::KEYLOCK));
            }
            #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
            _ => assert!(kind.capabilities().contains(BackendCapabilities::KEYLOCK)),
        }
    }
}
