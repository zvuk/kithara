use anyhow::{Context as _, Result};
use proc_macro2::Span;
use syn::{
    ItemImpl, ItemMod,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{
    exclude::attrs_have_cfg_test, parse::self_ty_name, suppress::Suppressions,
    violation::Violation, walker::relative_to,
};

pub(crate) mod consts {
    pub(crate) const ID: &str = "derivable_event";
    pub(super) const EXPLANATION: &str = "Use #[derive(Event)] instead of a hand-written impl outside the configured Event owner. Each Event is already a single-member EventSet; consumers choose which topics their EventSet combines.";
}

pub(crate) struct DerivableEvent;

impl Check for DerivableEvent {
    fn id(&self) -> &'static str {
        consts::ID
    }

    fn policy(&self) -> super::CheckPolicy {
        super::CheckPolicy::Default
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let config = &ctx.config.thresholds.derivable_event;
        if !config.enabled {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for path in ctx.scan.rs_files(ctx.scope)?.iter() {
            let source = ctx
                .scan
                .source(path)
                .with_context(|| format!("read {}", path.display()))?;
            let Ok(file) = syn::parse_file(&source) else {
                continue;
            };
            let relative = relative_to(ctx.workspace_root, path);
            let suppress = Suppressions::parse(&source);
            let mut visitor = ManualEvents::default();
            visitor.visit_file(&file);
            if !config
                .owner_path
                .as_ref()
                .is_some_and(|owner| relative.starts_with(owner))
            {
                for (name, span) in visitor.manual {
                    if !suppress.is_suppressed(span.start().line, consts::ID) {
                        out.push(
                            Violation::deny(
                                consts::ID,
                                format!("{}::{name}", relative.display()),
                                format!("{name} implements Event by hand outside the configured Event owner"),
                            )
                            .with_explanation(consts::EXPLANATION.into()),
                        );
                    }
                }
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

#[derive(Default)]
struct ManualEvents {
    manual: Vec<(String, Span)>,
}

impl<'ast> Visit<'ast> for ManualEvents {
    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        if !attrs_have_cfg_test(&node.attrs)
            && node.trait_.as_ref().is_some_and(|(path, _)| {
                path.segments
                    .last()
                    .is_some_and(|segment| segment.ident == "Event")
            })
            && let Some(name) = self_ty_name(&node.self_ty)
        {
            self.manual.push((name, node.span()));
        }
    }

    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if !attrs_have_cfg_test(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hand_written_in(source: &str) -> usize {
        let file = syn::parse_file(source).expect("the source parses");
        let mut visitor = ManualEvents::default();
        visitor.visit_file(&file);
        visitor.manual.len()
    }

    #[test]
    fn a_derived_event_is_not_a_hand_written_impl() {
        assert_eq!(
            hand_written_in("#[derive(Clone, Debug, Event)] pub enum FileEvent { Read }"),
            0
        );
    }

    #[test]
    fn a_hand_written_impl_is_reported() {
        assert_eq!(hand_written_in("impl Event for FileEvent {}"), 1);
    }

    #[test]
    fn a_qualified_hand_written_impl_is_reported() {
        assert_eq!(
            hand_written_in("impl ::kithara_events::Event for FileEvent {}"),
            1
        );
    }

    #[test]
    fn an_unrelated_impl_is_ignored() {
        assert_eq!(hand_written_in("impl Clone for FileEvent {}"), 0);
    }

    #[test]
    fn a_hand_written_impl_inside_a_test_module_is_ignored() {
        assert_eq!(
            hand_written_in("#[cfg(test)] mod tests { impl Event for Probe {} }"),
            0
        );
    }
}
