use anyhow::Result;

use super::{Check, Context, derivable_support};
use crate::common::{fix::FixOutcome, violation::Violation};

pub(crate) struct DerivableFrom;

impl Check for DerivableFrom {
    fn id(&self) -> &'static str {
        derivable_support::Kind::From.id()
    }
    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        derivable_support::run(
            ctx,
            derivable_support::Kind::From,
            ctx.config.thresholds.derivable_from.enabled,
        )
    }
    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        derivable_support::fix(
            ctx,
            derivable_support::Kind::From,
            ctx.config.thresholds.derivable_from.enabled,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::derivable_support::{Kind, fix_source};

    #[test]
    fn fixes_newtype_and_is_idempotent() {
        let src = "// type comment\nstruct Id(u32);\n// impl comment\nimpl From<u32> for Id { fn from(v: u32) -> Self { Self(v) } }\n";
        let (fixed, outcome) = fix_source(src, Kind::From).unwrap();
        assert!(fixed.contains("#[derive(derive_more::From)]"));
        assert!(!fixed.contains("impl From"));
        assert!(fixed.contains("// type comment") && fixed.contains("// impl comment"));
        assert_eq!(outcome.writes, 1);
        let (again, outcome) = fix_source(&fixed, Kind::From).unwrap();
        assert_eq!(again, fixed);
        assert_eq!(outcome.writes, 0);
    }

    #[test]
    fn excludes_cfg_test_module() {
        let src = "#[cfg(test)] mod tests { struct X(u32); impl From<u32> for X { fn from(value: u32) -> Self { Self(value) } } }";
        assert_eq!(fix_source(src, Kind::From).unwrap().1.writes, 0);
    }

    #[test]
    fn merges_existing_derive() {
        let src = "#[derive(Debug)] struct X(u32); impl From<u32> for X { fn from(value: u32) -> Self { Self(value) } }";
        let fixed = fix_source(src, Kind::From).unwrap().0;
        assert!(fixed.contains("#[derive(Debug, derive_more::From)]"));
        assert_eq!(fixed.matches("#[derive(").count(), 1);
    }

    #[test]
    fn preserves_multiline_derive_style() {
        let src = "#[derive(\n    Debug,\n)]\nstruct X(u32);\nimpl From<u32> for X { fn from(value: u32) -> Self { Self(value) } }\n";
        let fixed = fix_source(src, Kind::From).unwrap().0;
        assert!(fixed.contains("    Debug,\n    derive_more::From,\n)]"));
        assert_eq!(fixed.matches("#[derive(").count(), 1);
    }

    #[test]
    fn removes_impl_without_blank_line_residue() {
        let src = "struct X(u32);\n\nimpl From<u32> for X { fn from(value: u32) -> Self { Self(value) } }\n\n\nfn next() {}\n";
        let fixed = fix_source(src, Kind::From).unwrap().0;
        assert!(!fixed.contains("\n\n\n"));
        assert!(fixed.contains("struct X(u32);\n\nfn next()"));
    }

    #[test]
    fn preserves_following_nested_item_indentation() {
        let src = "mod m {\n    struct X(u32);\n    impl From<u32> for X { fn from(value: u32) -> Self { Self(value) } }\n    fn next() {}\n}\n";
        let fixed = fix_source(src, Kind::From).unwrap().0;
        assert!(fixed.contains("    struct X(u32);\n    fn next() {}\n}"));
    }

    #[test]
    fn rejects_transformed_value() {
        let src =
            "struct Id(u32); impl From<u16> for Id { fn from(v: u16) -> Self { Self(v.into()) } }";
        assert_eq!(fix_source(src, Kind::From).unwrap().1.writes, 0);
    }

    #[test]
    fn skips_cross_file_and_bounds_and_cfg_mismatch_and_other_from() {
        let cross = "impl From<u32> for Id { fn from(v: u32) -> Self { Self(v) } }";
        assert!(
            fix_source(cross, Kind::From)
                .unwrap()
                .1
                .skipped
                .iter()
                .any(|s| s.contains("another file"))
        );
        let bounds = "struct Id<T: Copy>(T); impl<T: Copy> From<T> for Id<T> { fn from(v: T) -> Self { Self(v) } }";
        assert!(!fix_source(bounds, Kind::From).unwrap().1.skipped.is_empty());
        let cfg = "#[cfg(unix)] struct Id(u32); #[cfg(windows)] impl From<u32> for Id { fn from(v: u32) -> Self { Self(v) } }";
        assert!(!fix_source(cfg, Kind::From).unwrap().1.skipped.is_empty());
        let conflict = "struct Id(u32); impl From<u32> for Id { fn from(v: u32) -> Self { Self(v) } } impl From<u16> for Id { fn from(v: u16) -> Self { Self(v as u32) } }";
        assert!(
            !fix_source(conflict, Kind::From)
                .unwrap()
                .1
                .skipped
                .is_empty()
        );
    }
}
