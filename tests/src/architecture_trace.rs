use std::path::Path;

use anyhow::Result;
use kithara_devtools::viz::trace::{TraceRecord, TraceRecordKind, TraceSource, TraceWriter};
use kithara_test_utils::probe::capture::Recorder;

pub fn write(
    path: &Path,
    records: impl IntoIterator<Item = TraceRecord>,
    probes: &Recorder,
) -> Result<()> {
    let mut writer = TraceWriter::create(path)?;
    for record in records {
        writer.write(&record)?;
    }
    let mut events = probes.snapshot();
    events.sort_by_key(kithara_test_utils::probe::capture::ProbeEvent::seq);
    for (index, event) in events.into_iter().enumerate() {
        let sequence = 10_000 + index as u64;
        let name = event
            .caller_fn()
            .and_then(symbol_name)
            .or_else(|| event.probe_name().map(str::to_string))
            .unwrap_or_else(|| "probe".to_string());
        let mut record = TraceRecord::new(sequence, TraceRecordKind::Event, name);
        if let (Some(path), Some(line)) = (event.caller_file(), event.caller_line()) {
            record =
                record.with_source(TraceSource::new(workspace_relative(path), line as usize, 0));
        }
        if let Some(thread) = event.thread_id() {
            record = record.with_thread(thread.to_string());
        }
        writer.write(&record)?;
    }
    writer.finish()
}

fn symbol_name(symbol: &str) -> Option<String> {
    let symbol = symbol.split("::{{closure}}").next().unwrap_or(symbol);
    let (prefix, method) = symbol.rsplit_once("::")?;
    let owner = prefix.rsplit_once("::").map_or(prefix, |(_, owner)| owner);
    Some(format!("{owner}::{method}"))
}

fn workspace_relative(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    for marker in ["/crates/", "/tests/", "/xtask/"] {
        if let Some((_, suffix)) = normalized.split_once(marker) {
            return format!("{}{suffix}", marker.trim_start_matches('/'));
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_symbol_is_reduced_to_owner_and_method() {
        assert_eq!(
            symbol_name("kithara_queue::queue::lifecycle::Queue::append"),
            Some("Queue::append".to_string())
        );
        assert_eq!(
            symbol_name("kithara_play::worker::run::{{closure}}"),
            Some("worker::run".to_string())
        );
    }
}
