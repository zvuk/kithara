use std::fs;

use anyhow::Result;
use quote::ToTokens;
use syn::{Attribute, visit, visit::Visit};

use super::{Check, Context};
use crate::{
    common::{violation::Violation, walker::relative_to},
    idioms::config::DerivableSeverity,
};

pub(crate) struct DerivablePatch;

impl Check for DerivablePatch {
    fn id(&self) -> &'static str {
        "derivable_patch"
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let config = &ctx.config.thresholds.derivable_patch;
        if !config.enabled {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for path in ctx.scan.rs_files(ctx.scope)?.iter() {
            let source = fs::read_to_string(path)?;
            let relative = relative_to(ctx.workspace_root, path).to_string_lossy();
            for line in check_source(&source) {
                let key = format!("{relative}:{line}:0");
                let message = "verbose humantime patch attribute: use #[patch(humantime)]";
                out.push(match config.severity {
                    DerivableSeverity::Deny => Violation::deny(self.id(), key, message),
                    DerivableSeverity::Warn => Violation::warn(self.id(), key, message),
                });
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

fn check_source(source: &str) -> Vec<usize> {
    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };
    let mut visitor = PatchVisitor::default();
    visitor.visit_file(&file);
    visitor.findings
}

#[derive(Default)]
struct PatchVisitor {
    findings: Vec<usize>,
}

impl<'ast> Visit<'ast> for PatchVisitor {
    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        const VERBOSE_HUMANTIME: &str =
            "patch (attribute (serde (with = \"kithara_config::duration::option\")))";

        if attribute.meta.to_token_stream().to_string() == VERBOSE_HUMANTIME {
            self.findings.push(attribute.pound_token.span.start().line);
        }
        visit::visit_attribute(self, attribute);
    }
}

#[cfg(test)]
mod tests {
    use super::check_source;

    #[test]
    fn reports_verbose_humantime_attribute() {
        assert_eq!(check_source("struct C { #[patch(attribute(serde(with = \"kithara_config::duration::option\")))] timeout: Duration }").len(), 1);
    }

    #[test]
    fn ignores_other_forwarded_attributes() {
        assert!(
            check_source(
                "struct C { #[patch(attribute(serde(rename = \"wait\")))] timeout: Duration }"
            )
            .is_empty()
        );
    }
}
