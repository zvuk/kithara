use anyhow::Result;
use syn::{
    File, Ident, ItemUse, Path, UseTree,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "firewheel_dsp_facade";

const EXPLANATION: &str = "\
Summary: firewheel's fade curves, A/B mix, smoothing filter and parameter \
smoother reach the workspace only through the `kithara_dsp` facade.

Why: one import path per helper lets `kithara_dsp` replace a re-export with \
its own type in one edit, without touching consumers.

Fix: import from `kithara_dsp::fade` or `kithara_dsp::param`. Node, event and \
buffer APIs stay direct firewheel imports.";

pub(crate) struct FirewheelDspFacade;

impl Check for FirewheelDspFacade {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.firewheel_dsp_facade;
        let forbidden = forbidden_modules(&cfg.modules);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            if cfg.allowed_files.contains(&rel) {
                continue;
            }
            let Some(file) = ctx.parsed_file(&path)? else {
                continue;
            };
            for line in facade_bypass_lines(file, &forbidden) {
                violations.push(
                    Violation::deny(
                        ID,
                        format!("{rel}:{line}"),
                        "firewheel DSP item imported past the `kithara_dsp` facade",
                    )
                    .with_explanation(EXPLANATION),
                );
            }
        }
        Ok(violations)
    }
}

fn forbidden_modules(modules: &[String]) -> Vec<Vec<String>> {
    const ROOTS: [&[&str]; 3] = [&["firewheel"], &["firewheel_core"], &["firewheel", "core"]];
    ROOTS
        .iter()
        .flat_map(|root| {
            modules.iter().map(move |module| {
                root.iter()
                    .map(|segment| (*segment).to_owned())
                    .chain(module.split("::").map(str::to_owned))
                    .collect()
            })
        })
        .collect()
}

fn facade_bypass_lines(file: &File, forbidden: &[Vec<String>]) -> Vec<usize> {
    let mut visitor = FacadeVisitor {
        forbidden,
        lines: Vec::new(),
    };
    visitor.visit_file(file);
    let mut lines = visitor.lines;
    lines.sort_unstable();
    lines.dedup();
    lines
}

struct FacadeVisitor<'a> {
    forbidden: &'a [Vec<String>],
    lines: Vec<usize>,
}

impl FacadeVisitor<'_> {
    fn may_reach(&self, prefix: &[String]) -> bool {
        self.forbidden
            .iter()
            .any(|module| module.starts_with(prefix))
    }

    fn reaches(&self, path: &[String]) -> bool {
        self.forbidden.iter().any(|module| path.starts_with(module))
    }

    fn scan(&mut self, tree: &UseTree, prefix: &mut Vec<String>) {
        match tree {
            UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                if self.reaches(prefix) {
                    self.lines.push(path.ident.span().start().line);
                } else {
                    self.scan(&path.tree, prefix);
                }
                prefix.pop();
            }
            UseTree::Name(name) => self.scan_leaf(prefix, &name.ident),
            UseTree::Rename(rename) => self.scan_leaf(prefix, &rename.ident),
            UseTree::Glob(glob) => {
                if self.may_reach(prefix) {
                    self.lines.push(glob.span().start().line);
                }
            }
            UseTree::Group(group) => {
                for item in &group.items {
                    self.scan(item, prefix);
                }
            }
        }
    }

    fn scan_leaf(&mut self, prefix: &mut Vec<String>, ident: &Ident) {
        prefix.push(ident.to_string());
        if self.reaches(prefix) {
            self.lines.push(ident.span().start().line);
        }
        prefix.pop();
    }
}

impl<'ast> Visit<'ast> for FacadeVisitor<'_> {
    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        self.scan(&item.tree, &mut Vec::new());
    }

    fn visit_path(&mut self, path: &'ast Path) {
        let segments: Vec<String> = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        if self.reaches(&segments)
            && let Some(first) = path.segments.first()
        {
            self.lines.push(first.ident.span().start().line);
        }
        visit::visit_path(self, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::config::FirewheelDspFacadeThreshold;

    fn lines(source: &str) -> Vec<usize> {
        let file = syn::parse_file(source).expect("fixture parses");
        facade_bypass_lines(
            &file,
            &forbidden_modules(&FirewheelDspFacadeThreshold::default().modules),
        )
    }

    #[test]
    fn flags_direct_grouped_and_inline_facade_paths() {
        let source = "\
use firewheel::dsp::fade::FadeCurve;
use firewheel_core::{dsp::mix::Mix, param::smoother::SmootherConfig};
use firewheel::core::dsp::filter::smoothing_filter::MIN_SETTLE_RATIO;
fn f() { let _ = firewheel::param::smoother::SmoothedParam::new; }
";
        assert_eq!(lines(source), [1, 2, 3, 4]);
    }

    #[test]
    fn leaves_node_api_and_unrelated_dsp_modules_alone() {
        let source = "\
use firewheel::node::ProcBuffers;
use firewheel::dsp::volume::Volume;
use firewheel_core::dsp::declick::DeclickValues;
use kithara_dsp::param::SmootherConfig;
";
        assert!(lines(source).is_empty());
    }

    #[test]
    fn flags_a_glob_that_can_reach_a_facade_module() {
        assert_eq!(lines("use firewheel::dsp::*;\n"), [1]);
        assert!(lines("use firewheel::nodes::*;\n").is_empty());
    }
}
