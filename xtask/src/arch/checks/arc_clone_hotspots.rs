//! Functions with many `Arc::clone(...)` / `Rc::clone(...)` calls.
//!
//! High clone-density inside one function signals an orchestrator that
//! distributes ownership of shared state — usually a god-object that owns
//! everything and hands `Arc` clones out to children. `arch::shared_state`
//! counts *declarations* of `Arc<Mutex/RwLock>`; this check counts
//! *propagation points* and is a complementary angle.
//!
//! Detection counts only the explicit `Arc::clone(&x)` / `Rc::clone(&x)`
//! form (the `clippy::clone_on_ref_ptr`-blessed shape). Method-call
//! `x.clone()` is intentionally not counted — the AST cannot tell whether
//! the receiver is `Arc<T>` or `MyValueType`.
//!
//! Each function is reported once with its full path. Closures inside a
//! function attribute their clones to the enclosing function, since the
//! whole expression body shares the same orchestration responsibility.

use anyhow::Result;
use syn::{
    Expr, ImplItem, Item, ItemImpl, Type,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "arc_clone_hotspots";

pub(crate) struct ArcCloneHotspots;

impl Check for ArcCloneHotspots {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.arc_clone_hotspots;
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            let mut hits: Vec<(String, usize)> = Vec::new();
            walk_items(&file.items, &mut Vec::new(), &mut hits);

            for (label, count) in hits {
                if count < cfg.warn {
                    continue;
                }
                let key = format!("{rel}::{label}");
                let msg = format!(
                    "{label}: {count} Arc/Rc::clone(...) calls (warn threshold {})",
                    cfg.warn
                );
                violations.push(Violation::warn(ID, key, msg));
            }
        }
        Ok(violations)
    }
}

fn walk_items(items: &[Item], scope: &mut Vec<String>, out: &mut Vec<(String, usize)>) {
    for item in items {
        match item {
            Item::Fn(f) => {
                let label = qualified(scope, &f.sig.ident.to_string());
                let mut v = CloneCounter::default();
                v.visit_block(&f.block);
                if v.count > 0 {
                    out.push((label, v.count));
                }
            }
            Item::Impl(im) => walk_impl(im, scope, out),
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    scope.push(m.ident.to_string());
                    walk_items(inner, scope, out);
                    scope.pop();
                }
            }
            _ => {}
        }
    }
}

fn walk_impl(im: &ItemImpl, scope: &[String], out: &mut Vec<(String, usize)>) {
    let owner = self_ty_label(&im.self_ty);
    for it in &im.items {
        if let ImplItem::Fn(f) = it {
            let label = qualified(scope, &format!("{owner}::{}", f.sig.ident));
            let mut v = CloneCounter::default();
            v.visit_block(&f.block);
            if v.count > 0 {
                out.push((label, v.count));
            }
        }
    }
}

fn qualified(scope: &[String], name: &str) -> String {
    if scope.is_empty() {
        name.to_string()
    } else {
        format!("{}::{name}", scope.join("::"))
    }
}

fn self_ty_label(ty: &Type) -> String {
    match ty {
        Type::Path(p) => p
            .path
            .segments
            .last()
            .map_or_else(|| "?".to_string(), |s| s.ident.to_string()),
        _ => "?".to_string(),
    }
}

#[derive(Default)]
struct CloneCounter {
    count: usize,
}

impl<'ast> Visit<'ast> for CloneCounter {
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let Expr::Path(p) = c.func.as_ref() {
            let segs: Vec<String> = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            // Recognises `Arc::clone(&x)`, `Rc::clone(&x)`, and the fully
            // qualified `std::sync::Arc::clone(&x)` / `std::rc::Rc::clone(&x)`.
            let n = segs.len();
            if n >= 2 && segs[n - 1] == "clone" && (segs[n - 2] == "Arc" || segs[n - 2] == "Rc") {
                self.count += 1;
            }
        }
        visit::visit_expr_call(self, c);
    }
}
