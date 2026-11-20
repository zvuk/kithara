use std::mem::{Discriminant, discriminant};

use fearless_simd::{Level, Simd};
use kithara_test_utils::kithara;

use super::mechanism::{route, routes};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Form {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    Platform,
    Scalar,
    Simd(Discriminant<Level>),
}

routes! {
    scalar_probe() -> super::Form {
        simd: super::native;
        scalar: super::plain;
        entry: super::scalar_entry;
        targets: [Scalar, Scalar, Scalar];
    }
    simd_probe() -> super::Form {
        simd: super::native;
        scalar: super::plain;
        entry: super::simd_entry;
        targets: [Simd, Simd, Simd];
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
routes! {
    platform_probe(samples: &mut [f32]) -> super::Form {
        simd: super::native_pcm;
        platform: super::accelerated;
        entry: super::platform_entry;
        targets: [Platform, Platform, Platform];
    }
}

fn plain() -> Form {
    Form::Scalar
}

fn native<S: Simd>(simd: S) -> Form {
    Form::Simd(discriminant(&simd.level()))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn native_pcm<S: Simd>(simd: S, _: &mut [f32]) -> Form {
    native(simd)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn accelerated(samples: &mut [f32]) -> Form {
    assert_eq!(
        crate::backend::accelerate::affine(samples, 2.0, 1.0),
        samples.len()
    );
    Form::Platform
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn platform_entry(samples: &mut [f32]) -> Form {
    route!(platform_probe => (samples))
}

fn scalar_entry() -> Form {
    route!(scalar_probe => ())
}

fn simd_entry() -> Form {
    route!(simd_probe => ())
}

#[kithara::test]
fn entry_executes_the_form_selected_by_its_cell() {
    assert_eq!(scalar_entry(), Form::Scalar);
    assert_eq!(simd_entry(), Form::Simd(discriminant(&Level::new())));
    assert!(scalar_probe::routes().any(|(name, entry)| name == "entry" && entry() == Form::Scalar));
    assert!(
        simd_probe::routes()
            .any(|(name, entry)| name == "entry"
                && entry() == Form::Simd(discriminant(&Level::new())))
    );
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[kithara::test]
fn platform_cell_executes_the_accelerate_form() {
    let mut samples = [0.25, -0.5, 1.0];
    assert_eq!(platform_entry(&mut samples), Form::Platform);
    assert_eq!(
        samples.map(f32::to_bits),
        [1.5_f32, 0.0, 3.0].map(f32::to_bits)
    );
    let (_, entry) = platform_probe::routes()
        .find(|(name, _)| *name == "entry")
        .expect("public entry row");
    let mut samples = [0.25, -0.5, 1.0];
    assert_eq!(entry(&mut samples), Form::Platform);
    assert_eq!(
        samples.map(f32::to_bits),
        [1.5_f32, 0.0, 3.0].map(f32::to_bits)
    );
}

// Generic-arm fixtures: the arm forwards the exact declared args (no implicit S). The opaque-state
// forms take the same args as the entry; the prepared-S forms declare S in both generics and args.
pub(crate) mod gen_probe {
    use std::mem::{Discriminant, discriminant};

    use fearless_simd::Simd;

    use super::route;
    pub(crate) struct State<const C: usize> {
        pub(crate) form: Option<Discriminant<Form>>,
        pub(crate) _p: [f32; C],
    }
    #[derive(Debug)]
    pub(crate) enum Form {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        Platform,
        Scalar,
        Simd,
    }
    pub(crate) fn unit<const C: usize, I: std::ops::Deref<Target = [f32]>>(
        s: &mut State<C>,
        _i: &[I; C],
    ) {
        route!(super::unit_probe => (s, _i));
    }
    pub(crate) fn unit_scalar<const C: usize, I: std::ops::Deref<Target = [f32]>>(
        s: &mut State<C>,
        _i: &[I; C],
    ) {
        s.form = Some(discriminant(&Form::Scalar));
    }
    #[inline(always)]
    pub(crate) fn unit_simd<const C: usize, I: std::ops::Deref<Target = [f32]>>(
        s: &mut State<C>,
        _i: &[I; C],
    ) {
        s.form = Some(discriminant(&Form::Simd));
    }
    pub(crate) fn boolean<const C: usize, I: std::ops::Deref<Target = [f32]>>(
        s: &mut State<C>,
        _i: &[I; C],
    ) -> bool {
        route!(super::bool_probe => (s, _i))
    }
    pub(crate) fn boolean_scalar<const C: usize, I: std::ops::Deref<Target = [f32]>>(
        s: &mut State<C>,
        _i: &[I; C],
    ) -> bool {
        s.form = Some(discriminant(&Form::Scalar));
        true
    }
    #[inline(always)]
    pub(crate) fn boolean_simd<const C: usize, I: std::ops::Deref<Target = [f32]>>(
        s: &mut State<C>,
        _i: &[I; C],
    ) -> bool {
        s.form = Some(discriminant(&Form::Simd));
        false
    }
    // The entry obtains the prepared S via the ordinary outer dispatch once, then routes with it.
    pub(crate) fn prepared_entry<S: Simd>(prepared: S, state: &mut State<2>) -> bool {
        route!(super::prepared_probe => (prepared, state))
    }
    pub(crate) fn prepared_scalar<S: Simd>(_prepared: S, state: &mut State<2>) -> bool {
        state.form = Some(discriminant(&Form::Scalar));
        true
    }
    #[inline(always)]
    pub(crate) fn prepared_simd<S: Simd>(_prepared: S, state: &mut State<2>) -> bool {
        state.form = Some(discriminant(&Form::Simd));
        false
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) fn platform_bool<S: Simd>(_prepared: S, state: &mut State<2>) -> bool {
        state.form = Some(discriminant(&Form::Platform));
        true
    }
    pub(crate) fn platform_entry<S: Simd>(prepared: S, state: &mut State<2>) -> bool {
        route!(super::gen_platform_probe => (prepared, state))
    }
}

routes! { @export
    unit_probe[const C: usize, I: std::ops::Deref<Target = [f32]>](
        s: &mut super::gen_probe::State<C>,
        i: &[I; C],
    ) -> () {
        simd: super::gen_probe::unit_simd;
        scalar: super::gen_probe::unit_scalar;
        entry: super::gen_probe::unit;
        targets: [Scalar, Simd, Simd];
    }
    bool_probe[const C: usize, I: std::ops::Deref<Target = [f32]>](
        s: &mut super::gen_probe::State<C>,
        i: &[I; C],
    ) -> bool {
        simd: super::gen_probe::boolean_simd;
        scalar: super::gen_probe::boolean_scalar;
        entry: super::gen_probe::boolean;
        targets: [Scalar, Simd, Simd];
    }
    prepared_probe[S: fearless_simd::Simd](
        prepared: S,
        state: &mut super::gen_probe::State<2>,
    ) -> bool {
        simd: super::gen_probe::prepared_simd;
        scalar: super::gen_probe::prepared_scalar;
        entry: super::gen_probe::prepared_entry;
        targets: [Scalar, Simd, Simd];
    }
    gen_platform_probe[S: fearless_simd::Simd](
        prepared: S,
        state: &mut super::gen_probe::State<2>,
    ) -> bool {
        simd: super::gen_probe::prepared_simd;
        scalar: super::gen_probe::prepared_scalar;
        platform: super::gen_probe::platform_bool;
        entry: super::gen_probe::platform_entry;
        targets: [Platform, Scalar, Scalar];
    }
}

#[kithara::test]
fn generic_unit_entry_routes_to_selected_form() {
    use std::mem::discriminant;
    let mut s = gen_probe::State::<2> {
        form: None,
        _p: [0.0; 2],
    };
    let i = [vec![1.0f32, 2.0], vec![3.0, 4.0]];
    gen_probe::unit(&mut s, &i);
    #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
    assert_eq!(s.form, Some(discriminant(&gen_probe::Form::Scalar)));
    #[cfg(not(all(target_vendor = "apple", target_arch = "aarch64")))]
    assert_eq!(s.form, Some(discriminant(&gen_probe::Form::Simd)));
}

#[kithara::test]
fn generic_bool_entry_routes_to_selected_form() {
    use std::mem::discriminant;
    let mut s = gen_probe::State::<2> {
        form: None,
        _p: [0.0; 2],
    };
    let i = [vec![1.0f32, 2.0], vec![3.0, 4.0]];
    let r = gen_probe::boolean(&mut s, &i);
    #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
    {
        assert!(r, "apple scalar returns true");
        assert_eq!(s.form, Some(discriminant(&gen_probe::Form::Scalar)));
    }
    #[cfg(not(all(target_vendor = "apple", target_arch = "aarch64")))]
    {
        assert!(!r, "non-apple simd returns false");
        assert_eq!(s.form, Some(discriminant(&gen_probe::Form::Simd)));
    }
}

// Prepared-S witness: entry obtains S via ordinary outer dispatch once, then routes with it.
#[kithara::test]
fn generic_prepared_s_entry_routes_to_selected_form() {
    use std::mem::discriminant;
    let mut s = gen_probe::State::<2> {
        form: None,
        _p: [0.0; 2],
    };
    let r = fearless_simd::dispatch!(
        Level::new(),
        simd => gen_probe::prepared_entry(simd, &mut s)
    );
    #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
    {
        assert!(r, "apple scalar returns true");
        assert_eq!(s.form, Some(discriminant(&gen_probe::Form::Scalar)));
    }
    #[cfg(not(all(target_vendor = "apple", target_arch = "aarch64")))]
    {
        assert!(!r, "non-apple simd returns false");
        assert_eq!(s.form, Some(discriminant(&gen_probe::Form::Simd)));
    }
}

// Platform portability: Apple-aarch64 executes the real platform form; every other target
// compiles without the platform path and selects Scalar.
#[kithara::test]
fn generic_platform_entry_routes_to_selected_form() {
    use std::mem::discriminant;
    let mut s = gen_probe::State::<2> {
        form: None,
        _p: [0.0; 2],
    };
    let r = fearless_simd::dispatch!(
        Level::new(),
        simd => gen_probe::platform_entry(simd, &mut s)
    );
    assert!(r, "platform and scalar forms return true");
    #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
    assert_eq!(s.form, Some(discriminant(&gen_probe::Form::Platform)));
    #[cfg(not(all(target_vendor = "apple", target_arch = "aarch64")))]
    assert_eq!(s.form, Some(discriminant(&gen_probe::Form::Scalar)));
}

// Sibling-access witness: a sibling module reaches the crate-private generic invoke directly.
pub(crate) mod gen_bridge {
    pub(crate) fn run_prepared<S: fearless_simd::Simd>(
        prepared: S,
        state: &mut super::gen_probe::State<2>,
    ) -> bool {
        super::prepared_probe::invoke(prepared, state)
    }
}

#[kithara::test]
fn generic_sibling_bridge_reaches_pub_crate_forms() {
    let mut s = gen_probe::State::<2> {
        form: None,
        _p: [0.0; 2],
    };
    let _ = fearless_simd::dispatch!(
        Level::new(),
        simd => gen_bridge::run_prepared(simd, &mut s)
    );
}
