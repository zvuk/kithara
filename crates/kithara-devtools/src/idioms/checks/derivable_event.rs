use std::{collections::BTreeSet, fs};

use anyhow::Result;
use proc_macro2::Span;
use syn::{
    Attribute, Fields, ItemEnum, ItemImpl, ItemMod, ItemStruct, Path, Token, Type,
    punctuated::Punctuated,
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
    pub(super) const EXPLANATION: &str = "Every event type must be reachable from the FFI surface. Add its type to an #[derive(EventSet)] enum under crates/kithara-ffi/src, or list it in [derivable_event] unforwarded with the reason it is deliberately internal. Event is implemented only by #[derive(Event)]; a hand-written impl outside kithara-events bypasses the census.";
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
        let mut declared = BTreeSet::new();
        let mut forwarded = BTreeSet::new();
        let mut out = Vec::new();
        for path in ctx.scan.rs_files(ctx.scope)?.iter() {
            let source = fs::read_to_string(path)?;
            let Ok(file) = syn::parse_file(&source) else {
                continue;
            };
            let relative = relative_to(ctx.workspace_root, path);
            let suppress = Suppressions::parse(&source);
            let mut census = Census::default();
            census.visit_file(&file);
            declared.extend(census.events.into_iter().filter_map(|(name, span)| {
                (!suppress.is_suppressed(span.start().line, consts::ID)).then_some(name)
            }));
            if relative.starts_with("crates/kithara-ffi/src") {
                forwarded.extend(census.forwarded);
            }
            if !relative.starts_with("crates/kithara-events") {
                for (name, span) in census.manual {
                    if !suppress.is_suppressed(span.start().line, consts::ID) {
                        out.push(
                            Violation::deny(
                                consts::ID,
                                format!("{}::{name}", relative.display()),
                                format!("{name} implements Event by hand outside kithara-events"),
                            )
                            .with_explanation(consts::EXPLANATION),
                        );
                    }
                }
            }
        }
        for name in declared.difference(&forwarded) {
            if !config.unforwarded.contains(name) {
                out.push(
                    Violation::deny(
                        consts::ID,
                        name.clone(),
                        format!("{name} derives Event but no EventSet forwards it"),
                    )
                    .with_explanation(consts::EXPLANATION),
                );
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

#[derive(Default)]
struct Census {
    events: Vec<(String, Span)>,
    forwarded: Vec<String>,
    manual: Vec<(String, Span)>,
}

impl<'ast> Visit<'ast> for Census {
    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        if attrs_have_cfg_test(&node.attrs) {
            return;
        }
        if derives(&node.attrs, "Event") {
            self.events.push((node.ident.to_string(), node.span()));
        }
        if derives(&node.attrs, "EventSet") {
            for variant in &node.variants {
                if let Fields::Unnamed(fields) = &variant.fields
                    && fields.unnamed.len() == 1
                    && let Some(field) = fields.unnamed.first()
                    && let Type::Path(path) = &field.ty
                    && let Some(segment) = path.path.segments.last()
                {
                    self.forwarded.push(segment.ident.to_string());
                }
            }
        }
        visit::visit_item_enum(self, node);
    }

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

    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        if !attrs_have_cfg_test(&node.attrs) && derives(&node.attrs, "Event") {
            self.events.push((node.ident.to_string(), node.span()));
        }
    }
}

fn derives(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("derive")
            && attr
                .parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)
                .is_ok_and(|paths| {
                    paths.iter().any(|path| {
                        path.segments
                            .last()
                            .is_some_and(|segment| segment.ident == name)
                    })
                })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn census(source: &str) -> Census {
        let file = syn::parse_file(source).expect("the source parses");
        let mut census = Census::default();
        census.visit_file(&file);
        census
    }

    fn events_in(source: &str) -> Vec<String> {
        census(source)
            .events
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    fn forwarded_in(source: &str) -> Vec<String> {
        census(source).forwarded
    }

    fn hand_written_in(source: &str) -> usize {
        census(source).manual.len()
    }

    #[test]
    fn a_derived_event_is_collected() {
        let found = events_in("#[derive(Clone, Debug, Event)] pub enum FileEvent { Read }");

        assert_eq!(found, vec!["FileEvent".to_string()]);
    }

    #[test]
    fn a_type_without_the_derive_is_ignored() {
        let found = events_in("#[derive(Clone, Debug)] pub enum Plain { Read }");

        assert!(found.is_empty());
    }

    #[test]
    fn a_derive_inside_a_test_module_is_ignored() {
        let found = events_in("#[cfg(test)] mod tests { #[derive(Event)] struct Probe; }");

        assert!(found.is_empty());
    }

    #[test]
    fn an_event_set_forwards_each_variant_type() {
        let found = forwarded_in("#[derive(EventSet)] pub enum Set { A(FileEvent), B(HlsEvent) }");

        assert_eq!(found, vec!["FileEvent".to_string(), "HlsEvent".to_string()]);
    }

    #[test]
    fn a_plain_enum_forwards_nothing() {
        let found = forwarded_in("pub enum Set { A(FileEvent) }");

        assert!(found.is_empty());
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
