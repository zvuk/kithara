use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Serialize;
use url::Url;

use self::lsp::{CallHierarchyItem, Client, ClientError};
use super::{
    filter::ArchitectureFilter,
    graph::{Edge, EdgeKind, Evidence, EvidenceGraph, NodeId, NodeKind, SourceLocation},
};

mod lsp;

const SEMANTIC_TIMEOUT: Duration = Duration::from_secs(120);
const SEMANTIC_SYMBOL_BUDGET: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SemanticState {
    Complete,
    Truncated,
    Unavailable,
    TimedOut,
    Failed,
}

impl SemanticState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Truncated => "truncated",
            Self::Unavailable => "unavailable",
            Self::TimedOut => "timed_out",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct SemanticSummary {
    pub(crate) state: SemanticState,
    pub(crate) requested_symbols: usize,
    pub(crate) prepared_symbols: usize,
    pub(crate) outgoing_calls: usize,
    pub(crate) resolved_edges: usize,
    pub(crate) unmatched_targets: usize,
    pub(crate) skipped_symbols: usize,
    pub(crate) diagnostics: Vec<String>,
}

impl SemanticSummary {
    pub(crate) fn static_only(diagnostic: impl Into<String>) -> Self {
        Self {
            state: SemanticState::Unavailable,
            requested_symbols: 0,
            prepared_symbols: 0,
            outgoing_calls: 0,
            resolved_edges: 0,
            unmatched_targets: 0,
            skipped_symbols: 0,
            diagnostics: vec![diagnostic.into()],
        }
    }

    pub(crate) fn is_incomplete(&self) -> bool {
        matches!(self.state, SemanticState::TimedOut | SemanticState::Failed)
    }
}

pub(crate) fn enrich(
    graph: &mut EvidenceGraph,
    root: &Path,
    package: Option<&str>,
    module: Option<&str>,
    scenario: Option<&str>,
    filter: &ArchitectureFilter,
) -> SemanticSummary {
    let eligible = select_symbols(graph, package, module, scenario, filter);
    let skipped_symbols = eligible.len().saturating_sub(SEMANTIC_SYMBOL_BUDGET);
    let selected = eligible
        .into_iter()
        .take(SEMANTIC_SYMBOL_BUDGET)
        .collect::<Vec<_>>();

    if selected.is_empty() {
        return SemanticSummary::static_only("no source symbols selected for semantic resolution");
    }

    let mut client = match Client::start(root, SEMANTIC_TIMEOUT) {
        Ok(client) => client,
        Err(ClientError::Unavailable(message)) => {
            return SemanticSummary::static_only(message);
        }
        Err(error) => {
            return failed_summary(
                &error,
                selected.len(),
                skipped_symbols,
                SemanticProgress::default(),
            );
        }
    };

    let index = LocationIndex::new(graph, root);
    let mut progress = match resolve_selected(graph, &index, &mut client, root, &selected) {
        Ok(progress) => progress,
        Err((error, progress)) => {
            return failed_summary(&error, selected.len(), skipped_symbols, progress);
        }
    };
    if let Err(error) = client.shutdown() {
        progress
            .diagnostics
            .push(format!("rust-analyzer shutdown: {error}"));
    }
    complete_summary(selected.len(), skipped_symbols, progress)
}

type SelectedSymbol = (NodeId, SourceLocation);

fn select_symbols(
    graph: &EvidenceGraph,
    package: Option<&str>,
    module: Option<&str>,
    scenario: Option<&str>,
    filter: &ArchitectureFilter,
) -> Vec<SelectedSymbol> {
    let trace_origin = scenario.map(|scenario| format!("trace:{scenario}"));
    graph
        .nodes()
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::Function | NodeKind::PublicFunction | NodeKind::Constructor
            )
        })
        .filter(|node| package.is_none_or(|package| node.id.package == package))
        .filter(|node| !filter.excludes(&node.id))
        .filter(|node| {
            module.is_none_or(|module| {
                node.id.module == module
                    || node
                        .id
                        .module
                        .strip_prefix(module)
                        .is_some_and(|suffix| suffix.starts_with("::"))
            })
        })
        .filter(|node| {
            trace_origin.as_ref().is_none_or(|origin| {
                node.evidence
                    .iter()
                    .any(|evidence| evidence.origin.starts_with(origin))
            })
        })
        .filter_map(|node| {
            node.location
                .as_ref()
                .map(|location| (node.id.clone(), location.clone()))
        })
        .collect()
}

#[derive(Default)]
struct SemanticProgress {
    prepared_symbols: usize,
    outgoing_calls: usize,
    resolved_edges: usize,
    unmatched_targets: usize,
    diagnostics: Vec<String>,
}

type ResolutionResult = Result<SemanticProgress, (ClientError, SemanticProgress)>;

fn resolve_selected(
    graph: &mut EvidenceGraph,
    index: &LocationIndex,
    client: &mut Client,
    root: &Path,
    selected: &[SelectedSymbol],
) -> ResolutionResult {
    let mut progress = SemanticProgress::default();
    let mut opened = BTreeSet::new();
    for (source, location) in selected {
        let uri = match source_uri(root, &location.path) {
            Ok(uri) => uri,
            Err(diagnostic) => {
                progress.diagnostics.push(diagnostic);
                continue;
            }
        };
        if opened.insert(location.path.clone()) {
            let text = match fs::read_to_string(root.join(&location.path)) {
                Ok(text) => text,
                Err(error) => {
                    progress
                        .diagnostics
                        .push(format!("read semantic source {}: {error}", location.path));
                    continue;
                }
            };
            if let Err(error) = client.open(&uri, &text) {
                return Err((error, progress));
            }
        }
        let prepared = match client.prepare(&uri, location.line, location.column) {
            Ok(items) => items,
            Err(error) => {
                return Err((error, progress));
            }
        };
        let Some(item) = select_prepared(prepared, source) else {
            continue;
        };
        progress.prepared_symbols += 1;
        let outgoing = match client.outgoing(&item) {
            Ok(outgoing) => outgoing,
            Err(error) => {
                return Err((error, progress));
            }
        };
        progress.outgoing_calls += outgoing.len();
        for call in outgoing {
            let targets = index.resolve(&call.to);
            if targets.len() != 1 {
                progress.unmatched_targets += 1;
                if !targets.is_empty() {
                    progress.diagnostics.push(format!(
                        "ambiguous semantic target `{}` from `{source}`",
                        call.to.name
                    ));
                }
                continue;
            }
            let target = targets[0].clone();
            graph.merge_edge(Edge::new(
                source.clone(),
                target,
                EdgeKind::Calls,
                Evidence::resolved(format!("rust-analyzer:{}:{}", location.path, location.line)),
            ));
            progress.resolved_edges += 1;
        }
    }
    Ok(progress)
}

fn complete_summary(
    requested_symbols: usize,
    skipped_symbols: usize,
    mut progress: SemanticProgress,
) -> SemanticSummary {
    let state = if progress.prepared_symbols == 0 {
        progress
            .diagnostics
            .push("rust-analyzer returned no call hierarchy items".to_string());
        SemanticState::Failed
    } else if skipped_symbols == 0 {
        SemanticState::Complete
    } else {
        SemanticState::Truncated
    };
    SemanticSummary {
        state,
        requested_symbols,
        prepared_symbols: progress.prepared_symbols,
        outgoing_calls: progress.outgoing_calls,
        resolved_edges: progress.resolved_edges,
        unmatched_targets: progress.unmatched_targets,
        skipped_symbols,
        diagnostics: progress.diagnostics,
    }
}

fn select_prepared(items: Vec<CallHierarchyItem>, source: &NodeId) -> Option<CallHierarchyItem> {
    if items.len() == 1 {
        return items.into_iter().next();
    }
    let expected = source.symbol.rsplit("::").next().unwrap_or(&source.symbol);
    let mut matching = items
        .into_iter()
        .filter(|item| item.name == expected)
        .collect::<Vec<_>>();
    (matching.len() == 1).then(|| matching.remove(0))
}

fn source_uri(root: &Path, relative: &str) -> Result<Url, String> {
    Url::from_file_path(root.join(relative))
        .map_err(|()| format!("cannot convert source path to URI: {relative}"))
}

fn failed_summary(
    error: &ClientError,
    requested_symbols: usize,
    skipped_symbols: usize,
    mut progress: SemanticProgress,
) -> SemanticSummary {
    let state = if matches!(error, ClientError::TimedOut { .. }) {
        SemanticState::TimedOut
    } else {
        SemanticState::Failed
    };
    progress.diagnostics.push(error.to_string());
    SemanticSummary {
        state,
        requested_symbols,
        prepared_symbols: progress.prepared_symbols,
        outgoing_calls: progress.outgoing_calls,
        resolved_edges: progress.resolved_edges,
        unmatched_targets: progress.unmatched_targets,
        skipped_symbols,
        diagnostics: progress.diagnostics,
    }
}

struct LocationIndex {
    root: PathBuf,
    by_location: BTreeMap<(String, usize), Vec<NodeId>>,
}

impl LocationIndex {
    fn new(graph: &EvidenceGraph, root: &Path) -> Self {
        let mut by_location = BTreeMap::new();
        for node in graph.nodes() {
            if let Some(location) = &node.location
                && matches!(
                    node.kind,
                    NodeKind::Function | NodeKind::PublicFunction | NodeKind::Constructor
                )
            {
                by_location
                    .entry((location.path.clone(), location.line))
                    .or_insert_with(Vec::new)
                    .push(node.id.clone());
            }
        }
        Self {
            root: root.to_path_buf(),
            by_location,
        }
    }

    fn resolve(&self, item: &CallHierarchyItem) -> Vec<&NodeId> {
        let Ok(path) = item.uri.to_file_path() else {
            return Vec::new();
        };
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return Vec::new();
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        let line = item.selection_range.start.line as usize + 1;
        let Some(candidates) = self.by_location.get(&(relative, line)) else {
            return Vec::new();
        };
        let mut exact = candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .symbol
                    .rsplit("::")
                    .next()
                    .is_some_and(|name| name == item.name)
            })
            .collect::<Vec<_>>();
        if exact.is_empty() && candidates.len() == 1 {
            exact.push(&candidates[0]);
        }
        exact
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viz::graph::{Evidence, Node};

    #[test]
    fn location_index_rejects_ambiguous_same_line_targets() {
        let root = Path::new("/workspace");
        let mut graph = EvidenceGraph::default();
        for symbol in ["First::run", "Second::run"] {
            graph.merge_node(
                Node::new(
                    NodeId::symbol("demo", "demo", "runtime", symbol),
                    NodeKind::Function,
                    symbol,
                    Evidence::static_fact("fixture"),
                )
                .at("src/runtime.rs", 10),
            );
        }
        let index = LocationIndex::new(&graph, root);
        let item = CallHierarchyItem::fixture(
            "run",
            Url::from_file_path("/workspace/src/runtime.rs").expect("file URI"),
            9,
        );

        assert_eq!(index.resolve(&item).len(), 2);
    }

    #[test]
    fn prepared_item_must_match_the_source_symbol_when_ambiguous() {
        let uri = Url::parse("file:///workspace/src/lib.rs").expect("URI");
        let items = vec![
            CallHierarchyItem::fixture("other", uri.clone(), 1),
            CallHierarchyItem::fixture("start", uri, 2),
        ];
        let source = NodeId::symbol("demo", "demo", "crate", "start");

        assert_eq!(
            select_prepared(items, &source).map(|item| item.name),
            Some("start".to_string())
        );
    }

    #[test]
    fn excluded_symbols_do_not_enter_semantic_selection() {
        let mut graph = EvidenceGraph::default();
        for package in ["included", "excluded"] {
            graph.merge_node(
                Node::new(
                    NodeId::symbol(package, package, "runtime", "start"),
                    NodeKind::Function,
                    "start",
                    Evidence::static_fact("fixture"),
                )
                .at(format!("{package}/src/lib.rs"), 1),
            );
        }
        let filter =
            ArchitectureFilter::compile(vec!["excluded".to_string()], Vec::new()).expect("filter");

        let selected = select_symbols(&graph, None, None, None, &filter);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0.package, "included");
    }
}
