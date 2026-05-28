mod parser;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use clap::Args;
use parser::{CallGraph, ParsedDefine, build_graph, parse_ll};
use serde_json::{Map, Value, json};
use syn::{ImplItem, Item, ItemImpl, spanned::Spanned};

use crate::common::{
    parse::parse_file,
    project::ProjectConfig,
    walker::{relative_to, walk_rs_files},
};

#[derive(Debug, Args)]
pub(crate) struct CallgraphArgs {
    /// Reuse existing `target/callgraph-ir` IR instead of re-emitting it.
    #[arg(long)]
    pub(crate) no_emit: bool,
}

/// Isolated target dir for IR emission, kept off the normal build cache. The
/// checked-in output lives at `<workspace>/.config/idioms/callgraph.json`.
const TARGET_DIR: &str = "target/callgraph-ir";

pub(crate) fn run(args: &CallgraphArgs) -> Result<()> {
    let workspace_root = workspace_root()?;
    if !args.no_emit {
        emit_ir(&workspace_root)?;
    }
    let deps_dir = workspace_root.join(TARGET_DIR).join("debug/deps");
    let defines = parse_all_ir(&deps_dir)?;
    let graph = build_graph(&defines);

    let join = validate_join(&workspace_root, &graph)?;
    print_join_report(&join);

    let output_rel = ".config/idioms/callgraph.json";
    let output = workspace_root.join(output_rel);
    let rustc_version = rustc_version()?;
    write_callgraph(&output, &graph, &rustc_version)?;

    println!(
        "callgraph.json: {} nodes, {} edges -> {output_rel}",
        graph.node_count(),
        graph.edge_count(),
    );
    Ok(())
}

fn emit_ir(workspace_root: &Path) -> Result<()> {
    println!("emitting LLVM-IR into {TARGET_DIR} (slow, opt-in)...");
    let config = ProjectConfig::load(workspace_root)?;
    let mut args = vec!["build".to_owned(), "--workspace".to_owned()];
    for krate in &config.callgraph.workspace_exclude {
        args.push("--exclude".to_owned());
        args.push(krate.clone());
    }
    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .args(&args)
        .env("CARGO_TARGET_DIR", TARGET_DIR)
        .env(
            "RUSTFLAGS",
            "--emit=llvm-ir -C debuginfo=1 -C codegen-units=1",
        )
        .status()
        .context("spawn cargo build for LLVM-IR emission")?;
    if !status.success() {
        anyhow::bail!("LLVM-IR emission build failed");
    }
    Ok(())
}

fn parse_all_ir(deps_dir: &Path) -> Result<Vec<ParsedDefine>> {
    let mut ll_files: Vec<PathBuf> = std::fs::read_dir(deps_dir)
        .with_context(|| {
            format!(
                "read IR deps dir: {} (run without --no-emit?)",
                deps_dir.display()
            )
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ll"))
        .collect();
    ll_files.sort();

    let mut defines = Vec::new();
    for path in &ll_files {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("read IR file: {}", path.display()))?;
        defines.extend(parse_ll(&source));
    }
    Ok(defines)
}

/// Declaration line of an AST function candidate for the source-join. Several
/// candidates can share a `(file, base_name)` key (same-named methods on
/// different types in one file); nearest-line wins against the IR line.
type AstFnLine = usize;

/// `(rel_file, base_name) -> candidate declaration lines`.
type AstFnIndex = BTreeMap<(String, String), Vec<AstFnLine>>;

/// AST source index used for the join: hand-written functions plus type
/// declaration lines (so `#[derive(...)]`-generated trait methods, whose IR
/// `DISubprogram` line points at the type, can be attributed to source).
struct AstIndex {
    fns: AstFnIndex,
    /// `(rel_file, line)` for every `struct`/`enum`/`union` declaration. The
    /// line is the item span start (the first `#[derive]` attribute line when
    /// present), matching where `rustc` anchors derived methods.
    type_decls: BTreeSet<(String, usize)>,
}

/// Result of joining IR define-nodes to AST functions. `named_*` excludes
/// compiler-synthesized subprograms (`{closure#N}`, `{async_block#N}`, ...) that
/// have no hand-written source `fn` to match — the rate that matters for the
/// population a name-keyed consumer (phase B) can act on. `derive_matched`
/// counts derive-generated methods attributed to a type declaration line.
struct JoinReport {
    total: usize,
    matched: usize,
    derive_matched: usize,
    named_total: usize,
    named_matched: usize,
    unmatched: Vec<String>,
}

/// A compiler-synthesized subprogram name like `{closure#0}` / `{async_block#0}`
/// has no named source `fn` and is structurally unmatchable by `(file, name)`.
fn is_synthetic_name(name: &str) -> bool {
    name.starts_with('{')
}

/// Outcome of joining one IR node to source.
enum Join {
    /// Matched a hand-written `fn` declaration (nearest AST line).
    Fn,
    /// Matched a `#[derive]`-generated trait method, attributed to its type.
    Derive,
    /// No source attribution found.
    Miss,
}

fn validate_join(workspace_root: &Path, graph: &CallGraph) -> Result<JoinReport> {
    let ast = collect_ast(workspace_root)?;
    let mut matched = 0;
    let mut derive_matched = 0;
    let mut named_total = 0;
    let mut named_matched = 0;
    let mut unmatched = Vec::new();
    for (node, name) in &graph.nodes {
        let synthetic = is_synthetic_name(name);
        if !synthetic {
            named_total += 1;
        }
        match join_node(&ast, &node.file, name, node.line) {
            Join::Fn => {
                matched += 1;
                if !synthetic {
                    named_matched += 1;
                }
            }
            Join::Derive => {
                matched += 1;
                derive_matched += 1;
                if !synthetic {
                    named_matched += 1;
                }
            }
            Join::Miss => {
                if !synthetic {
                    // Synthetic names are expected misses; surface only real ones.
                    unmatched.push(format!("{}:{}:{}", node.file, node.line, name));
                }
            }
        }
    }
    Ok(JoinReport {
        total: graph.nodes.len(),
        matched,
        derive_matched,
        named_total,
        named_matched,
        unmatched,
    })
}

/// Join one IR node to source. Primary key `(file, base_name)` for hand-written
/// functions, disambiguating multiple candidates by the AST declaration line
/// nearest to the IR `DISubprogram` line. Falls back to attributing a known
/// derive-method name to a type declaration on the same line.
fn join_node(ast: &AstIndex, file: &str, name: &str, ir_line: usize) -> Join {
    // Method names emitted by `#[derive(...)]` / auto trait impls. These have no
    // hand-written source `fn`; the IR attributes them to the type declaration.
    const DERIVE_METHODS: &[&str] = &[
        "clone",
        "clone_from",
        "fmt",
        "default",
        "eq",
        "ne",
        "hash",
        "cmp",
        "partial_cmp",
        "lt",
        "le",
        "gt",
        "ge",
        "drop",
        "from",
        "augment_args",
        "augment_args_for_update",
    ];

    if let Some(lines) = ast.fns.get(&(file.to_string(), name.to_string())) {
        // Nearest-line disambiguation among same-name candidates.
        if lines
            .iter()
            .copied()
            .min_by_key(|line| line.abs_diff(ir_line))
            .is_some()
        {
            return Join::Fn;
        }
    }
    if DERIVE_METHODS.contains(&name) && ast.type_decls.contains(&(file.to_string(), ir_line)) {
        return Join::Derive;
    }
    Join::Miss
}

fn collect_ast(workspace_root: &Path) -> Result<AstIndex> {
    let mut fns = AstFnIndex::new();
    let mut type_decls = BTreeSet::new();
    let crates_dir = workspace_root.join("crates");
    for path in walk_rs_files(&crates_dir)? {
        let rel = relative_to(workspace_root, &path)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(file) = parse_file(&path) else {
            continue;
        };
        collect_file_fns(&file.items, &rel, &mut fns);
        collect_type_decls(&file.items, &rel, &mut type_decls);
    }
    Ok(AstIndex { fns, type_decls })
}

fn collect_type_decls(items: &[Item], rel: &str, out: &mut BTreeSet<(String, usize)>) {
    for item in items {
        match item {
            Item::Struct(s) => insert_type_lines(out, rel, &s.attrs, s.span()),
            Item::Enum(e) => insert_type_lines(out, rel, &e.attrs, e.span()),
            Item::Union(u) => insert_type_lines(out, rel, &u.attrs, u.span()),
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect_type_decls(inner, rel, out);
                }
            }
            _ => {}
        }
    }
}

/// Record every source line a `rustc`-derived method might be attributed to: the
/// `struct`/`enum` keyword line and each `#[derive(...)]` attribute line above
/// it. `proc-macro2` reports the item span at the keyword, not the attributes.
fn insert_type_lines(
    out: &mut BTreeSet<(String, usize)>,
    rel: &str,
    attrs: &[syn::Attribute],
    item_span: proc_macro2::Span,
) {
    out.insert((rel.to_string(), item_span.start().line));
    for attr in attrs {
        out.insert((rel.to_string(), attr.span().start().line));
    }
}

fn collect_file_fns(items: &[Item], rel: &str, out: &mut AstFnIndex) {
    for item in items {
        match item {
            Item::Fn(f) => push_fn(out, rel, f.sig.ident.to_string(), f.span()),
            Item::Impl(im) => collect_impl_fns(im, rel, out),
            Item::Trait(t) => {
                for ti in &t.items {
                    if let syn::TraitItem::Fn(m) = ti
                        && m.default.is_some()
                    {
                        push_fn(out, rel, m.sig.ident.to_string(), m.span());
                    }
                }
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect_file_fns(inner, rel, out);
                }
            }
            _ => {}
        }
    }
}

fn collect_impl_fns(im: &ItemImpl, rel: &str, out: &mut AstFnIndex) {
    for it in &im.items {
        if let ImplItem::Fn(m) = it {
            push_fn(out, rel, m.sig.ident.to_string(), m.span());
        }
    }
}

fn push_fn(out: &mut AstFnIndex, rel: &str, name: String, span: proc_macro2::Span) {
    let line = span.start().line;
    out.entry((rel.to_string(), name)).or_default().push(line);
}

fn print_join_report(join: &JoinReport) {
    let synthetic = join.total - join.named_total;
    println!(
        "source-join: {} of {} IR workspace nodes matched source ({}; {} via #[derive])",
        join.matched,
        join.total,
        percent(join.matched, join.total),
        join.derive_matched
    );
    println!(
        "  named-only (excluding {synthetic} synthetic closures/async-blocks): \
         {} of {} matched ({})",
        join.named_matched,
        join.named_total,
        percent(join.named_matched, join.named_total)
    );
    if !join.unmatched.is_empty() {
        println!(
            "unmatched named examples ({} total, up to 10):",
            join.unmatched.len()
        );
        for ex in join.unmatched.iter().take(10) {
            println!("  {ex}");
        }
    }
}

/// Format `num / den` as a percentage with one decimal, using integer math to
/// avoid a lossy `usize -> f64` cast.
fn percent(num: usize, den: usize) -> String {
    if den == 0 {
        return "0.0%".to_string();
    }
    let permille = num * 1000 / den;
    format!("{}.{}%", permille / 10, permille % 10)
}

fn write_callgraph(output: &Path, graph: &CallGraph, rustc_version: &str) -> Result<()> {
    let mut nodes = Map::new();
    for (node, name) in &graph.nodes {
        nodes.insert(node.label(), Value::String(name.clone()));
    }
    let mut edges = Map::new();
    for (node, targets) in &graph.edges {
        let mut arr: Vec<Value> = targets.iter().map(|t| Value::String(t.label())).collect();
        arr.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        edges.insert(node.label(), Value::Array(arr));
    }
    let doc = json!({
        "version": 1,
        "generated_with": rustc_version,
        "nodes": Value::Object(nodes),
        "edges": Value::Object(edges),
    });
    let mut text = serde_json::to_string_pretty(&doc)?;
    text.push('\n');
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir: {}", parent.display()))?;
    }
    std::fs::write(output, text).with_context(|| format!("write: {}", output.display()))?;
    Ok(())
}

fn workspace_root() -> Result<PathBuf> {
    let metadata = MetadataCommand::new().no_deps().exec()?;
    Ok(metadata.workspace_root.as_std_path().to_path_buf())
}

fn rustc_version() -> Result<String> {
    let out = Command::new("rustc")
        .arg("-V")
        .output()
        .context("run rustc -V")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
