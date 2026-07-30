use std::{
    fs::File,
    io::{BufWriter, Write as _},
    path::Path,
};

use anyhow::{Context, Result};
pub(crate) use importer::{TraceState, TraceSummary, import};
use serde::{Deserialize, Serialize};

pub const TRACE_SCHEMA_VERSION: u32 = 1;

mod importer;

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
    pub line: usize,
    pub column: usize,
}

impl TraceSource {
    #[must_use]
    pub fn new<P: Into<String>>(path: P, line: usize, column: usize) -> Self {
        Self {
            path: path.into(),
            line,
            column,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
#[serde(deny_unknown_fields)]
pub struct TraceRecord {
    schema_version: u32,
    sequence: u64,
    kind: TraceRecordKind,
    name: String,
    #[serde(default)]
    source: Option<TraceSource>,
    #[serde(default)]
    span_id: Option<String>,
    #[serde(default)]
    parent_span_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    correlation_id: Option<String>,
    #[serde(default)]
    resource_id: Option<String>,
    #[serde(default)]
    resource_type: Option<String>,
}

impl TraceRecord {
    #[must_use]
    pub fn new<N: Into<String>>(sequence: u64, kind: TraceRecordKind, name: N) -> Self {
        Self {
            schema_version: TRACE_SCHEMA_VERSION,
            sequence,
            kind,
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
    pub fn with_source(mut self, source: TraceSource) -> Self {
        self.source = Some(source);
        self
    }

    #[must_use]
    pub fn with_span<S: Into<String>>(mut self, span_id: S) -> Self {
        self.span_id = Some(span_id.into());
        self
    }

    #[must_use]
    pub fn with_parent_span<S: Into<String>>(mut self, span_id: S) -> Self {
        self.parent_span_id = Some(span_id.into());
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

    #[must_use]
    pub fn with_correlation<C: Into<String>>(mut self, correlation_id: C) -> Self {
        self.correlation_id = Some(correlation_id.into());
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

    /// Flushes the trace to disk.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying file cannot be flushed.
    pub fn finish(mut self) -> Result<()> {
        self.writer.flush().context("flush architecture trace")
    }
}
