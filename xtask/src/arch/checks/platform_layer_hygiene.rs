use std::fs;

use anyhow::Result;
use kithara_xtask_core::common::{
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

use super::{Check, Context};

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
            if !in_scope(&rel_str) {
                continue;
            }
            let content = fs::read_to_string(&path)?;
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

/// True for the cross-backend / consumer-facing platform modules that must
/// stay clock- and lock-agnostic. The backend-implementation sub-trees that
/// legitimately *build* the primitives by wrapping raw `parking_lot`/`std`/
/// `web_time` are excluded here — they are the platform's equivalent of the
/// `native`/`wasm` backends and cannot route through an abstraction they
/// themselves provide.
fn in_scope(rel_str: &str) -> bool {
    const PLATFORM_ROOT: &str = "crates/kithara-platform/src/";
    let Some(inner) = rel_str.strip_prefix(PLATFORM_ROOT) else {
        return false;
    };
    // The `native` and `wasm` backends are the canonical wrapping sites for
    // raw OS / browser primitives — that is their job, never a leak.
    if inner.starts_with("native/") || inner.starts_with("wasm/") {
        return false;
    }
    // Inside `flash` and `common`, the primitive-implementation sub-trees are
    // the flash virtual-clock backend (mirrors `native/sync` + `native/tokio`)
    // and the std-only cancel tree / the platform's own time chokepoint. They
    // own raw primitives by contract; everything else in flash/common must not.
    !is_impl_subtree(inner)
}

/// Backend-implementation sub-trees within `flash`/`common` that own raw
/// primitives by contract:
/// - `flash/sync`, `flash/tokio`: the flash virtual-clock re-implementations
///   of the platform sync / tokio primitives (their internal lock is a raw
///   `parking_lot::Mutex`, exactly like `native/sync/mutex.rs`).
/// - `flash/system`: the quiescence engine; its real-I/O pacer anchors on the
///   REAL `web_time` clock on purpose (virtual time must never outrun it).
/// - `common/cancel`: the std-only propagate-down cancel tree, deliberately
///   built on bare `std::sync::Mutex` so it compiles unchanged on wasm32 and
///   stays RT-safe (documented in `common/cancel/mod.rs`).
/// - `common/time.rs`: the platform's own time backend (`web_time` re-export)
///   that `kithara_platform::time` binds to a real or virtual clock.
fn is_impl_subtree(inner: &str) -> bool {
    inner.starts_with("flash/sync/")
        || inner.starts_with("flash/tokio/")
        || inner.starts_with("flash/system/")
        || inner.starts_with("common/cancel/")
        || inner == "common/time.rs"
}

/// Pure core: the (1-based line, matched primitive) of every raw sync/time
/// import or qualified use in `content` that sits in production code (outside
/// line comments and `#[cfg(test)]` modules). Exempt primitives — atomics,
/// `Arc`/`Rc`/`Weak`, `LazyLock`/`OnceLock`/`Once`, `thread::panicking` — are
/// never matched (they carry no virtual-clock or blocking-wait semantics, so
/// they need no facade routing).
fn scan_source(content: &str) -> Vec<(usize, &'static str)> {
    if !content.contains("parking_lot") && !content.contains("web_time") {
        // The only `std::sync` primitives we flag are Mutex/RwLock/Condvar;
        // a file with none of these tokens cannot violate.
        if !content.contains("Mutex") && !content.contains("RwLock") && !content.contains("Condvar")
        {
            return Vec::new();
        }
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

/// Match a single forbidden raw primitive on a line of code (comment already
/// stripped). `parking_lot::*` and `web_time::*` are forbidden wholesale (they
/// are not exempt anywhere in-scope); for `std::sync` only the blocking-wait
/// lock types are forbidden — the once-init / refcount / atomic primitives are
/// the exempt class.
fn match_primitive(code: &str) -> Option<&'static str> {
    if code.contains("parking_lot::") || contains_use_root(code, "parking_lot") {
        return Some("parking_lot");
    }
    if code.contains("web_time::") || contains_use_root(code, "web_time") {
        return Some("web_time");
    }
    // Raw std-sync blocking-wait locks. `Arc`/`Weak`/`LazyLock`/`OnceLock`/
    // `Once`/atomics are NOT matched (exempt class).
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

/// True when `code` is a `use` declaration whose path roots at `root` (flat or
/// grouped import). Catches `use parking_lot::Mutex;` and
/// `use parking_lot::{Mutex, Condvar};`.
fn contains_use_root(code: &str, root: &str) -> bool {
    let trimmed = code.trim_start();
    if !trimmed.starts_with("use ") && !trimmed.starts_with("pub use ") {
        return false;
    }
    code.contains(&format!("{root}::"))
}

/// Conservative `#[cfg(test)]` mod range detection by source lines: spans from
/// a `#[cfg(test)]` attribute's following `mod` to the brace that closes it.
/// Mirrors the intent of `cancel_root_sites` without re-parsing the AST (this
/// check needs only line-level skipping of test modules).
fn cfg_test_ranges(content: &str) -> Vec<(usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].contains("#[cfg(test)]") {
            // Find the `mod` opening brace at/after this attribute.
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
native/wasm backends can be swapped wholesale.

Why: the cross-backend flash control surface and the shared `common` modules
are the platform's public face. A raw lock or wall clock here is a second,
un-virtualizable primitive that the engine cannot observe — exactly the kind of
leak that lets a flash test diverge from its real-time twin.

Exempt (allowed raw): atomics (`std::sync::atomic::*`), refcount handles
(`Arc`/`Rc`/`Weak`), once-init (`LazyLock`/`OnceLock`/`Once`), and
`thread::panicking()` — none carry virtual-clock or blocking-wait semantics, so
they need no facade routing.

Legitimate wrapping sites (NOT in scope): `native/`, `wasm/`, `flash/sync/`,
`flash/tokio/`, `flash/system/`, `common/cancel/`, `common/time.rs` — these
BUILD the primitives by wrapping raw `parking_lot`/`std`/`web_time` and are the
platform's equivalent of a backend impl.

Fix: import the platform's own `sync::{Mutex,RwLock,Condvar}` / `time::Instant`,
or move the primitive-owning code into one of the implementation sub-trees if it
genuinely belongs there.

See `crates/kithara-platform/README.md` and `AGENTS.md`.";

#[cfg(test)]
mod tests {
    use super::{in_scope, is_impl_subtree, scan_source};

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
    fn exempts_atomics_arc_once_and_panicking() {
        let src = "use std::sync::atomic::AtomicU64;\n\
            use std::sync::{Arc, Weak};\n\
            use std::sync::{Once, OnceLock, LazyLock};\n\
            let p = std::thread::panicking();\n";
        assert!(scan_source(src).is_empty(), "got: {:?}", scan_source(src));
    }

    #[test]
    fn ignores_grouped_std_sync_without_lock_types() {
        // A grouped std::sync import of exempt-only primitives must not fire.
        let src = "use std::sync::{Arc, OnceLock, Weak};\n";
        assert!(scan_source(src).is_empty(), "got: {:?}", scan_source(src));
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
            "crates/kithara-platform/src/native/sync/mutex.rs"
        ));
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
        // in scope.
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
