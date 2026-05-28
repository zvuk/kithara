use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;

/// A workspace function node, keyed by its source location `(rel_file, line)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NodeKey {
    pub(crate) file: String,
    pub(crate) line: usize,
}

impl NodeKey {
    pub(crate) fn label(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

/// Workspace call graph: source-location nodes and direct-call edges between
/// them. Monomorphized generic instances that resolve to the same source line
/// are unioned into one node.
#[derive(Debug, Default)]
pub(crate) struct CallGraph {
    /// `node -> base function name`.
    pub(crate) nodes: BTreeMap<NodeKey, String>,
    /// `node -> set of callee nodes`. Self-loops are kept.
    pub(crate) edges: BTreeMap<NodeKey, BTreeSet<NodeKey>>,
}

impl CallGraph {
    pub(crate) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub(crate) fn edge_count(&self) -> usize {
        self.edges.values().map(BTreeSet::len).sum()
    }
}

struct Patterns {
    difile: Regex,
    disubprogram: Regex,
    define: Regex,
    call_sym: Regex,
}

impl Patterns {
    fn new() -> Self {
        Self {
            // !12 = !DIFile(filename: "crates/x/src/y.rs", directory: "...")
            difile: Regex::new(r#"^!(\d+)\s*=\s*!DIFile\(filename:\s*"([^"]*)""#)
                .expect("static DIFile regex"),
            // !34 = distinct !DISubprogram(name: "foo", ... file: !12, ... line: 7, ...)
            disubprogram: Regex::new(r"^!(\d+)\s*=\s*(?:distinct\s+)?!DISubprogram\((.*)\)\s*$")
                .expect("static DISubprogram regex"),
            // define ... @"<SYM>"(...) ... !dbg !34 {
            define: Regex::new(r#"^define\s+.*?@(?:"([^"]*)"|([A-Za-z0-9_.$]+))\s*\("#)
                .expect("static define regex"),
            // direct call/invoke target: @"<SYM>" or @<SYM>
            call_sym: Regex::new(r#"@(?:"([^"]*)"|([A-Za-z0-9_.$]+))"#)
                .expect("static call symbol regex"),
        }
    }
}

/// Metadata gathered in the first pass over one `.ll` file.
struct Meta {
    /// `!NN -> filename` for every `!DIFile`.
    files: BTreeMap<u64, String>,
    /// `!NN -> (file_meta_ref, line, base_name)` for every named `!DISubprogram`.
    subprograms: BTreeMap<u64, Subprogram>,
}

#[derive(Debug, Clone)]
struct Subprogram {
    file_ref: u64,
    line: usize,
    name: String,
}

/// Parse one textual `.ll` file into the set of workspace defines it declares.
/// A define is a workspace define when its `DISubprogram`'s `DIFile` filename
/// is under `crates/*/src`.
pub(crate) fn parse_ll(source: &str) -> Vec<ParsedDefine> {
    let pats = Patterns::new();
    let meta = collect_meta(source, &pats);
    collect_defines(source, &pats, &meta)
}

/// A define resolved to workspace source coordinates, kept flat so the caller
/// (and tests) can inspect symbol/name/callees before graph assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedDefine {
    pub(crate) symbol: String,
    pub(crate) file: String,
    pub(crate) line: usize,
    pub(crate) name: String,
    /// Direct-call callee symbols found in the body (de-duplicated, sorted).
    pub(crate) callees: Vec<String>,
}

fn collect_meta(source: &str, pats: &Patterns) -> Meta {
    let mut files = BTreeMap::new();
    let mut subprograms = BTreeMap::new();
    for line in source.lines() {
        let line = line.trim_start();
        if let Some(caps) = pats.difile.captures(line) {
            let Ok(id) = caps[1].parse::<u64>() else {
                continue;
            };
            files.insert(id, normalize_path(&caps[2]));
            continue;
        }
        if let Some(caps) = pats.disubprogram.captures(line) {
            let Ok(id) = caps[1].parse::<u64>() else {
                continue;
            };
            let body = &caps[2];
            let (Some(name), Some(file_ref), Some(sub_line)) = (
                field_string(body, "name"),
                field_meta_ref(body, "file"),
                field_uint(body, "line"),
            ) else {
                continue;
            };
            subprograms.insert(
                id,
                Subprogram {
                    file_ref,
                    line: sub_line,
                    name: base_name(&name),
                },
            );
        }
    }
    Meta { files, subprograms }
}

fn collect_defines(source: &str, pats: &Patterns, meta: &Meta) -> Vec<ParsedDefine> {
    let mut out = Vec::new();
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(caps) = pats.define.captures(line) else {
            continue;
        };
        let symbol = caps
            .get(1)
            .or_else(|| caps.get(2))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let dbg = dbg_ref(line);
        // Gather the body until the closing brace at column 0.
        let mut body = String::new();
        let mut closed = false;
        if line.trim_end().ends_with('{') {
            for body_line in lines.by_ref() {
                if body_line == "}" {
                    closed = true;
                    break;
                }
                body.push_str(body_line);
                body.push('\n');
            }
        }
        let _ = closed;
        let Some(dbg) = dbg else { continue };
        let Some(sub) = meta.subprograms.get(&dbg) else {
            continue;
        };
        let Some(file) = meta.files.get(&sub.file_ref) else {
            continue;
        };
        if !is_workspace_file(file) {
            continue;
        }
        let callees = collect_callees(&body, pats);
        out.push(ParsedDefine {
            symbol,
            file: file.clone(),
            line: sub.line,
            name: sub.name.clone(),
            callees,
        });
    }
    out
}

/// Collect direct-call callee symbols from a function body. Indirect calls
/// (`call ... %reg(...)`) carry no `@symbol` on the call and are skipped.
fn collect_callees(body: &str, pats: &Patterns) -> Vec<String> {
    let mut set = BTreeSet::new();
    for raw in body.lines() {
        let line = raw.trim_start();
        if !is_call_line(line) {
            continue;
        }
        // The callee symbol is the operand at/just before the argument list.
        // Take the LAST `@sym` on the line that is immediately followed by `(`
        // so we ignore `@sym` tokens appearing only inside bitcast arguments.
        if let Some(symbol) = call_target_symbol(line, pats) {
            set.insert(symbol);
        }
    }
    set.into_iter().collect()
}

fn is_call_line(line: &str) -> bool {
    // Match `call`/`invoke` as a leading keyword, possibly after an SSA
    // assignment like `%5 = call ...` or `%5 = tail call ...`.
    let rest = line
        .strip_prefix("call ")
        .or_else(|| line.strip_prefix("invoke "));
    if rest.is_some() {
        return true;
    }
    if let Some(after_eq) = line.split_once('=').map(|(_, r)| r.trim_start()) {
        for kw in ["call ", "tail call ", "musttail call ", "notail call "] {
            if after_eq.starts_with(kw) {
                return true;
            }
        }
    }
    line.starts_with("tail call ")
        || line.starts_with("musttail call ")
        || line.starts_with("notail call ")
}

/// Extract the called symbol from a `call`/`invoke` line. Returns the symbol
/// that directly precedes the `(` argument list; `None` for indirect calls
/// (target is an SSA register) or symbol-free lines.
fn call_target_symbol(line: &str, pats: &Patterns) -> Option<String> {
    // Find every `@sym(` occurrence; the call target is the one followed by `(`.
    let mut best: Option<String> = None;
    for caps in pats.call_sym.captures_iter(line) {
        let m = caps.get(0)?;
        let after = &line[m.end()..];
        // Allow whitespace then `(`.
        if after.trim_start().starts_with('(') {
            let sym = caps
                .get(1)
                .or_else(|| caps.get(2))
                .map(|s| s.as_str().to_string());
            if let Some(sym) = sym {
                best = Some(sym);
            }
        }
    }
    best
}

/// Assemble a [`CallGraph`] from the parsed defines of every `.ll` file.
/// `symbol -> node` is built first (workspace defines only), then edges are
/// resolved and unioned per node.
pub(crate) fn build_graph(defines: &[ParsedDefine]) -> CallGraph {
    let mut sym_to_node: BTreeMap<&str, NodeKey> = BTreeMap::new();
    for d in defines {
        let node = NodeKey {
            file: d.file.clone(),
            line: d.line,
        };
        sym_to_node.insert(d.symbol.as_str(), node);
    }

    let mut graph = CallGraph::default();
    for d in defines {
        let node = NodeKey {
            file: d.file.clone(),
            line: d.line,
        };
        graph
            .nodes
            .entry(node.clone())
            .or_insert_with(|| d.name.clone());
        let out = graph.edges.entry(node.clone()).or_default();
        for callee in &d.callees {
            if let Some(target) = sym_to_node.get(callee.as_str()) {
                out.insert(target.clone());
            }
        }
    }
    // Drop empty edge sets so the persisted form omits edgeless nodes.
    graph.edges.retain(|_, v| !v.is_empty());
    graph
}

fn dbg_ref(line: &str) -> Option<u64> {
    let idx = line.find("!dbg ")?;
    let rest = &line[idx + "!dbg ".len()..];
    let rest = rest.trim_start().strip_prefix('!')?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Position just after `<key>: ` for the first occurrence where `key` sits on
/// a field boundary (start-of-string, `(`, whitespace or `,`). Avoids matching
/// `name` inside `linkageName` or `line` inside `scopeLine`.
fn field_value_start(body: &str, key: &str) -> Option<usize> {
    let needle = format!("{key}: ");
    let mut from = 0;
    while let Some(rel) = body[from..].find(&needle) {
        let at = from + rel;
        let boundary = at == 0
            || body[..at]
                .chars()
                .next_back()
                .is_some_and(|c| matches!(c, '(' | ',' | ' '));
        if boundary {
            return Some(at + needle.len());
        }
        from = at + needle.len();
    }
    None
}

/// `name: "value"` → `value`.
fn field_string(body: &str, key: &str) -> Option<String> {
    let start = field_value_start(body, key)?;
    let rest = body[start..].strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// `key: !NN` → `NN`.
fn field_meta_ref(body: &str, key: &str) -> Option<u64> {
    let start = field_value_start(body, key)?;
    let rest = body[start..].strip_prefix('!')?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// `key: N` → `N` (decimal uint, not a metadata ref).
fn field_uint(body: &str, key: &str) -> Option<usize> {
    let start = field_value_start(body, key)?;
    let rest = &body[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse().ok()
}

/// Strip generic args: `call_mut<...>` → `call_mut`.
fn base_name(name: &str) -> String {
    name.find('<')
        .map_or_else(|| name.to_string(), |i| name[..i].to_string())
}

/// Normalize a `DIFile` filename. `rustc` sometimes appends a synthetic
/// `/@/<hash>` suffix to the crate-root file (e.g. `…/src/lib.rs/@/abc`);
/// strip it so the path is the real workspace-relative source file.
fn normalize_path(p: &str) -> String {
    let p = p.replace('\\', "/");
    match p.split_once("/@/") {
        Some((base, _)) => base.to_string(),
        None => p,
    }
}

/// A workspace function lives under `crates/<crate>/src/...`.
fn is_workspace_file(file: &str) -> bool {
    file.starts_with("crates/") && file.contains("/src/")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
define void @"_ZN3foo3barE"() unnamed_addr #0 !dbg !10 {
start:
  %0 = call i32 @"_ZN3foo6helperE"()
  call void @"_ZN3foo7indirectE"()
  %vt = load ptr, ptr %slot
  call void %vt(ptr %self)
  ret void
}

define i32 @"_ZN3foo6helperE"() unnamed_addr #0 !dbg !20 {
start:
  %0 = call i32 @"_ZN3std4core3extE"()
  ret i32 0
}

define void @"_ZN3foo7indirectE"() unnamed_addr #0 !dbg !30 {
start:
  ret void
}

!10 = distinct !DISubprogram(name: "bar", scope: !2, file: !100, line: 12, type: !3, unit: !1)
!20 = distinct !DISubprogram(name: "helper<u8>", scope: !2, file: !100, line: 30, type: !3, unit: !1)
!30 = distinct !DISubprogram(name: "indirect", scope: !2, file: !100, line: 45, type: !3, unit: !1)
!100 = !DIFile(filename: "crates/foo/src/lib.rs", directory: "/abs/path")
"#;

    #[test]
    fn callgraph_parses_nodes_edges_and_skips_indirect() {
        let defines = parse_ll(SAMPLE);
        // Three workspace defines: bar, helper, indirect.
        assert_eq!(defines.len(), 3, "expected three workspace defines");

        let bar = defines
            .iter()
            .find(|d| d.name == "bar")
            .expect("bar define present");
        assert_eq!(bar.file, "crates/foo/src/lib.rs");
        assert_eq!(bar.line, 12);

        // bar -> helper is a direct workspace call.
        assert!(bar.callees.contains(&"_ZN3foo6helperE".to_string()));
        // bar -> indirect (the named one) is also a direct call.
        assert!(bar.callees.contains(&"_ZN3foo7indirectE".to_string()));

        // Generic args stripped: helper<u8> -> helper.
        let helper = defines
            .iter()
            .find(|d| d.symbol == "_ZN3foo6helperE")
            .expect("helper define present");
        assert_eq!(helper.name, "helper");
        assert_eq!(helper.line, 30);

        let graph = build_graph(&defines);
        // Nodes: one per source line (12, 30, 45).
        assert_eq!(graph.node_count(), 3);

        let bar_node = NodeKey {
            file: "crates/foo/src/lib.rs".to_string(),
            line: 12,
        };
        let helper_node = NodeKey {
            file: "crates/foo/src/lib.rs".to_string(),
            line: 30,
        };
        let indirect_node = NodeKey {
            file: "crates/foo/src/lib.rs".to_string(),
            line: 45,
        };
        let bar_edges = graph.edges.get(&bar_node).expect("bar has edges");
        // Direct edges to helper and the named indirect-symbol define.
        assert!(bar_edges.contains(&helper_node));
        assert!(bar_edges.contains(&indirect_node));
        // The vtable/register call (`%vt(...)`) produced no @symbol → no extra edge.
        assert_eq!(bar_edges.len(), 2);

        // helper calls a NON-workspace symbol (std core ext) → no edge.
        assert!(graph.edges.get(&helper_node).is_none());
    }

    #[test]
    fn non_workspace_define_is_excluded() {
        let ir = r#"
define void @"_ZN3std3extE"() !dbg !1 {
  ret void
}
!1 = distinct !DISubprogram(name: "ext", file: !2, line: 3, unit: !0)
!2 = !DIFile(filename: "/rustc/abc/library/std/src/lib.rs", directory: "/")
"#;
        let defines = parse_ll(ir);
        assert!(defines.is_empty(), "std define must be excluded");
    }

    #[test]
    fn subprogram_without_name_is_skipped() {
        let ir = r#"
define void @"_ZN3foo9compilerE"() !dbg !1 {
  ret void
}
!1 = distinct !DISubprogram(scope: !2, file: !2, line: 3, unit: !0)
!2 = !DIFile(filename: "crates/foo/src/lib.rs", directory: "/")
"#;
        let defines = parse_ll(ir);
        assert!(defines.is_empty(), "nameless subprogram must be skipped");
    }

    #[test]
    fn base_name_strips_generics() {
        assert_eq!(base_name("call_mut<closure_env#0>"), "call_mut");
        assert_eq!(base_name("plain"), "plain");
    }
}
