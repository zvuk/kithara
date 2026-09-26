use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{Builder, JoinHandle},
};

use kithara_platform::{
    flash, logging,
    time::{Duration, SystemTime},
};
use serde::Serialize;
use serde_json::Value;

use super::shared::{HangDump, NoContext};

const _: () = assert!(
    consts::MAX_BOUNDED_INPUT_BYTES * consts::MAX_JSON_EXPANSION + consts::ENVELOPE_OVERHEAD_BYTES
        < consts::MAX_ENVELOPE_BYTES
);

static NEXT_DUMP_ID: AtomicU64 = AtomicU64::new(0);

/// Sanitize and bound a label for use in a dump filename.
#[must_use]
pub(crate) fn sanitize_label(label: &str) -> String {
    const MAX_FILENAME_LABEL_CHARS: usize = 96;

    let sanitized: String = label
        .chars()
        .take(MAX_FILENAME_LABEL_CHARS)
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            _ => '.',
        })
        .collect();
    if sanitized.is_empty() {
        "unknown".to_owned()
    } else {
        sanitized
    }
}

mod consts {
    pub(super) const ENV_DUMP_DIR: &str = "KITHARA_HANG_DUMP_DIR";
    pub(super) const ENV_PREKILL_SECS: &str = "KITHARA_HANG_PREKILL_SECS";
    pub(super) const ENV_TIMEOUT_SECS: &str = "KITHARA_HANG_TIMEOUT_SECS";
    pub(super) const NEXTEST_ATTEMPT: &str = "NEXTEST_ATTEMPT";
    pub(super) const NEXTEST_ATTEMPT_ID: &str = "NEXTEST_ATTEMPT_ID";
    pub(super) const NEXTEST_BINARY_ID: &str = "NEXTEST_BINARY_ID";
    pub(super) const NEXTEST_RUN_ID: &str = "NEXTEST_RUN_ID";
    pub(super) const NEXTEST_STRESS_CURRENT: &str = "NEXTEST_STRESS_CURRENT";
    pub(super) const NEXTEST_STRESS_TOTAL: &str = "NEXTEST_STRESS_TOTAL";
    pub(super) const NEXTEST_TEST_GLOBAL_SLOT: &str = "NEXTEST_TEST_GLOBAL_SLOT";
    pub(super) const NEXTEST_TEST_GROUP: &str = "NEXTEST_TEST_GROUP";
    pub(super) const NEXTEST_TEST_GROUP_SLOT: &str = "NEXTEST_TEST_GROUP_SLOT";
    pub(super) const NEXTEST_TEST_NAME: &str = "NEXTEST_TEST_NAME";
    pub(super) const NEXTEST_TEST_THREADS: &str = "NEXTEST_TEST_THREADS";
    pub(super) const NEXTEST_TOTAL_ATTEMPTS: &str = "NEXTEST_TOTAL_ATTEMPTS";

    pub(super) const MAX_ENVELOPE_BYTES: usize = 4 * 1024 * 1024;

    pub(super) const MAX_NEXTEST_FIELD_BYTES: usize = 8 * 1024;

    pub(super) const MAX_LABEL_BYTES: usize = 8 * 1024;

    pub(super) const MAX_DIAGNOSTIC_BYTES: usize = 32 * 1024;

    pub(super) const MAX_CONTEXT_BYTES: usize = 192 * 1024;

    pub(super) const MAX_FLASH_BYTES: usize = 256 * 1024;

    pub(super) const MAX_FLIGHT_CHANNEL_BYTES: usize = 32 * 1024;

    pub(super) const MAX_JSON_EXPANSION: usize = 6;

    pub(super) const ENVELOPE_OVERHEAD_BYTES: usize = 16 * 1024;

    pub(super) const MAX_BOUNDED_INPUT_BYTES: usize = 12 * MAX_NEXTEST_FIELD_BYTES
        + MAX_LABEL_BYTES
        + MAX_DIAGNOSTIC_BYTES
        + MAX_CONTEXT_BYTES
        + MAX_FLASH_BYTES
        + 2 * MAX_FLIGHT_CHANNEL_BYTES;
}

#[derive(Debug, Serialize)]
struct NextestContext {
    attempt: Option<String>,
    attempt_id: Option<String>,
    binary_id: Option<String>,
    run_id: Option<String>,
    stress_current: Option<String>,
    stress_total: Option<String>,
    test_global_slot: Option<String>,
    test_group: Option<String>,
    test_group_slot: Option<String>,
    test_name: Option<String>,
    test_threads: Option<String>,
    total_attempts: Option<String>,
}

impl NextestContext {
    fn capture() -> Self {
        Self::from_lookup(|key| env::var(key).ok())
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        let mut read = |key| {
            lookup(key)
                .filter(|value| !value.is_empty())
                .map(|value| bounded_owned(value, consts::MAX_NEXTEST_FIELD_BYTES))
        };
        Self {
            run_id: read(consts::NEXTEST_RUN_ID),
            binary_id: read(consts::NEXTEST_BINARY_ID),
            attempt_id: read(consts::NEXTEST_ATTEMPT_ID),
            attempt: read(consts::NEXTEST_ATTEMPT),
            total_attempts: read(consts::NEXTEST_TOTAL_ATTEMPTS),
            test_name: read(consts::NEXTEST_TEST_NAME),
            stress_current: read(consts::NEXTEST_STRESS_CURRENT),
            stress_total: read(consts::NEXTEST_STRESS_TOTAL),
            test_group: read(consts::NEXTEST_TEST_GROUP),
            test_global_slot: read(consts::NEXTEST_TEST_GLOBAL_SLOT),
            test_group_slot: read(consts::NEXTEST_TEST_GROUP_SLOT),
            test_threads: read(consts::NEXTEST_TEST_THREADS),
        }
    }
}

#[derive(Debug, Serialize)]
struct DumpEnvelope<'a> {
    diagnostic: &'a str,
    label: &'a str,
    schema: &'static str,
    nextest: NextestContext,
    flash: Option<String>,
    context: Value,
    /// Flight-recorder tails captured at dump time: the newest DEBUG events
    /// and `#[kithara::probe]` firings. Every dump kind carries them — the
    /// watchdog/timeout panics that follow a dump are suppressed as
    /// duplicates, so this envelope is the only carrier of that context.
    flight_events: Vec<String>,
    flight_probes: Vec<String>,
    timestamp_ms: u128,
    pid: u32,
}

fn flash_dump(label: &str) -> Option<String> {
    nonempty_flash_dump(flash::hang_dump(label))
}

fn nonempty_flash_dump(dump: String) -> Option<String> {
    if dump.trim().is_empty() {
        None
    } else {
        Some(bounded_owned(dump, consts::MAX_FLASH_BYTES))
    }
}

fn omission_marker(bytes: usize) -> String {
    format!("\n...[kithara omitted_bytes={bytes}]...\n")
}

fn bounded_excerpt(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }

    let marker_budget = omission_marker(value.len()).len();
    debug_assert!(marker_budget < max_bytes);
    let retained = max_bytes - marker_budget;
    let head_target = retained.div_ceil(2);
    let tail_target = retained / 2;

    let mut head_end = head_target;
    while !value.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = value.len() - tail_target;
    while !value.is_char_boundary(tail_start) {
        tail_start += 1;
    }

    let marker = omission_marker(tail_start - head_end);
    let mut excerpt = String::with_capacity(max_bytes);
    excerpt.push_str(&value[..head_end]);
    excerpt.push_str(&marker);
    excerpt.push_str(&value[tail_start..]);
    debug_assert!(excerpt.len() <= max_bytes);
    excerpt
}

fn bounded_owned(value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        value
    } else {
        bounded_excerpt(&value, max_bytes)
    }
}

/// Keep the newest lines whose total length fits the channel budget; a dump
/// wants the moments before the failure, so the oldest lines go first.
fn bounded_tail(mut lines: Vec<String>, max_bytes: usize) -> Vec<String> {
    let mut total = 0usize;
    let keep_from = lines
        .iter()
        .enumerate()
        .rev()
        .take_while(|(_, line)| {
            total += line.len();
            total <= max_bytes
        })
        .map(|(index, _)| index)
        .last()
        .unwrap_or(lines.len());
    lines.drain(..keep_from);
    lines
}

fn bounded_context(payload: String) -> Value {
    if payload.len() > consts::MAX_CONTEXT_BYTES {
        return Value::String(bounded_excerpt(&payload, consts::MAX_CONTEXT_BYTES));
    }

    serde_json::from_str(&payload).unwrap_or(Value::String(payload))
}

fn serialize_envelope(mut envelope: DumpEnvelope<'_>) -> serde_json::Result<Option<String>> {
    let payload = serde_json::to_string(&envelope)?;
    if payload.len() < consts::MAX_ENVELOPE_BYTES {
        return Ok(Some(payload));
    }

    let encoded_bytes = payload.len();
    envelope.flash = None;
    envelope.flight_events = Vec::new();
    envelope.flight_probes = Vec::new();
    envelope.context = Value::String(format!(
        "[kithara context and Flash omitted: encoded_bytes={encoded_bytes}]"
    ));
    let fallback = serde_json::to_string(&envelope)?;
    Ok((fallback.len() < consts::MAX_ENVELOPE_BYTES).then_some(fallback))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn prekill_elapsed(cancelled: &Receiver<()>, timeout: Duration) -> bool {
    matches!(
        cancelled.recv_timeout(timeout),
        Err(RecvTimeoutError::Timeout)
    )
}

/// Stress-only guard that records evidence shortly before the outer runner
/// terminates a test. It never aborts the process.
#[doc(hidden)]
#[must_use]
#[derive(Debug)]
pub struct PreKillGuard {
    cancel: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl PreKillGuard {
    /// Start the pre-kill evidence timer when `KITHARA_HANG_PREKILL_SECS` is a
    /// positive integer. The timer is inert otherwise.
    pub fn new(test_name: &str) -> Self {
        let Some(timeout) = env::var(consts::ENV_PREKILL_SECS)
            .ok()
            .and_then(|value| parse_timeout_secs(&value))
        else {
            return Self {
                cancel: None,
                worker: None,
            };
        };

        let (cancel, cancelled) = mpsc::channel();
        let test_name = test_name.to_owned();
        let error_name = test_name.clone();
        let spawn = Builder::new()
            .name("kithara-prekill".to_owned())
            .spawn(move || {
                if prekill_elapsed(&cancelled, timeout) {
                    let diagnostic = format!(
                        "test `{test_name}` still running after {timeout:?}; outer termination is imminent"
                    );
                    record_test_hang("pre-kill", &diagnostic);
                }
            });

        let worker = match spawn {
            Ok(worker) => worker,
            Err(err) => {
                logging::log_error(&format!(
                    "[kithara_hang_detector] failed to start pre-kill timer for {error_name}: {err}"
                ));
                return Self {
                    cancel: None,
                    worker: None,
                };
            }
        };

        Self {
            cancel: Some(cancel),
            worker: Some(worker),
        }
    }
}

impl Drop for PreKillGuard {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[must_use]
pub(crate) fn resolve_dump_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(env) = env::var_os(consts::ENV_DUMP_DIR) {
        return PathBuf::from(env);
    }
    env::temp_dir()
}

pub(crate) fn write_dump<C: HangDump>(label: &str, ctx: &C, dir: Option<&Path>, diag: &str) {
    const MAX_FALLBACK_LOG_BYTES: usize = 64 * 1024;

    let label = bounded_excerpt(label, consts::MAX_LABEL_BYTES);
    let diagnostic = bounded_excerpt(diag, consts::MAX_DIAGNOSTIC_BYTES);
    let context = bounded_context(ctx.dump_json());
    let ts = now_ms();
    let pid = std::process::id();
    let envelope = DumpEnvelope {
        pid,
        context,
        schema: "kithara.hang.v1",
        label: &label,
        diagnostic: &diagnostic,
        flash: flash_dump(&label),
        timestamp_ms: ts,
        nextest: NextestContext::capture(),
        flight_events: bounded_tail(crate::flight::tail(), consts::MAX_FLIGHT_CHANNEL_BYTES),
        flight_probes: bounded_tail(
            crate::flight::probes_tail(),
            consts::MAX_FLIGHT_CHANNEL_BYTES,
        ),
    };
    let payload = match serialize_envelope(envelope) {
        Ok(Some(payload)) => payload,
        Ok(None) => {
            logging::log_error(&format!(
                "[kithara_hang_detector] bounded hang dump exceeded {MAX_ENVELOPE_BYTES} bytes for {label}",
                MAX_ENVELOPE_BYTES = consts::MAX_ENVELOPE_BYTES
            ));
            return;
        }
        Err(err) => {
            logging::log_error(&format!(
                "[kithara_hang_detector] failed to serialize hang dump for {label}: {err}"
            ));
            return;
        }
    };
    let dir = resolve_dump_dir(dir);
    let dump_id = NEXT_DUMP_ID.fetch_add(1, Ordering::Relaxed);
    let filename = format!(
        "kithara-hang-{label}-{ts}-{pid}-{dump_id}.json",
        label = sanitize_label(&label),
    );
    let file = dir.join(&filename);
    let temp = dir.join(format!(".{filename}.tmp"));
    let write = (|| {
        fs::create_dir_all(&dir)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        output.write_all(payload.as_bytes())?;
        output.sync_data()?;
        drop(output);
        fs::rename(&temp, &file)?;
        Ok::<(), std::io::Error>(())
    })();
    match write {
        Ok(()) => logging::log_error(&format!(
            "[kithara_hang_detector] hang detected: {label} ts_ms={ts} pid={pid} dump={} [{diagnostic}]",
            file.display()
        )),
        Err(err) => {
            let _ = fs::remove_file(&temp);
            let fallback = bounded_excerpt(&payload, MAX_FALLBACK_LOG_BYTES);
            logging::log_error(&format!(
                "[kithara_hang_detector] hang detected: {label} ts_ms={ts} pid={pid} dump-write-failed={err} [{diagnostic}] payload={fallback}"
            ));
        }
    }
}

/// Record attempt-correlated hang evidence for `#[kithara::test]` expansions.
#[doc(hidden)]
pub fn record_test_hang(label: &str, diagnostic: &str) {
    write_dump(label, &NoContext, None, diagnostic);
    // The timeout expansions panic right after this call on the same thread;
    // that panic is already documented by the dump above. (The pre-kill timer
    // thread never panics, so its stray flag is inert.)
    super::panic_dump::suppress_next_panic_dump();
}

#[must_use]
pub(crate) fn env_timeout() -> Option<Duration> {
    static CACHED: OnceLock<Option<Duration>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let value = env::var(consts::ENV_TIMEOUT_SECS).ok()?;
        parse_timeout_secs(&value)
    })
}

#[must_use]
pub(crate) fn parse_timeout_secs(value: &str) -> Option<Duration> {
    let secs = value.parse::<u64>().ok()?;
    (secs > 0).then_some(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nextest_context_captures_attempt_correlation() {
        let nextest = NextestContext::from_lookup(|key| match key {
            consts::NEXTEST_RUN_ID => Some("run-id".to_owned()),
            consts::NEXTEST_BINARY_ID => Some("binary".to_owned()),
            consts::NEXTEST_ATTEMPT_ID => Some("run-id:binary@stress-7$module::test#2".to_owned()),
            consts::NEXTEST_ATTEMPT => Some("2".to_owned()),
            consts::NEXTEST_TOTAL_ATTEMPTS => Some("3".to_owned()),
            consts::NEXTEST_TEST_NAME => Some("module::test".to_owned()),
            consts::NEXTEST_STRESS_CURRENT => Some("7".to_owned()),
            consts::NEXTEST_STRESS_TOTAL => Some("100".to_owned()),
            consts::NEXTEST_TEST_THREADS => Some("12".to_owned()),
            _ => None,
        });

        let value = serde_json::to_value(nextest).expect("serialize nextest context");
        assert_eq!(value["run_id"], "run-id");
        assert_eq!(value["binary_id"], "binary");
        assert_eq!(value["attempt_id"], "run-id:binary@stress-7$module::test#2");
        assert_eq!(value["attempt"], "2");
        assert_eq!(value["total_attempts"], "3");
        assert_eq!(value["test_name"], "module::test");
        assert_eq!(value["stress_current"], "7");
        assert_eq!(value["stress_total"], "100");
        assert_eq!(value["test_threads"], "12");
    }

    #[test]
    fn nextest_context_is_null_without_nextest_environment() {
        let value = serde_json::to_value(NextestContext::from_lookup(|_| None))
            .expect("serialize absent nextest context");

        assert!(
            value.as_object().is_some_and(|fields| {
                !fields.is_empty() && fields.values().all(Value::is_null)
            })
        );
    }

    #[test]
    fn oversized_nextest_metadata_is_visibly_bounded() {
        let raw = format!(
            "run-head{}run-tail",
            "x".repeat(consts::MAX_NEXTEST_FIELD_BYTES)
        );
        let nextest =
            NextestContext::from_lookup(|key| (key == consts::NEXTEST_RUN_ID).then(|| raw.clone()));
        let run_id = nextest.run_id.expect("captured run id");

        assert!(run_id.len() <= consts::MAX_NEXTEST_FIELD_BYTES);
        assert!(run_id.starts_with("run-head"));
        assert!(run_id.ends_with("run-tail"));
        assert!(run_id.contains("[kithara omitted_bytes="));
    }

    #[test]
    fn empty_flash_dump_is_absent() {
        assert_eq!(nonempty_flash_dump(String::new()), None);
        assert_eq!(nonempty_flash_dump(" \n\t".to_owned()), None);
    }

    #[test]
    fn nonempty_flash_dump_is_preserved() {
        let dump = "[flash hang dump] context".to_owned();

        assert_eq!(nonempty_flash_dump(dump.clone()), Some(dump));
    }

    #[test]
    fn bounded_excerpt_preserves_unicode_head_tail_and_marks_omission() {
        let value = format!("HEAD-{}-TAIL", "\u{1f980}".repeat(100));

        let excerpt = bounded_excerpt(&value, 96);

        assert!(excerpt.len() <= 96);
        assert!(excerpt.starts_with("HEAD-"));
        assert!(excerpt.ends_with("-TAIL"));
        assert!(excerpt.contains("[kithara omitted_bytes="));

        let prefix = "\n...[kithara omitted_bytes=";
        let suffix = "]...\n";
        let marker_start = excerpt.find(prefix).expect("omission marker");
        let digits_start = marker_start + prefix.len();
        let digits_end = digits_start
            + excerpt[digits_start..]
                .find(suffix)
                .expect("omission marker suffix");
        let marker_end = digits_end + suffix.len();
        let omitted = excerpt[digits_start..digits_end]
            .parse::<usize>()
            .expect("omitted byte count");
        let retained = marker_start + excerpt.len() - marker_end;
        assert_eq!(omitted, value.len() - retained);
    }

    #[test]
    fn oversized_context_and_flash_keep_bounded_head_and_tail() {
        let context = format!(
            "context-head{}context-tail",
            "x".repeat(consts::MAX_CONTEXT_BYTES)
        );
        let flash = format!(
            "flash-head{}flash-tail",
            "y".repeat(consts::MAX_FLASH_BYTES)
        );

        let context = bounded_context(context);
        let flash = nonempty_flash_dump(flash).expect("non-empty Flash dump");

        let context = context
            .as_str()
            .expect("oversized context degrades to text");
        assert!(context.len() <= consts::MAX_CONTEXT_BYTES);
        assert!(context.starts_with("context-head"));
        assert!(context.ends_with("context-tail"));
        assert!(context.contains("[kithara omitted_bytes="));
        assert!(flash.len() <= consts::MAX_FLASH_BYTES);
        assert!(flash.starts_with("flash-head"));
        assert!(flash.ends_with("flash-tail"));
        assert!(flash.contains("[kithara omitted_bytes="));
    }

    #[test]
    fn prekill_wait_distinguishes_cancel_from_deadline() {
        let (cancel, cancelled) = mpsc::channel();
        cancel.send(()).expect("send cancellation");
        assert!(!prekill_elapsed(&cancelled, Duration::ZERO));

        let (_keepalive, elapsed) = mpsc::channel();
        assert!(prekill_elapsed(&elapsed, Duration::ZERO));
    }

    #[test]
    fn envelope_serializes_flash_evidence() {
        let envelope = DumpEnvelope {
            schema: "kithara.hang.v1",
            label: "pre-kill",
            diagnostic: "still running",
            flash: Some("[flash hang dump] still running".to_owned()),
            timestamp_ms: 1,
            pid: 2,
            nextest: NextestContext::from_lookup(|_| None),
            context: Value::Null,
            flight_events: vec!["DEBUG kithara_test: marker".to_owned()],
            flight_probes: Vec::new(),
        };

        let value = serde_json::to_value(envelope).expect("serialize hang envelope");

        assert_eq!(value["flash"], "[flash hang dump] still running");
        assert_eq!(value["flight_events"][0], "DEBUG kithara_test: marker");
    }

    #[test]
    fn bounded_fields_keep_worst_case_json_below_consumer_limit() {
        let hostile_nextest = "\0".repeat(consts::MAX_NEXTEST_FIELD_BYTES + 1);
        let nextest = NextestContext::from_lookup(|_| Some(hostile_nextest.clone()));
        let label = bounded_excerpt(
            &"\0".repeat(consts::MAX_LABEL_BYTES + 1),
            consts::MAX_LABEL_BYTES,
        );
        let diagnostic = bounded_excerpt(
            &"\0".repeat(consts::MAX_DIAGNOSTIC_BYTES + 1),
            consts::MAX_DIAGNOSTIC_BYTES,
        );
        let envelope = DumpEnvelope {
            nextest,
            schema: "kithara.hang.v1",
            label: &label,
            diagnostic: &diagnostic,
            flash: nonempty_flash_dump("\0".repeat(consts::MAX_FLASH_BYTES + 1)),
            timestamp_ms: u128::MAX,
            pid: u32::MAX,
            context: bounded_context("\0".repeat(consts::MAX_CONTEXT_BYTES + 1)),
            flight_events: bounded_tail(
                vec!["\0".repeat(consts::MAX_FLIGHT_CHANNEL_BYTES + 1)],
                consts::MAX_FLIGHT_CHANNEL_BYTES,
            ),
            flight_probes: bounded_tail(
                vec!["\0".repeat(consts::MAX_FLIGHT_CHANNEL_BYTES / 2); 3],
                consts::MAX_FLIGHT_CHANNEL_BYTES,
            ),
        };

        let payload = serialize_envelope(envelope)
            .expect("serialize bounded envelope")
            .expect("bounded envelope fits consumer limit");

        assert!(payload.len() < consts::MAX_ENVELOPE_BYTES);
    }

    #[test]
    fn size_fallback_preserves_exact_attempt_identity() {
        let attempt_id = "run-id:binary@stress-7$module::test#2";
        let envelope = DumpEnvelope {
            schema: "kithara.hang.v1",
            label: "pre-kill",
            diagnostic: "still running",
            flash: Some("x".repeat(consts::MAX_ENVELOPE_BYTES)),
            timestamp_ms: 1,
            pid: 2,
            nextest: NextestContext::from_lookup(|key| {
                (key == consts::NEXTEST_ATTEMPT_ID).then(|| attempt_id.to_owned())
            }),
            context: Value::Null,
            flight_events: vec!["evicted by the size fallback".to_owned()],
            flight_probes: Vec::new(),
        };

        let payload = serialize_envelope(envelope)
            .expect("serialize fallback envelope")
            .expect("fallback fits consumer limit");
        let value: Value = serde_json::from_str(&payload).expect("parse fallback envelope");

        assert!(payload.len() < consts::MAX_ENVELOPE_BYTES);
        assert_eq!(value["nextest"]["attempt_id"], attempt_id);
        assert!(value["flash"].is_null());
        assert!(
            value["context"]
                .as_str()
                .is_some_and(|context| { context.contains("context and Flash omitted") })
        );
        assert!(
            value["flight_events"].as_array().is_some_and(Vec::is_empty),
            "size fallback must drop the flight tail"
        );
    }

    #[test]
    fn bounded_tail_keeps_newest_lines_within_budget() {
        let lines = vec!["old".repeat(10), "mid".repeat(10), "new".repeat(10)];

        let kept = bounded_tail(lines, 65);

        assert_eq!(kept.len(), 2);
        assert!(kept[0].starts_with("mid"));
        assert!(kept[1].starts_with("new"));
        assert!(bounded_tail(vec!["x".repeat(100)], 10).is_empty());
        let all = bounded_tail(vec!["a".to_owned(), "b".to_owned()], 100);
        assert_eq!(all, vec!["a".to_owned(), "b".to_owned()]);
    }
}
