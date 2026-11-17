use std::path::Path;

use anyhow::Result;
use kithara_devtools_trace_format::{
    TRACE_SCHEMA_VERSION, TraceRecord, TraceRecordKind, TraceWriter,
};
use serde_json::{Value, json};

use crate::usdt_trace::Scope;

/// Writes `records`, then every probe firing `probes` recorded, in firing order.
pub fn write(path: &Path, records: impl IntoIterator<Item = Value>, probes: &Scope) -> Result<()> {
    let mut writer = TraceWriter::create(path)?;
    for record in records {
        writer.write(&serde_json::from_value(record)?)?;
    }
    for (index, event) in probes.events().into_iter().enumerate() {
        let sequence = 10_000 + index as u64;
        let source = match (event.file, event.line) {
            (Some(path), Some(line)) => {
                json!({ "path": workspace_relative(path), "line": line, "column": 0 })
            }
            _ => Value::Null,
        };
        let record: TraceRecord = serde_json::from_value(json!({
            "schema_version": TRACE_SCHEMA_VERSION, "sequence": sequence,
            "kind": TraceRecordKind::Event, "name": event.probe,
            "source": source, "thread_id": format!("{:?}", event.thread)
        }))?;
        writer.write(&record)?;
    }
    writer.finish()
}

/// Trims a compile-time source path to the workspace-relative form the
/// architecture graph keys its nodes by.
fn workspace_relative(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    for marker in ["/crates/", "/tests/", "/xtask/"] {
        if let Some((_, suffix)) = normalized.split_once(marker) {
            return format!("{}{suffix}", marker.trim_start_matches('/'));
        }
    }
    normalized
}
