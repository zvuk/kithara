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
