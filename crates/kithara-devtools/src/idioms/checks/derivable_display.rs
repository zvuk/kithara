use anyhow::Result;

use super::{Check, Context, derivable_support};
use crate::common::{fix::FixOutcome, violation::Violation};

pub(crate) struct DerivableDisplay;

impl Check for DerivableDisplay {
    fn id(&self) -> &'static str {
        derivable_support::Kind::Display.id()
    }
    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        derivable_support::run(
            ctx,
            derivable_support::Kind::Display,
            ctx.config.thresholds.derivable_display.enabled,
        )
    }
    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        derivable_support::fix(
            ctx,
            derivable_support::Kind::Display,
            ctx.config.thresholds.derivable_display.enabled,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::derivable_support::{Kind, fix_source};

    #[test]
    fn fixes_format_and_is_idempotent() {
        let src = "// type\nstruct Pair { left: u8, right: u8 }\n// impl\nimpl std::fmt::Display for Pair { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, \"{}:{}\", self.left, self.right) } }";
        let (fixed, outcome) = fix_source(src, Kind::Display).unwrap();
        assert!(fixed.contains("#[derive(derive_more::Display)]"));
        assert!(fixed.contains("#[display(\"{}:{}\", left, right)]"));
        assert!(!fixed.contains("impl std::fmt::Display"));
        assert!(fixed.contains("// type") && fixed.contains("// impl"));
        assert_eq!(outcome.writes, 1);
        assert_eq!(fix_source(&fixed, Kind::Display).unwrap().1.writes, 0);
    }

    #[test]
    fn inserts_derive_below_doc_comment() {
        let src = "/// doc\npub struct Name(String);\nimpl Display for Name { fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result { self.0.fmt(f) } }\n";
        let fixed = fix_source(src, Kind::Display).unwrap().0;
        let doc = fixed.find("/// doc").unwrap();
        let derive = fixed.find("#[derive(derive_more::Display)]").unwrap();
        let display = fixed.find("#[display(\"{_0}\")]").unwrap();
        let item = fixed.find("pub struct Name").unwrap();
        assert!(doc < derive && derive < display && display < item);
        assert!(fixed.contains("#[display(\"{_0}\")]\npub struct Name"));
    }

    #[test]
    fn fixes_transparent_forward() {
        let src = "struct Name(String); impl Display for Name { fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result { self.0.fmt(f) } }";
        assert!(
            fix_source(src, Kind::Display)
                .unwrap()
                .0
                .contains("#[display(\"{_0}\")]")
        );
    }

    #[test]
    fn rejects_multiple_statements() {
        let src = "struct Name(String); impl Display for Name { fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result { prepare(); write!(f, \"{}\", self.0) } }";
        assert_eq!(fix_source(src, Kind::Display).unwrap().1.writes, 0);
    }

    #[test]
    fn skips_enum_cross_file_and_bounds() {
        let en = "enum E { A } impl Display for E { fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result { f.write_str(\"A\") } }";
        assert!(!fix_source(en, Kind::Display).unwrap().1.skipped.is_empty());
        let cross = "impl Display for Name { fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result { f.write_str(\"x\") } }";
        assert!(
            !fix_source(cross, Kind::Display)
                .unwrap()
                .1
                .skipped
                .is_empty()
        );
        let bounds = "struct Name<T: Display>(T); impl<T: Display> Display for Name<T> { fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result { write!(f, \"{}\", self.0) } }";
        assert!(
            !fix_source(bounds, Kind::Display)
                .unwrap()
                .1
                .skipped
                .is_empty()
        );
    }
}
