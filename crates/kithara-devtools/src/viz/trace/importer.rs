use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead as _, BufReader},
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::{TRACE_SCHEMA_VERSION, TraceRecord, TraceRecordKind, TraceSource};
use crate::viz::graph::{
    Edge, EdgeKind, Evidence, EvidenceGraph, Node, NodeId, NodeKind, SourceLocation,
};

const TRACE_EVENT_BUDGET: usize = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TraceState {
    Complete,
    Empty,
    Truncated,
    Failed,
}

#[derive(Debug, Serialize)]
pub(crate) struct TraceSummary {
    pub(crate) state: TraceState,
    pub(crate) records: usize,
    pub(crate) matched_records: usize,
    pub(crate) unmatched_records: usize,
    pub(crate) diagnostics: Vec<String>,
}

pub(crate) fn import(
    path: &Path,
    scenario: &str,
    graph: &mut EvidenceGraph,
    manual: bool,
) -> TraceSummary {
    match read(path) {
        Ok(records) => import_records(records, scenario, graph, manual),
        Err(error) => TraceSummary {
            state: TraceState::Failed,
            records: 0,
            matched_records: 0,
            unmatched_records: 0,
            diagnostics: vec![error.to_string()],
        },
    }
}

fn read(path: &Path) -> Result<Vec<TraceRecord>> {
    let file = File::open(path).with_context(|| format!("open trace: {}", path.display()))?;
    let mut records = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("read trace line {}", index + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let record: TraceRecord = serde_json::from_str(&line)
            .with_context(|| format!("parse trace line {}", index + 1))?;
        if record.schema_version != TRACE_SCHEMA_VERSION {
            bail!(
                "trace line {} uses schema {}, expected {}",
                index + 1,
                record.schema_version,
                TRACE_SCHEMA_VERSION
            );
        }
        records.push(record);
    }
    Ok(records)
}

fn import_records(
    records: Vec<TraceRecord>,
    scenario: &str,
    graph: &mut EvidenceGraph,
    manual: bool,
) -> TraceSummary {
    if records.is_empty() {
        return TraceSummary {
            state: TraceState::Empty,
            records: 0,
            matched_records: 0,
            unmatched_records: 0,
            diagnostics: vec!["trace contains no records".to_string()],
        };
    }
    let total = records.len();
    let truncated = total > TRACE_EVENT_BUDGET;
    let mut context = ImportContext::new(graph, scenario, manual);
    for record in records.into_iter().take(TRACE_EVENT_BUDGET) {
        context.import_record(&record);
    }
    context.finish(total, truncated)
}

struct ImportContext<'a> {
    graph: &'a mut EvidenceGraph,
    source_index: SourceIndex,
    scenario: &'a str,
    scenario_id: NodeId,
    manual: bool,
    spans: BTreeMap<String, NodeId>,
    sends: BTreeMap<String, NodeId>,
    tasks: BTreeMap<String, NodeId>,
    matched_records: usize,
    diagnostics: Vec<String>,
    first_record: bool,
}

impl<'a> ImportContext<'a> {
    fn new(graph: &'a mut EvidenceGraph, scenario: &'a str, manual: bool) -> Self {
        let source_index = SourceIndex::new(graph);
        let scenario_id = NodeId::symbol("<runtime>", scenario, "<scenario>", "<scenario>");
        graph.merge_node(Node::new(
            scenario_id.clone(),
            NodeKind::Scenario,
            scenario,
            trace_evidence(manual, format!("trace:{scenario}")),
        ));
        Self {
            graph,
            source_index,
            scenario,
            scenario_id,
            manual,
            spans: BTreeMap::new(),
            sends: BTreeMap::new(),
            tasks: BTreeMap::new(),
            matched_records: 0,
            diagnostics: Vec::new(),
            first_record: true,
        }
    }

    fn import_record(&mut self, record: &TraceRecord) {
        let evidence = self.evidence(record.sequence);
        let current = self.current_id(record);
        self.attach_current(record, &current, &evidence);
        self.link_context(record, &current, &evidence);
        self.apply_kind(record, &current, &evidence);
        self.link_receive(record, &current);
    }

    fn current_id(&self, record: &TraceRecord) -> NodeId {
        self.source_index
            .resolve(record)
            .cloned()
            .unwrap_or_else(|| {
                NodeId::symbol(
                    "<runtime>",
                    self.scenario,
                    "<trace>",
                    format!("{:020}:{}", record.sequence, record.name),
                )
            })
    }

    fn attach_current(&mut self, record: &TraceRecord, current: &NodeId, evidence: &Evidence) {
        if self.graph.add_node_evidence(current, evidence.clone()) {
            self.matched_records += 1;
        } else {
            let mut node = Node::new(
                current.clone(),
                NodeKind::RuntimeEvent,
                &record.name,
                evidence.clone(),
            );
            if let Some(source) = &record.source {
                node.location = Some(SourceLocation {
                    path: source.path.clone(),
                    line: source.line,
                    column: source.column,
                });
            }
            self.graph.merge_node(node);
        }
        self.graph.merge_edge(Edge::new(
            self.scenario_id.clone(),
            current.clone(),
            EdgeKind::Contains,
            evidence.clone(),
        ));
        if self.first_record {
            self.graph.merge_edge(Edge::new(
                self.scenario_id.clone(),
                current.clone(),
                EdgeKind::Starts,
                evidence.clone(),
            ));
            self.first_record = false;
        }
    }

    fn link_context(&mut self, record: &TraceRecord, current: &NodeId, evidence: &Evidence) {
        if let Some(parent) = record
            .parent_span_id
            .as_ref()
            .and_then(|parent| self.spans.get(parent))
        {
            self.graph.merge_edge(Edge::new(
                parent.clone(),
                current.clone(),
                EdgeKind::Calls,
                evidence.clone(),
            ));
        }
        if record.kind != TraceRecordKind::TaskSpawn
            && let Some(task) = record
                .task_id
                .as_ref()
                .and_then(|task| self.tasks.get(task))
        {
            self.graph.merge_edge(Edge::new(
                task.clone(),
                current.clone(),
                EdgeKind::Calls,
                evidence.clone(),
            ));
        }
        if let Some(span) = &record.span_id {
            self.spans.insert(span.clone(), current.clone());
        }
    }

    fn apply_kind(&mut self, record: &TraceRecord, current: &NodeId, evidence: &Evidence) {
        match record.kind {
            TraceRecordKind::TaskSpawn => self.add_task(record, current, evidence),
            TraceRecordKind::ResourceCreate => {
                self.add_resource(record, current, EdgeKind::Constructs, evidence);
            }
            TraceRecordKind::ResourceClone => {
                self.add_resource(record, current, EdgeKind::Clones, evidence);
            }
            TraceRecordKind::ResourceTransfer | TraceRecordKind::Receive => {
                self.add_resource(record, current, EdgeKind::Transfers, evidence);
            }
            TraceRecordKind::ResourceDrop => {
                self.add_resource(record, current, EdgeKind::Drops, evidence);
            }
            TraceRecordKind::Send => {
                if let Some(correlation) = &record.correlation_id {
                    self.sends.insert(correlation.clone(), current.clone());
                }
                self.add_resource(record, current, EdgeKind::Sends, evidence);
            }
            TraceRecordKind::SpanEnter | TraceRecordKind::SpanExit | TraceRecordKind::Event => {}
        }
    }

    fn add_task(&mut self, record: &TraceRecord, current: &NodeId, evidence: &Evidence) {
        let task_name = record.task_id.as_deref().unwrap_or(&record.name);
        let task = NodeId::site(
            "<runtime>",
            self.scenario,
            "<task>",
            &format!("task:{task_name}"),
        );
        self.graph.merge_node(Node::new(
            task.clone(),
            NodeKind::Task,
            task_name,
            evidence.clone(),
        ));
        self.graph.merge_edge(Edge::new(
            current.clone(),
            task.clone(),
            EdgeKind::Spawns,
            evidence.clone(),
        ));
        self.tasks.insert(task_name.to_string(), task);
    }

    fn add_resource(
        &mut self,
        record: &TraceRecord,
        current: &NodeId,
        kind: EdgeKind,
        evidence: &Evidence,
    ) {
        let (Some(resource_id), Some(resource_type)) = (
            record.resource_id.as_deref(),
            record.resource_type.as_deref(),
        ) else {
            self.diagnostics.push(format!(
                "trace record {} `{}` has no resource identity",
                record.sequence, record.name
            ));
            return;
        };
        let resource = NodeId::resource(
            "<runtime>",
            self.scenario,
            &format!("{resource_type}:{resource_id}"),
        );
        self.graph.merge_node(Node::new(
            resource.clone(),
            NodeKind::Resource,
            format!("{resource_type} #{resource_id}"),
            evidence.clone(),
        ));
        self.graph
            .merge_edge(Edge::new(current.clone(), resource, kind, evidence.clone()));
    }

    fn link_receive(&mut self, record: &TraceRecord, current: &NodeId) {
        if record.kind != TraceRecordKind::Receive {
            return;
        }
        let Some(sender) = record
            .correlation_id
            .as_ref()
            .and_then(|correlation| self.sends.get(correlation))
        else {
            return;
        };
        self.graph.merge_edge(Edge::new(
            sender.clone(),
            current.clone(),
            EdgeKind::Sends,
            self.evidence(record.sequence),
        ));
    }

    fn evidence(&self, sequence: u64) -> Evidence {
        trace_evidence(self.manual, format!("trace:{}:{sequence}", self.scenario))
    }

    fn finish(mut self, total: usize, truncated: bool) -> TraceSummary {
        if truncated {
            self.diagnostics.push(format!(
                "trace has {total} records; imported first {TRACE_EVENT_BUDGET}"
            ));
        }
        let imported = total.min(TRACE_EVENT_BUDGET);
        TraceSummary {
            state: if truncated {
                TraceState::Truncated
            } else {
                TraceState::Complete
            },
            records: imported,
            matched_records: self.matched_records,
            unmatched_records: imported - self.matched_records,
            diagnostics: self.diagnostics,
        }
    }
}

fn trace_evidence(manual: bool, origin: String) -> Evidence {
    if manual {
        Evidence::manual(origin)
    } else {
        Evidence::observed(origin)
    }
}

struct SourceIndex {
    nodes: BTreeMap<(String, usize), Vec<NodeId>>,
    symbols: BTreeMap<String, Vec<NodeId>>,
}

impl SourceIndex {
    fn new(graph: &EvidenceGraph) -> Self {
        let mut nodes = BTreeMap::new();
        let mut symbols = BTreeMap::new();
        for node in graph.nodes() {
            if !matches!(
                node.kind,
                NodeKind::Function | NodeKind::PublicFunction | NodeKind::Constructor
            ) {
                continue;
            }
            symbols
                .entry(node.id.symbol.clone())
                .or_insert_with(Vec::new)
                .push(node.id.clone());
            if let Some(location) = &node.location {
                nodes
                    .entry((location.path.clone(), location.line))
                    .or_insert_with(Vec::new)
                    .push(node.id.clone());
            }
        }
        Self { nodes, symbols }
    }

    fn resolve(&self, record: &TraceRecord) -> Option<&NodeId> {
        if let Some(source) = &record.source
            && let Some(resolved) = self.resolve_source(source, &record.name)
        {
            return Some(resolved);
        }
        let candidates = self.symbols.get(&record.name)?;
        (candidates.len() == 1).then(|| &candidates[0])
    }

    fn resolve_source(&self, source: &TraceSource, name: &str) -> Option<&NodeId> {
        let exact = self.nodes.get(&(source.path.clone(), source.line));
        let candidates = exact.or_else(|| {
            self.nodes
                .range((source.path.clone(), 0)..=(source.path.clone(), source.line))
                .next_back()
                .map(|(_, candidates)| candidates)
        })?;
        let matching = candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .symbol
                    .rsplit("::")
                    .next()
                    .is_some_and(|symbol| symbol == name)
            })
            .collect::<Vec<_>>();
        if matching.len() == 1 {
            matching.into_iter().next()
        } else if candidates.len() == 1 {
            candidates.first()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::viz::trace::TraceWriter;

    #[test]
    fn trace_round_trip_preserves_versioned_records() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("trace.jsonl");
        let record = TraceRecord::new(1, TraceRecordKind::Send, "send")
            .with_source(TraceSource::new("src/lib.rs", 4, 2))
            .with_task("worker")
            .with_thread("thread-1")
            .with_correlation("message-1")
            .with_resource("Buffer", "7");
        let mut writer = TraceWriter::create(&path).expect("writer");
        writer.write(&record).expect("record");
        writer.finish().expect("finish");

        assert_eq!(read(&path).expect("trace"), vec![record]);
    }

    #[test]
    fn schema_mismatch_and_malformed_json_are_rejected() {
        let temp = tempdir().expect("tempdir");
        let mismatch = temp.path().join("mismatch.jsonl");
        std::fs::write(
            &mismatch,
            r#"{"schema_version":99,"sequence":1,"kind":"event","name":"event","source":null,"span_id":null,"parent_span_id":null,"task_id":null,"thread_id":null,"correlation_id":null,"resource_id":null,"resource_type":null}"#,
        )
        .expect("mismatch");
        assert!(
            read(&mismatch)
                .expect_err("schema")
                .to_string()
                .contains("schema")
        );

        let malformed = temp.path().join("malformed.jsonl");
        std::fs::write(&malformed, "{not json}\n").expect("malformed");
        assert!(read(&malformed).is_err());
    }

    #[test]
    fn cross_thread_receive_requires_explicit_correlation() {
        let send = TraceRecord::new(1, TraceRecordKind::Send, "send")
            .with_thread("one")
            .with_resource("Message", "1");
        let receive = TraceRecord::new(2, TraceRecordKind::Receive, "receive")
            .with_thread("two")
            .with_resource("Message", "1");
        let mut graph = EvidenceGraph::default();

        let summary = import_records(vec![send, receive], "demo", &mut graph, false);

        assert_eq!(summary.state, TraceState::Complete);
        assert!(!graph.edges().any(|edge| {
            edge.kind == EdgeKind::Sends
                && edge.source.module == "<trace>"
                && edge.target.module == "<trace>"
        }));
    }

    #[test]
    fn correlated_send_and_receive_create_an_observed_edge() {
        let send = TraceRecord::new(1, TraceRecordKind::Send, "send")
            .with_correlation("message-1")
            .with_resource("Message", "1");
        let receive = TraceRecord::new(2, TraceRecordKind::Receive, "receive")
            .with_correlation("message-1")
            .with_resource("Message", "1");
        let mut graph = EvidenceGraph::default();

        import_records(vec![send, receive], "demo", &mut graph, false);

        assert!(graph.edges().any(|edge| {
            edge.kind == EdgeKind::Sends
                && edge.source.module == "<trace>"
                && edge.target.module == "<trace>"
        }));
    }
}
