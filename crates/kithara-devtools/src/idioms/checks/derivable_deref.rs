use anyhow::Result;

use super::{Check, Context, derivable_support};
use crate::common::{fix::FixOutcome, violation::Violation};

pub(crate) struct DerivableDeref;

impl Check for DerivableDeref {
    fn id(&self) -> &'static str {
        derivable_support::Kind::Deref.id()
    }
    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        derivable_support::run(
            ctx,
            derivable_support::Kind::Deref,
            ctx.config.thresholds.derivable_deref.enabled,
        )
    }
    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        derivable_support::fix(
            ctx,
            derivable_support::Kind::Deref,
            ctx.config.thresholds.derivable_deref.enabled,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::derivable_support::{Kind, fix_source};

    #[test]
    fn fixes_single_field_and_is_idempotent() {
        let src = "// type\nstruct Values(Vec<u8>);\n// impl\nimpl std::ops::Deref for Values { type Target = Vec<u8>; fn deref(&self) -> &Self::Target { &self.0 } }";
        let (fixed, outcome) = fix_source(src, Kind::Deref).unwrap();
        assert!(fixed.contains("#[derive(derive_more::Deref)]"));
        assert!(!fixed.contains("impl std::ops::Deref"));
        assert!(fixed.contains("// type") && fixed.contains("// impl"));
        assert_eq!(outcome.writes, 1);
        assert_eq!(fix_source(&fixed, Kind::Deref).unwrap().1.writes, 0);
    }

    #[test]
    fn preserves_nested_item_and_field_indentation() {
        let src = "mod m {\n    struct Values {\n        selected: Vec<u8>,\n        other: bool,\n    }\n    impl Deref for Values { type Target = Vec<u8>; fn deref(&self) -> &Self::Target { &self.selected } }\n}\n";
        let fixed = fix_source(src, Kind::Deref).unwrap().0;
        assert!(fixed.contains("    #[derive(derive_more::Deref)]\n    struct Values"));
        assert!(fixed.contains("        #[deref]\n        selected: Vec<u8>"));
    }

    #[test]
    fn rejects_wrong_shape() {
        let src = "struct Values(Vec<u8>); impl Deref for Values { type Target = [u8]; fn deref(&self) -> &Self::Target { self.0.as_slice() } }";
        assert_eq!(fix_source(src, Kind::Deref).unwrap().1.writes, 0);
    }

    #[test]
    fn skips_target_mismatch_cross_file_and_bounds() {
        let mismatch = "struct Values(Vec<u8>); impl Deref for Values { type Target = [u8]; fn deref(&self) -> &Self::Target { &self.0 } }";
        assert!(
            !fix_source(mismatch, Kind::Deref)
                .unwrap()
                .1
                .skipped
                .is_empty()
        );
        let cross = "impl Deref for Values { type Target = Vec<u8>; fn deref(&self) -> &Self::Target { &self.0 } }";
        assert!(!fix_source(cross, Kind::Deref).unwrap().1.skipped.is_empty());
        let bounds = "struct Values<T: Copy>(T); impl<T: Copy> Deref for Values<T> { type Target = T; fn deref(&self) -> &Self::Target { &self.0 } }";
        assert!(
            !fix_source(bounds, Kind::Deref)
                .unwrap()
                .1
                .skipped
                .is_empty()
        );
    }
}
