use std::fs;

use anyhow::{Result, anyhow};
use proc_macro2::{TokenStream, TokenTree};
use syn::{ItemUse, UseTree, visit::Visit};

use super::{Check, Context};
use crate::common::{
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "platform_layer_hygiene";

pub(crate) struct PlatformLayerHygiene;

impl Check for PlatformLayerHygiene {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let content = fs::read_to_string(&path)?;

            if !owns_raw_arc(&rel_str) {
                for line_num in scan_raw_arc(&content)? {
                    violations.push(
                        Violation::deny(
                            ID,
                            format!("{rel_str}:{line_num}"),
                            "raw `std::sync::Arc` outside the platform backend; import \
                             `kithara_platform::sync::Arc`",
                        )
                        .with_explanation(ARC_EXPLANATION),
                    );
                }
            }

            if !in_scope(&rel_str) {
                continue;
            }
            for (line_num, primitive) in scan_source(&content) {
                violations.push(
                    Violation::deny(
                        ID,
                        format!("{rel_str}:{line_num}"),
                        format!(
                            "raw `{primitive}` in the platform's backend-agnostic layer; \
                             route through the platform's own sync/time abstraction"
                        ),
                    )
                    .with_explanation(EXPLANATION),
                );
            }
        }
        Ok(violations)
    }
}

fn owns_raw_arc(rel_str: &str) -> bool {
    matches!(
        rel_str,
        "crates/kithara-platform/src/system/ownership.rs"
            | "crates/kithara-platform/src/wasm/sync/mod.rs"
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FlatToken {
    Std(usize),
    Sync,
    Arc,
    Colon,
    Other,
}

fn scan_raw_arc(content: &str) -> Result<Vec<usize>> {
    let stream = content
        .parse::<TokenStream>()
        .map_err(|err| anyhow!("failed to tokenize Rust source while checking raw Arc: {err}"))?;
    let mut tokens = Vec::new();
    flatten_tokens(stream, &mut tokens);

    let mut lines = Vec::new();
    for window in tokens.windows(7) {
        let [
            FlatToken::Std(line),
            FlatToken::Colon,
            FlatToken::Colon,
            FlatToken::Sync,
            FlatToken::Colon,
            FlatToken::Colon,
            FlatToken::Arc,
        ] = window
        else {
            continue;
        };
        if lines.last() != Some(line) {
            lines.push(*line);
        }
    }

    let file = syn::parse_file(content)
        .map_err(|err| anyhow!("failed to parse Rust source while checking raw Arc: {err}"))?;
    let mut visitor = RawArcUseVisitor::default();
    visitor.visit_file(&file);
    lines.extend(visitor.lines);
    lines.sort_unstable();
    lines.dedup();
    Ok(lines)
}

#[derive(Default)]
struct RawArcUseVisitor {
    lines: Vec<usize>,
}

impl<'ast> Visit<'ast> for RawArcUseVisitor {
    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        let mut prefix = Vec::new();
        scan_use_tree(&item.tree, &mut prefix, &mut self.lines);
    }
}

fn scan_use_tree(tree: &UseTree, prefix: &mut Vec<String>, lines: &mut Vec<usize>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            scan_use_tree(&path.tree, prefix, lines);
            prefix.pop();
        }
        UseTree::Name(name) if is_std_sync(prefix) && name.ident == "Arc" => {
            lines.push(name.ident.span().start().line);
        }
        UseTree::Rename(rename) if is_std_sync(prefix) && rename.ident == "Arc" => {
            lines.push(rename.ident.span().start().line);
        }
        UseTree::Group(group) => {
            for item in &group.items {
                scan_use_tree(item, prefix, lines);
            }
        }
        UseTree::Name(_) | UseTree::Rename(_) | UseTree::Glob(_) => {}
    }
}

fn is_std_sync(prefix: &[String]) -> bool {
    prefix.len() == 2 && prefix[0] == "std" && prefix[1] == "sync"
}

fn flatten_tokens(stream: TokenStream, out: &mut Vec<FlatToken>) {
    for token in stream {
        match token {
            TokenTree::Ident(ident) => {
                let line = ident.span().start().line;
                out.push(match ident.to_string().as_str() {
                    "std" => FlatToken::Std(line),
                    "sync" => FlatToken::Sync,
                    "Arc" => FlatToken::Arc,
                    _ => FlatToken::Other,
                });
            }
            TokenTree::Punct(punct) if punct.as_char() == ':' => out.push(FlatToken::Colon),
            TokenTree::Group(group) => flatten_tokens(group.stream(), out),
            TokenTree::Punct(_) | TokenTree::Literal(_) => out.push(FlatToken::Other),
        }
    }
}

fn in_scope(rel_str: &str) -> bool {
    const PLATFORM_ROOT: &str = "crates/kithara-platform/src/";
    let Some(inner) = rel_str.strip_prefix(PLATFORM_ROOT) else {
        return false;
    };
    if inner.starts_with("backend/")
        || inner.starts_with("system/")
        || inner.starts_with("loom/")
        || inner.starts_with("wasm/")
    {
        return false;
    }
    !is_impl_subtree(inner)
}

fn is_impl_subtree(inner: &str) -> bool {
    inner.starts_with("flash/sync/")
        || inner.starts_with("flash/tokio/")
        || inner.starts_with("flash/system/")
        || inner.starts_with("common/cancel/")
        || inner == "common/time.rs"
}

fn scan_source(content: &str) -> Vec<(usize, &'static str)> {
    if !content.contains("parking_lot")
        && !content.contains("web_time")
        && !content.contains("Mutex")
        && !content.contains("RwLock")
        && !content.contains("Condvar")
    {
        return Vec::new();
    }

    let test_ranges = cfg_test_ranges(content);
    let mut hits = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_num = idx + 1;
        if test_ranges
            .iter()
            .any(|&(s, e)| line_num >= s && line_num <= e)
        {
            continue;
        }
        let code = strip_line_comment(line);
        if let Some(primitive) = match_primitive(code) {
            hits.push((line_num, primitive));
        }
    }
    hits
}

fn match_primitive(code: &str) -> Option<&'static str> {
    if code.contains("parking_lot::") || contains_use_root(code, "parking_lot") {
        return Some("parking_lot");
    }
    if code.contains("web_time::") || contains_use_root(code, "web_time") {
        return Some("web_time");
    }
    for ty in ["Mutex", "RwLock", "Condvar"] {
        if code.contains(&format!("std::sync::{ty}")) {
            return Some(std_lock_label(ty));
        }
    }
    None
}

fn std_lock_label(ty: &str) -> &'static str {
    match ty {
        "Mutex" => "std::sync::Mutex",
        "RwLock" => "std::sync::RwLock",
        "Condvar" => "std::sync::Condvar",
        _ => "std::sync",
    }
}

fn contains_use_root(code: &str, root: &str) -> bool {
    let trimmed = code.trim_start();
    if !trimmed.starts_with("use ") && !trimmed.starts_with("pub use ") {
        return false;
    }
    code.contains(&format!("{root}::"))
}

fn cfg_test_ranges(content: &str) -> Vec<(usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].contains("#[cfg(test)]") {
            let mut j = i + 1;
            while j < lines.len() && !lines[j].contains('{') {
                j += 1;
            }
            if j < lines.len() {
                let start = i + 1;
                let mut depth: usize = 0;
                let mut k = j;
                loop {
                    let (opens, closes) = brace_counts(lines[k]);
                    depth = depth.saturating_add(opens);
                    // The mod's opening brace puts depth at >=1; once closes
                    // bring it back to 0 the block is done.
                    if closes >= depth {
                        break;
                    }
                    depth -= closes;
                    k += 1;
                    if k >= lines.len() {
                        break;
                    }
                }
                ranges.push((start, k + 1));
                i = k + 1;
                continue;
            }
        }
        i += 1;
    }
    ranges
}

/// `(open-brace count, close-brace count)` on a line, comment stripped.
fn brace_counts(line: &str) -> (usize, usize) {
    let code = strip_line_comment(line);
    let opens = code.bytes().filter(|&b| b == b'{').count();
    let closes = code.bytes().filter(|&b| b == b'}').count();
    (opens, closes)
}

/// Strip a trailing `//` line comment so matching only inspects code. Keeps
/// the part before the first `//` not inside a string literal (best-effort:
/// counts unescaped double-quotes).
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut prev_backslash = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !in_string && b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            return &line[..i];
        }
        if b == b'"' && !prev_backslash {
            in_string = !in_string;
        }
        prev_backslash = b == b'\\' && !prev_backslash;
        i += 1;
    }
    line
}

const EXPLANATION: &str = "\
Summary: a raw `parking_lot` / `web_time` / `std::sync::{Mutex,RwLock,Condvar}`
import sits in the platform's backend-agnostic layer (`flash`/`common`, outside
the primitive-implementation sub-trees). These modules must route sync and time
through the platform's own abstractions so the flash virtual clock and the
system/loom/wasm backends can be swapped wholesale.

Why: the cross-backend flash control surface and the shared `common` modules
are the platform's public face. A raw lock or wall clock here is a second,
un-virtualizable primitive that the engine cannot observe — exactly the kind of
leak that lets a flash test diverge from its real-time twin.

This lock/time check does not classify atomics, refcount handles, once-init
primitives, or `thread::panicking()`. Their routing is enforced separately;
in particular, every consumer imports `Arc` from `kithara_platform::sync`.

Legitimate wrapping sites (NOT in scope): `system/`, `loom/`, `wasm/`, `flash/sync/`,
`flash/tokio/`, `flash/system/`, `common/cancel/`, `common/time.rs` — these
BUILD the primitives by wrapping raw `parking_lot`/`std`/`web_time` and are the
platform's equivalent of a backend impl.

Fix: import the platform's own `sync::{Mutex,RwLock,Condvar}` / `time::Instant`,
or move the primitive-owning code into one of the implementation sub-trees if it
genuinely belongs there.

See `crates/kithara-platform/README.md` and `AGENTS.md`.";

const ARC_EXPLANATION: &str = "\
Summary: a consumer imports or names `std::sync::Arc` directly instead of using
the canonical `kithara_platform::sync::Arc` surface.

Why: all synchronization ownership must enter through kithara-platform so the
workspace has one stable import path and backend selection remains centralized.
The system ownership and wasm sync modules are the only sites that own the raw std type.

Fix: import `kithara_platform::sync::Arc`. Do not add a parallel Arc alias or a
second ownership type.";

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::{in_scope, is_impl_subtree, owns_raw_arc, scan_raw_arc, scan_source};

    #[test]
    fn flags_raw_parking_lot_and_web_time_and_std_lock() {
        let src = "use parking_lot::Mutex;\nuse web_time::Instant;\nuse std::sync::Mutex;\n";
        let hits = scan_source(src);
        assert_eq!(
            hits,
            vec![(1, "parking_lot"), (2, "web_time"), (3, "std::sync::Mutex"),]
        );
    }

    #[test]
    fn lock_scan_exempts_non_blocking_primitives() {
        let src = "use std::sync::atomic::AtomicU64;\n\
            use std::sync::{Arc, Weak};\n\
            use std::sync::{Once, OnceLock, LazyLock};\n\
            let p = std::thread::panicking();\n";
        assert!(scan_source(src).is_empty(), "got: {:?}", scan_source(src));
    }

    #[test]
    fn lock_scan_ignores_grouped_std_sync_without_lock_types() {
        let src = "use std::sync::{Arc, OnceLock, Weak};\n";
        assert!(scan_source(src).is_empty(), "got: {:?}", scan_source(src));
    }

    #[test]
    fn flags_raw_arc_paths_and_ignores_source_strings() -> Result<()> {
        let src = r#"
            use std::sync::Arc;
            type Shared = ::std::sync::Arc<u8>;
            use std::{collections::HashMap, sync::{Weak, Arc}};
            const FIXTURE: &str = "use std::sync::Arc;";
        "#;
        assert_eq!(scan_raw_arc(src)?, vec![2, 3, 4]);
        Ok(())
    }

    #[test]
    fn raw_arc_owner_is_exact() {
        assert!(owns_raw_arc(
            "crates/kithara-platform/src/system/ownership.rs"
        ));
        assert!(owns_raw_arc("crates/kithara-platform/src/wasm/sync/mod.rs"));
        assert!(!owns_raw_arc(
            "crates/kithara-platform/src/system/sync/mutex.rs"
        ));
        assert!(!owns_raw_arc("crates/kithara-audio/src/audio.rs"));
    }

    #[test]
    fn flags_grouped_parking_lot_import() {
        let src = "use parking_lot::{Mutex, Condvar};\n";
        assert_eq!(scan_source(src), vec![(1, "parking_lot")]);
    }

    #[test]
    fn skips_comments_and_cfg_test_modules() {
        let src = "// use parking_lot::Mutex; in a comment\n\
            fn prod() {}\n\
            #[cfg(test)]\n\
            mod tests {\n\
            use parking_lot::Mutex;\n\
            }\n";
        assert!(scan_source(src).is_empty(), "got: {:?}", scan_source(src));
    }

    #[test]
    fn scope_excludes_backend_and_impl_subtrees() {
        // Backend impls and primitive sub-trees are NOT in scope.
        assert!(!in_scope(
            "crates/kithara-platform/src/system/sync/mutex.rs"
        ));
        assert!(!in_scope(
            "crates/kithara-platform/src/backend/flash_system/mutex.rs"
        ));
        assert!(!in_scope("crates/kithara-platform/src/loom/sync/mutex.rs"));
        assert!(!in_scope("crates/kithara-platform/src/wasm/sync/mutex.rs"));
        assert!(!in_scope(
            "crates/kithara-platform/src/flash/tokio/semaphore.rs"
        ));
        assert!(!in_scope(
            "crates/kithara-platform/src/flash/sync/notify.rs"
        ));
        assert!(!in_scope(
            "crates/kithara-platform/src/flash/system/inner.rs"
        ));
        assert!(!in_scope(
            "crates/kithara-platform/src/common/cancel/node.rs"
        ));
        assert!(!in_scope("crates/kithara-platform/src/common/time.rs"));
        // Other crates are out of scope (this guard is platform-only).
        assert!(!in_scope("crates/kithara-audio/src/audio.rs"));
    }

    #[test]
    fn scope_includes_cross_backend_flash_and_common() {
        // Consumer-facing flash control surface + shared common modules ARE
        assert!(in_scope("crates/kithara-platform/src/flash/api.rs"));
        assert!(in_scope("crates/kithara-platform/src/flash/ctx.rs"));
        assert!(in_scope("crates/kithara-platform/src/flash/time.rs"));
        assert!(in_scope(
            "crates/kithara-platform/src/common/flash_inert.rs"
        ));
        assert!(in_scope("crates/kithara-platform/src/common/maybe_send.rs"));
    }

    #[test]
    fn impl_subtree_classification() {
        assert!(is_impl_subtree("flash/sync/notify.rs"));
        assert!(is_impl_subtree("flash/tokio/mpsc.rs"));
        assert!(is_impl_subtree("flash/system/pace.rs"));
        assert!(is_impl_subtree("common/cancel/token.rs"));
        assert!(is_impl_subtree("common/time.rs"));
        assert!(!is_impl_subtree("flash/api.rs"));
        assert!(!is_impl_subtree("common/maybe_send.rs"));
    }
}
