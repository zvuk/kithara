use anyhow::Result;
use syn::{
    Attribute, Field, ImplItemFn, ItemConst, ItemEnum, ItemFn, ItemMod, ItemStatic, ItemStruct,
    Local, Pat, PatIdent, Variant,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    suppress::Suppressions,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "retry_fallback";

struct Consts;

impl Consts {
    const FORBIDDEN_IDENTS: &'static [&'static str] = &[
        "attempt",
        "attempts",
        "retry",
        "retries",
        "retry_count",
        "retry_limit",
        "max_attempts",
        "max_retries",
        "fallback",
        "fall_back",
    ];

    const FORBIDDEN_SUBSTRINGS: &'static [&'static str] = &[
        "_retry",
        "_retries",
        "_with_retries",
        "_with_retry",
        "_with_fallback",
        "_or_fallback",
        "_or_retry",
        "try_or_",
        "try_then_",
        "fallback_",
    ];
}

const EXPLANATION: &str = "\
Detected a retry/attempt counter or try-then-fallback chain. Both \
patterns paper over a broken primary path: if the first call may fail, \
fix the contract — don't hide the bug behind N attempts or a chain of \
alternative implementations.

Why it matters. `attempt`, `retries`, `max_retries`, and `fallback` \
fields turn a single algorithmic failure into a per-call lottery. The \
test that fails 1-in-10 with a retry-3 wrapper hides a real race; the \
production path that 'falls back to B if A fails' double-encodes the \
problem (A is wrong, B is also wrong, the contract is wrong). They \
also accumulate: each new attempt grows the surface for new races, \
each fallback hides another underlying failure mode.

❌  if request.attempt == 0 { try_seek() } else if request.attempt < MAX { retry() }
✅  fix `try_seek` so it always lands or returns a typed error the caller \
   handles deterministically.

❌  fn read_or_fallback(...) -> Bytes { read_primary().unwrap_or_else(read_secondary) }
✅  pick one source (or model the choice as user-facing config), don't \
   chain implementations.

Fields in `*Retrying` and `*RetryExhausted` variants of `*Event` enums are \
observational telemetry, not retry control state.

Suppress with `// xtask-lint-ignore: retry_fallback` ONLY for legitimate \
user-facing defaults (e.g. a config field literally named `fallback_url` \
where the user opted in to two endpoints). Suppression for control flow \
is a code smell that should be discussed and fixed, not silenced.";

pub(crate) struct RetryFallback;

impl Check for RetryFallback {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.retry_fallback;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel_path = relative_to(ctx.workspace_root, &path).to_path_buf();
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if matches_any(&exempt, std::path::Path::new(&rel)) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let suppress = Suppressions::parse(&source);
            let mut v = IdentVisitor {
                rel: &rel,
                suppress: &suppress,
                out: &mut violations,
                inside_event_enum: false,
                inside_retry_event_variant: false,
                inside_test_mod: false,
            };
            v.visit_file(&file);
        }
        Ok(violations)
    }
}

struct IdentVisitor<'a> {
    suppress: &'a Suppressions,
    out: &'a mut Vec<Violation>,
    rel: &'a str,
    inside_event_enum: bool,
    inside_retry_event_variant: bool,
    /// `true` while traversing inside a `#[cfg(test)]` module — test code
    /// can legitimately use names like `flags_max_retries_const` that
    /// describe the rule's own behaviour without smelling like a retry.
    inside_test_mod: bool,
}

/// Returns `true` if any of the given attributes is `#[cfg(test)]`.
fn is_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        if !a.path().is_ident("cfg") {
            return false;
        }
        let mut is_test = false;
        let _ = a.parse_nested_meta(|m| {
            if m.path.is_ident("test") {
                is_test = true;
            }
            Ok(())
        });
        is_test
    })
}

impl<'a> IdentVisitor<'a> {
    fn flag(&mut self, line: usize, name: &str, kind: &str) {
        if self.inside_test_mod || self.suppress.is_suppressed(line, ID) {
            return;
        }
        let key = format!("{}:{line}:{name}", self.rel);
        let message = format!(
            "{rel}:{line} — {kind} `{name}` encodes a retry/fallback. Fix \
             the primary contract instead of counting attempts or chaining \
             alternatives.",
            rel = self.rel,
            line = line,
            kind = kind,
            name = name,
        );
        self.out
            .push(Violation::deny(ID, key, message).with_explanation(EXPLANATION));
    }
}

fn name_is_forbidden(name: &str) -> bool {
    let lower = name.to_lowercase();
    if Consts::FORBIDDEN_IDENTS
        .iter()
        .any(|forbidden| lower == *forbidden)
    {
        return true;
    }
    Consts::FORBIDDEN_SUBSTRINGS
        .iter()
        .any(|sub| lower.contains(*sub))
}

fn is_retry_event_variant(name: &str) -> bool {
    name.ends_with("Retrying") || name.ends_with("RetryExhausted")
}

impl<'ast> Visit<'ast> for IdentVisitor<'_> {
    fn visit_field(&mut self, node: &'ast Field) {
        if let Some(ident) = &node.ident
            && !self.inside_retry_event_variant
        {
            let name = ident.to_string();
            if name_is_forbidden(&name) {
                self.flag(ident.span().start().line, &name, "field");
            }
        }
        visit::visit_field(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        let name = node.sig.ident.to_string();
        if name_is_forbidden(&name) {
            self.flag(node.sig.ident.span().start().line, &name, "fn");
        }
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        let was_inside = self.inside_event_enum;
        self.inside_event_enum = node.ident.to_string().ends_with("Event");
        visit::visit_item_enum(self, node);
        self.inside_event_enum = was_inside;
    }

    fn visit_item_const(&mut self, node: &'ast ItemConst) {
        let name = node.ident.to_string();
        if name_is_forbidden(&name) {
            self.flag(node.ident.span().start().line, &name, "const");
        }
        visit::visit_item_const(self, node);
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let name = node.sig.ident.to_string();
        if name_is_forbidden(&name) {
            self.flag(node.sig.ident.span().start().line, &name, "fn");
        }
        visit::visit_item_fn(self, node);
    }

    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        let was_inside = self.inside_test_mod;
        if is_cfg_test(&node.attrs) {
            self.inside_test_mod = true;
        }
        visit::visit_item_mod(self, node);
        self.inside_test_mod = was_inside;
    }

    fn visit_item_static(&mut self, node: &'ast ItemStatic) {
        let name = node.ident.to_string();
        if name_is_forbidden(&name) {
            self.flag(node.ident.span().start().line, &name, "static");
        }
        visit::visit_item_static(self, node);
    }

    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        let name = node.ident.to_string();
        if name_is_forbidden(&name) {
            self.flag(node.ident.span().start().line, &name, "struct");
        }
        visit::visit_item_struct(self, node);
    }

    fn visit_local(&mut self, node: &'ast Local) {
        if let Pat::Ident(PatIdent { ident, .. }) = &node.pat {
            let name = ident.to_string();
            if name_is_forbidden(&name) {
                self.flag(ident.span().start().line, &name, "let");
            }
        }
        visit::visit_local(self, node);
    }

    fn visit_variant(&mut self, node: &'ast Variant) {
        let was_inside = self.inside_retry_event_variant;
        self.inside_retry_event_variant =
            self.inside_event_enum && is_retry_event_variant(&node.ident.to_string());
        visit::visit_variant(self, node);
        self.inside_retry_event_variant = was_inside;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_violations(source: &str) -> usize {
        let file = syn::parse_file(source).expect("parse");
        let suppress = Suppressions::parse(source);
        let mut out = Vec::new();
        let mut v = IdentVisitor {
            rel: "test.rs",
            suppress: &suppress,
            out: &mut out,
            inside_event_enum: false,
            inside_retry_event_variant: false,
            inside_test_mod: false,
        };
        v.visit_file(&file);
        out.len()
    }

    #[test]
    fn flags_attempt_field() {
        let src = "struct Req { attempt: u8 }\n";
        assert_eq!(count_violations(src), 1);
    }

    #[test]
    fn flags_max_retries_const() {
        let src = "const MAX_RETRIES: u8 = 3;\n";
        assert_eq!(count_violations(src), 1);
    }

    #[test]
    fn flags_fallback_fn() {
        let src = "fn read_or_fallback() {}\n";
        assert_eq!(count_violations(src), 1);
    }

    #[test]
    fn flags_let_attempts() {
        let src = "fn f() { let attempts = 0; }\n";
        assert_eq!(count_violations(src), 1);
    }

    #[test]
    fn allows_retry_fields_in_retry_telemetry_event() {
        let src = "enum DownloadEvent {\n\
            Retrying { attempt: u32, max_retries: u32 },\n\
            RetryExhausted { max_retries: u32 },\n\
        }\n";
        assert_eq!(count_violations(src), 0);
    }

    #[test]
    fn flags_retry_fields_in_other_event_variants() {
        let src = "enum DownloadEvent { Configured { max_retries: u32 } }\n";
        assert_eq!(count_violations(src), 1);
    }

    #[test]
    fn flags_retry_fields_in_state_enums() {
        let src = "enum RetryState { Retrying { attempt: u32 } }\n";
        assert_eq!(count_violations(src), 1);
    }

    #[test]
    fn allows_unrelated_names() {
        let src = "struct Foo { primary: u8, secondary: u8 }\nfn read() {}\n";
        assert_eq!(count_violations(src), 0);
    }

    #[test]
    fn suppression_silences_violation() {
        let src = "// xtask-lint-ignore: retry_fallback\nstruct Req { attempt: u8 }\n";
        assert_eq!(count_violations(src), 0);
    }
}
