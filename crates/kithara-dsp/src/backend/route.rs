#[cfg(test)]
mod tests {
    use std::mem::{Discriminant, discriminant};

    use fearless_simd::{Level, Simd};
    use kithara_test_utils::kithara;

    use super::{route, routes};

    #[derive(Debug, PartialEq, Eq)]
    pub enum Form {
        Scalar,
        Simd(Discriminant<Level>),
    }

    routes! {
        scalar_probe() -> Form {
            simd: super::native;
            scalar: super::plain;
            entry: super::scalar_entry;
            targets: [Scalar, Scalar, Scalar];
        }
        simd_probe() -> Form {
            simd: super::native;
            scalar: super::plain;
            entry: super::simd_entry;
            targets: [Simd, Simd, Simd];
        }
    }

    fn plain() -> Form {
        Form::Scalar
    }

    fn native<S: Simd>(simd: S) -> Form {
        Form::Simd(discriminant(&simd.level()))
    }

    fn scalar_entry() -> Form {
        route!(tests::scalar_probe => ())
    }

    fn simd_entry() -> Form {
        route!(tests::simd_probe => ())
    }

    #[kithara::test]
    fn entry_executes_the_form_selected_by_its_cell() {
        assert_eq!(scalar_entry(), Form::Scalar);
        assert_eq!(simd_entry(), Form::Simd(discriminant(&Level::new())));
    }
}
