use std::{
    fs::File,
    io::{BufWriter, Write as _},
    path::Path,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const TRACE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum TraceRecordKind {
    SpanEnter,
    SpanExit,
    Event,
    TaskSpawn,
    ResourceCreate,
    ResourceClone,
    ResourceTransfer,
    ResourceDrop,
    Send,
    Receive,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct TraceSource {
    pub path: String,
    pub column: usize,
    pub line: usize,
}

impl TraceSource {
    #[must_use]
    pub fn new<P: Into<String>>(path: P, line: usize, column: usize) -> Self {
        Self {
            line,
            column,
            path: path.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, fieldwork::Fieldwork)]
#[non_exhaustive]
#[serde(deny_unknown_fields)]
#[fieldwork(opt_in, with)]
pub struct TraceRecord {
    #[serde(default)]
    pub(super) correlation_id: Option<String>,
    #[serde(default)]
    pub(super) parent_span_id: Option<String>,
    #[serde(default)]
    pub(super) resource_id: Option<String>,
    #[serde(default)]
    pub(super) resource_type: Option<String>,
    #[serde(default)]
    #[field(with, option_set_some)]
    pub(super) source: Option<TraceSource>,
    #[serde(default)]
    pub(super) span_id: Option<String>,
    #[serde(default)]
    pub(super) task_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    pub(super) name: String,
    pub(super) kind: TraceRecordKind,
    pub(super) schema_version: u32,
    pub(super) sequence: u64,
}

impl TraceRecord {
    #[must_use]
    pub fn new<N: Into<String>>(sequence: u64, kind: TraceRecordKind, name: N) -> Self {
        Self {
            sequence,
            kind,
            schema_version: TRACE_SCHEMA_VERSION,
            name: name.into(),
            source: None,
            span_id: None,
            parent_span_id: None,
            task_id: None,
            thread_id: None,
            correlation_id: None,
            resource_id: None,
            resource_type: None,
        }
    }

    #[must_use]
    pub fn with_correlation<C: Into<String>>(mut self, correlation_id: C) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    #[must_use]
    pub fn with_parent_span<S: Into<String>>(mut self, span_id: S) -> Self {
        self.parent_span_id = Some(span_id.into());
        self
    }

    #[must_use]
    pub fn with_resource<T: Into<String>, I: Into<String>>(
        mut self,
        resource_type: T,
        resource_id: I,
    ) -> Self {
        self.resource_type = Some(resource_type.into());
        self.resource_id = Some(resource_id.into());
        self
    }

    #[must_use]
    pub fn with_span<S: Into<String>>(mut self, span_id: S) -> Self {
        self.span_id = Some(span_id.into());
        self
    }

    #[must_use]
    pub fn with_task<T: Into<String>>(mut self, task_id: T) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    #[must_use]
    pub fn with_thread<T: Into<String>>(mut self, thread_id: T) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }
}

pub struct TraceWriter {
    writer: BufWriter<File>,
}

impl TraceWriter {
    /// Creates a versioned JSONL trace at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error when the trace file cannot be created.
    pub fn create(path: &Path) -> Result<Self> {
        let file =
            File::create(path).with_context(|| format!("create trace: {}", path.display()))?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    /// Flushes the trace to disk.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying file cannot be flushed.
    pub fn finish(mut self) -> Result<()> {
        self.writer.flush().context("flush architecture trace")
    }

    /// Appends one neutral trace record.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization or writing fails.
    pub fn write(&mut self, record: &TraceRecord) -> Result<()> {
        serde_json::to_writer(&mut self.writer, record)?;
        self.writer.write_all(b"\n")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn writer_preserves_records_as_versioned_json_lines() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("trace.jsonl");
        let records = [
            TraceRecord::new(1, TraceRecordKind::Send, "send")
                .with_source(TraceSource::new("src/lib.rs", 4, 2))
                .with_span("producer")
                .with_parent_span("root")
                .with_task("worker")
                .with_thread("thread-1")
                .with_correlation("message-1")
                .with_resource("Buffer", "7"),
            TraceRecord::new(2, TraceRecordKind::Receive, "receive").with_correlation("message-1"),
        ];
        let mut writer = TraceWriter::create(&path).expect("writer");
        for record in &records {
            writer.write(record).expect("record");
        }
        writer.finish().expect("finish");
        let jsonl = fs::read_to_string(&path).expect("read trace");
        let decoded = jsonl
            .lines()
            .map(|line| serde_json::from_str::<TraceRecord>(line).expect("trace record"))
            .collect::<Vec<_>>();

        assert!(jsonl.ends_with('\n'));
        assert_eq!(decoded, records);
        assert!(
            decoded
                .iter()
                .all(|record| record.schema_version == TRACE_SCHEMA_VERSION)
        );
    }
}
