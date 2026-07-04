use anyhow::Result;
use kithara_xtask_core::common::{
    parse::parse_file,
    suppress::Suppressions,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};
use syn::{
    Expr, ExprAwait, ExprBlock, ExprCall, ExprMethodCall, ExprPath, Local, Pat, PatIdent, Path,
    Stmt,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};

pub(crate) const ID: &str = "await_under_guard";

const EXPLANATION: &str = "\
Detected an `.await` point inside a scope that still holds a `MutexGuard` \
/ `RwLockReadGuard` / `RwLockWriteGuard`.

Why it matters. Holding a sync mutex across `.await` is one of the most \
pernicious bugs in async Rust:

1. Deadlock under contention. The task suspends with the lock held. The \
   runtime moves the task to a different worker. Another task on the same \
   worker tries to acquire the same lock — workers can't make progress, \
   the original task can't be resumed because all workers are blocked. \
   Under sufficient load this stalls the whole runtime.
2. Send-bound issues. `MutexGuard` is `!Send` for `std::sync::Mutex` and \
   most `parking_lot` variants. Holding it across `.await` makes the \
   future `!Send`, breaking `tokio::spawn` (multi-thread runtime).
3. Latency tail. Even when no deadlock occurs, the lock is held for the \
   duration of the `.await` operation (potentially milliseconds for I/O), \
   turning a microsecond critical section into a millisecond stall for \
   every contender.

The fix is one of:
- Drop before await: `let result = { let g = mutex.lock_sync(); compute(&*g) }; do_async(result).await;`
- Use async-aware lock: `tokio::sync::Mutex` or `kithara_platform`'s \
  `lock_async` is designed to span `.await`.
- Move data out: clone the value under the guard, drop the guard, then await.

❌  let g = self.state.lock_sync(); let result = fetch(&g.url).await; g.record(result);
✅  let url = { let g = self.state.lock_sync(); g.url.clone() }; let result = fetch(&url).await;

Suppress with `// xtask-lint-ignore: await_under_guard` only when (1) the \
lock is `tokio::sync::Mutex` / kithara_platform `AsyncMutex` (designed to \
span `.await`), (2) the awaited future provably doesn't re-enter the same \
lock and isn't on a single-threaded runtime, (3) explicit `drop(guard)` \
precedes `.await` but the heuristic missed it. Document why it's safe — \
deadlocks are silent.";

pub(crate) struct AwaitUnderGuard;

impl Check for AwaitUnderGuard {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.await_under_guard;
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
            let mut v = AwaitVisitor {
                rel: &rel,
                suppress: &suppress,
                guards: Vec::new(),
                out: &mut violations,
            };
            v.visit_file(&file);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

#[derive(Debug, Clone)]
struct ActiveGuard {
    binding: String,
    /// Source line of the `let g = ...` statement.
    decl_line: usize,
    /// Lock-acquiring method name (e.g. `lock_sync`, `read`).
    method: String,
}

struct AwaitVisitor<'a> {
    rel: &'a str,
    suppress: &'a Suppressions,
    guards: Vec<ActiveGuard>,
    out: &'a mut Vec<Violation>,
}

impl<'ast> Visit<'ast> for AwaitVisitor<'_> {
    fn visit_block(&mut self, b: &'ast syn::Block) {
        let snapshot = self.guards.len();
        self.process_block(b);
        self.guards.truncate(snapshot);
    }

    fn visit_expr_block(&mut self, b: &'ast ExprBlock) {
        self.visit_block(&b.block);
    }

    fn visit_expr_await(&mut self, a: &'ast ExprAwait) {
        visit::visit_expr_await(self, a);
        if self.guards.is_empty() {
            return;
        }
        let s = a.await_token.span().start();
        if self.suppress.is_suppressed(s.line, ID) {
            return;
        }
        for g in self.guards.clone() {
            let key = format!("{}:{}:{}::{}", self.rel, s.line, s.column, g.binding);
            let msg = format!(
                "G1: `.await` reached at line {await_line} while guard `{binding}` from \
                 `.{method}()` (line {decl_line}) is still alive — drop the guard before \
                 awaiting or use an async-aware lock",
                await_line = s.line,
                binding = g.binding,
                method = g.method,
                decl_line = g.decl_line,
            );
            self.out
                .push(Violation::warn(ID, key, msg).with_explanation(EXPLANATION));
        }
    }
}

impl AwaitVisitor<'_> {
    fn process_block(&mut self, b: &syn::Block) {
        let snapshot = self.guards.len();
        for stmt in &b.stmts {
            self.process_stmt(stmt);
        }
        self.guards.truncate(snapshot);
    }

    fn process_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Local(local) => {
                if let Some(init) = &local.init {
                    visit::visit_expr(self, &init.expr);
                    if let Some((label, _)) = init.diverge.as_ref() {
                        let _ = label;
                    }
                }
                if let Some(g) = guard_from_local(local) {
                    self.guards.push(g);
                }
            }
            Stmt::Expr(e, _) => {
                if let Some(target) = drop_call_target(e) {
                    self.guards.retain(|g| g.binding != target);
                }
                visit::visit_expr(self, e);
            }
            other => visit::visit_stmt(self, other),
        }
    }
}

/// `let <ident> = <expr>.<method>()` where method acquires a sync lock —
/// returns the registered guard binding.
fn guard_from_local(local: &Local) -> Option<ActiveGuard> {
    let ident = pat_ident_name(&local.pat)?;
    let init = local.init.as_ref()?;
    let mc = method_call_at_tail(&init.expr)?;
    let method = mc.method.to_string();
    if !is_sync_lock_method(&method) {
        return None;
    }
    let span = local.let_token.span().start();
    Some(ActiveGuard {
        binding: ident,
        decl_line: span.line,
        method,
    })
}

/// `drop(<ident>)` or `mem::drop(<ident>)` — returns the dropped binding.
fn drop_call_target(e: &Expr) -> Option<String> {
    let Expr::Call(ExprCall { func, args, .. }) = e else {
        return None;
    };
    let Expr::Path(ExprPath { path, .. }) = &**func else {
        return None;
    };
    if !path_ends_with(path, "drop") {
        return None;
    }
    if args.len() != 1 {
        return None;
    }
    if let Expr::Path(ExprPath { path: p, .. }) = &args[0]
        && p.segments.len() == 1
    {
        return Some(p.segments[0].ident.to_string());
    }
    None
}

fn pat_ident_name(p: &Pat) -> Option<String> {
    match p {
        Pat::Ident(PatIdent {
            ident,
            subpat: None,
            ..
        }) => Some(ident.to_string()),
        _ => None,
    }
}

/// Drill through `?` and final `.method()` to reach the tail method call.
fn method_call_at_tail(e: &Expr) -> Option<&ExprMethodCall> {
    match e {
        Expr::MethodCall(mc) => Some(mc),
        Expr::Try(t) => method_call_at_tail(&t.expr),
        _ => None,
    }
}

fn is_sync_lock_method(name: &str) -> bool {
    matches!(
        name,
        "lock" | "lock_sync" | "read" | "write" | "upgradable_read"
    )
}

fn path_ends_with(path: &Path, name: &str) -> bool {
    path.segments
        .last()
        .is_some_and(|seg| seg.ident == name && seg.arguments.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_in(src: &str) -> usize {
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let suppress = Suppressions::parse(src);
        let mut out = Vec::new();
        let mut v = AwaitVisitor {
            rel: "fixture.rs",
            suppress: &suppress,
            guards: Vec::new(),
            out: &mut out,
        };
        v.visit_file(&file);
        out.len()
    }

    #[test]
    fn await_under_lock_sync_flagged() {
        let src = "
async fn f(m: std::sync::Mutex<u32>) {
    let g = m.lock_sync();
    other().await;
    drop(g);
}
async fn other() {}
";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn await_under_lock_flagged() {
        let src = "
async fn f(m: std::sync::Mutex<u32>) {
    let g = m.lock();
    other().await;
}
async fn other() {}
";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn await_under_read_flagged() {
        let src = "
async fn f(m: std::sync::RwLock<u32>) {
    let g = m.read();
    other().await;
}
async fn other() {}
";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn explicit_drop_clears_guard() {
        let src = "
async fn f(m: std::sync::Mutex<u32>) {
    let g = m.lock_sync();
    drop(g);
    other().await;
}
async fn other() {}
";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn await_outside_lock_scope_not_flagged() {
        let src = "
async fn f(m: std::sync::Mutex<u32>) {
    let value = { let g = m.lock_sync(); *g };
    drop(value);
    other().await;
}
async fn other() {}
";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn no_lock_no_violation() {
        let src = "
async fn f() {
    other().await;
}
async fn other() {}
";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn lock_async_method_not_flagged() {
        let src = "
async fn f(m: tokio::sync::Mutex<u32>) {
    let g = m.lock_async();
    other().await;
}
async fn other() {}
";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn multiple_guards_each_reported() {
        let src = "
async fn f(a: std::sync::Mutex<u32>, b: std::sync::Mutex<u32>) {
    let ga = a.lock_sync();
    let gb = b.lock_sync();
    other().await;
}
async fn other() {}
";
        assert_eq!(count_in(src), 2);
    }

    #[test]
    fn suppression_works() {
        let src = "
async fn f(m: std::sync::Mutex<u32>) {
    let g = m.lock_sync();
    // xtask-lint-ignore: await_under_guard
    other().await;
}
async fn other() {}
";
        assert_eq!(count_in(src), 0);
    }
}
