use crate::StretchKind;

#[test]
fn roundtrips_compiled_variants_through_u8() {
    for kind in StretchKind::all().iter().copied() {
        assert_eq!(StretchKind::from(u8::from(kind)), kind);
    }
}

#[test]
fn keeps_stable_discriminants_and_default_decode() {
    #[cfg(feature = "stretch-signalsmith")]
    assert_eq!(u8::from(StretchKind::Signalsmith), 1);

    #[cfg(feature = "stretch-bungee")]
    assert_eq!(u8::from(StretchKind::Bungee), 2);

    let default = StretchKind::all()[0];
    assert_eq!(StretchKind::from(0), default);
    assert_eq!(StretchKind::from(99), default);
}
