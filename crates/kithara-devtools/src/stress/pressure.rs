use std::{
    any::Any,
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    thread::{Builder, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Error, Result, anyhow, ensure};
use serde::Serialize;

use crate::consts;

pub(super) struct Sampler {
    worker: JoinHandle<WorkerOutcome>,
    path: PathBuf,
    cancel: SyncSender<()>,
}

#[derive(Debug)]
struct SampleContext {
    cgroup_path: Option<PathBuf>,
    proc_path: PathBuf,
    cgroup_scope: String,
}

#[derive(Debug)]
struct Observation {
    metrics: BTreeMap<String, String>,
    load1: Option<f64>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum Marker {
    Start,
    Sample,
    End,
}

#[derive(Serialize)]
struct Scope<'a> {
    cgroup_v2: &'a str,
    proc_pressure: &'static str,
}

#[derive(Serialize)]
struct SampleRecord<'a> {
    metrics: &'a BTreeMap<String, String>,
    schema: &'static str,
    marker: Marker,
    cgroup_path: Option<&'a Path>,
    load1: Option<f64>,
    scope: Scope<'a>,
    timestamp_ms: u64,
}

#[derive(Serialize)]
struct EndRecord {
    schema: &'static str,
    metrics: BTreeMap<String, String>,
    marker: Marker,
    load1: Option<f64>,
    primary_exit_code: Option<i32>,
    sampler_healthy: bool,
    timestamp_ms: u64,
}

struct WorkerOutcome {
    error: Option<Error>,
    last_timestamp_ms: u64,
}

impl Sampler {
    pub(super) fn finish(self, primary_exit_code: Option<i32>) -> Result<bool> {
        let Self {
            cancel,
            path,
            worker,
        } = self;
        let cancel_error = cancel
            .send(())
            .err()
            .map(|_| anyhow!("cancel pressure sampler worker"));
        drop(cancel);

        let outcome = match worker.join() {
            Ok(outcome) => outcome,
            Err(payload) => {
                let failure = anyhow!(
                    "pressure sampler worker panicked outside its guard: {}",
                    panic_message(payload.as_ref())
                );
                return match append_unhealthy_end(&path, primary_exit_code) {
                    Ok(()) => Err(failure),
                    Err(end_error) => Err(failure.context(format!(
                        "append unhealthy pressure end marker also failed: {end_error:#}"
                    ))),
                };
            }
        };
        let mut failure = outcome.error;
        if let Some(error) = cancel_error {
            add_failure(&mut failure, error);
        }
        let healthy = failure.is_none();
        let end = append_end(&path, outcome.last_timestamp_ms, healthy, primary_exit_code);

        match (failure, end) {
            (None, Ok(())) => Ok(true),
            (Some(error), Ok(())) => Err(error.context("pressure sampler worker failed")),
            (None, Err(error)) => Err(error),
            (Some(error), Err(end_error)) => Err(error.context(format!(
                "append unhealthy pressure end marker also failed: {end_error:#}"
            ))),
        }
    }

    pub(super) fn start(
        path: &Path,
        cgroup_path: Option<&Path>,
        cgroup_scope: &str,
    ) -> Result<Self> {
        Self::start_with_context(
            path,
            SampleContext {
                cgroup_path: cgroup_path.map(Path::to_path_buf),
                cgroup_scope: cgroup_scope.to_owned(),
                proc_path: PathBuf::from("/proc"),
            },
        )
    }

    fn start_with_context(path: &Path, context: SampleContext) -> Result<Self> {
        let path = path.to_path_buf();
        let mut writer = BufWriter::new(
            File::create(&path)
                .with_context(|| format!("create pressure artifact {}", path.display()))?,
        );
        let startup = (|| {
            let observation = collect(&context)?;
            let start_timestamp_ms = wall_timestamp_ms()?;
            write_sample(
                &mut writer,
                Marker::Start,
                start_timestamp_ms,
                &observation,
                &context,
            )?;
            Ok(start_timestamp_ms)
        })();
        drop(writer);
        let start_timestamp_ms = match startup {
            Ok(timestamp) => timestamp,
            Err(error) => return fail_start(&path, error),
        };

        let (cancel, receiver) = mpsc::sync_channel(1);
        let worker_path = path.clone();
        let worker = match Builder::new()
            .name("stress-pressure".to_owned())
            .spawn(move || worker_entry(&worker_path, &receiver, start_timestamp_ms, &context))
        {
            Ok(worker) => worker,
            Err(spawn_error) => {
                let error = Error::new(spawn_error).context("start pressure sampler worker");
                return match append_end(&path, start_timestamp_ms, false, None) {
                    Ok(()) => Err(error),
                    Err(end_error) => Err(error.context(format!(
                        "append unhealthy pressure end marker after worker start failure: {end_error:#}"
                    ))),
                };
            }
        };

        Ok(Self {
            worker,
            path,
            cancel,
        })
    }
}

fn fail_start<T>(path: &Path, error: Error) -> Result<T> {
    let end =
        wall_timestamp_ms().and_then(|timestamp_ms| append_end(path, timestamp_ms, false, None));
    match end {
        Ok(()) => Err(error.context("initialize pressure sampler")),
        Err(end_error) => Err(error.context(format!(
            "initialize pressure sampler; append unhealthy end marker also failed: {end_error:#}"
        ))),
    }
}

fn worker_entry(
    path: &Path,
    receiver: &Receiver<()>,
    start_timestamp_ms: u64,
    context: &SampleContext,
) -> WorkerOutcome {
    let mut last_timestamp_ms = start_timestamp_ms;
    let file = match OpenOptions::new().append(true).open(path) {
        Ok(file) => file,
        Err(error) => {
            return WorkerOutcome {
                last_timestamp_ms,
                error: Some(Error::new(error).context(format!(
                    "open pressure artifact for sampling {}",
                    path.display()
                ))),
            };
        }
    };
    let mut writer = BufWriter::new(file);
    let run = panic::catch_unwind(AssertUnwindSafe(|| {
        worker_loop(&mut writer, receiver, &mut last_timestamp_ms, context)
    }));
    let mut failure = match run {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(payload) => Some(anyhow!(
            "pressure sampler worker panicked: {}",
            panic_message(payload.as_ref())
        )),
    };
    if let Err(error) = writer
        .flush()
        .with_context(|| format!("flush pressure artifact {}", path.display()))
    {
        add_failure(&mut failure, error);
    }
    WorkerOutcome {
        last_timestamp_ms,
        error: failure,
    }
}

fn worker_loop(
    writer: &mut BufWriter<File>,
    receiver: &Receiver<()>,
    last_timestamp_ms: &mut u64,
    context: &SampleContext,
) -> Result<()> {
    loop {
        match receiver.recv_timeout(consts::SAMPLE_INTERVAL) {
            Ok(()) => return Ok(()),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(anyhow!(
                    "pressure sampler cancellation channel disconnected"
                ));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        let observation = collect(context)?;
        let timestamp_ms = next_timestamp_ms(*last_timestamp_ms, wall_timestamp_ms()?)?;
        write_sample(writer, Marker::Sample, timestamp_ms, &observation, context)?;
        *last_timestamp_ms = timestamp_ms;
    }
}

fn collect(context: &SampleContext) -> Result<Observation> {
    let metadata = fs::metadata(&context.proc_path).with_context(|| {
        format!(
            "inspect pressure sampler proc root {}",
            context.proc_path.display()
        )
    })?;
    ensure!(
        metadata.is_dir(),
        "pressure sampler proc root is not a directory: {}",
        context.proc_path.display()
    );

    let mut metrics = BTreeMap::new();
    let loadavg = read_available(&context.proc_path.join("loadavg"))?;
    let load1 = parse_load1(loadavg.as_deref());
    if let Some(loadavg) = loadavg {
        metrics.insert("proc.loadavg".to_owned(), loadavg);
    }
    for &(key, relative) in consts::PROC_METRICS {
        insert_available(&mut metrics, key, &context.proc_path.join(relative))?;
    }
    if let Some(stat) = read_available(&context.proc_path.join("stat"))? {
        metrics.insert("proc.stat".to_owned(), first_lines(&stat, 5));
    }
    if let Some(meminfo) = read_available(&context.proc_path.join("meminfo"))? {
        collect_meminfo(&mut metrics, &meminfo);
    }
    if let Some(cgroup_path) = &context.cgroup_path {
        for &(key, relative) in consts::CGROUP_METRICS {
            insert_available(&mut metrics, key, &cgroup_path.join(relative))?;
        }
    }

    Ok(Observation { metrics, load1 })
}

fn insert_available(metrics: &mut BTreeMap<String, String>, key: &str, path: &Path) -> Result<()> {
    if let Some(value) = read_available(path)? {
        metrics.insert(key.to_owned(), value);
    }
    Ok(())
}

fn read_available(path: &Path) -> Result<Option<String>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(
                Error::new(error).context(format!("open pressure metric {}", path.display()))
            );
        }
    };
    let mut bytes = Vec::with_capacity(consts::MAX_METRIC_BYTES.saturating_add(1));
    file.take(consts::MAX_METRIC_READ_BYTES)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read pressure metric {}", path.display()))?;
    ensure!(
        bytes.len() <= consts::MAX_METRIC_BYTES,
        "pressure metric exceeds {} bytes: {}",
        consts::MAX_METRIC_BYTES,
        path.display()
    );
    let value = String::from_utf8(bytes)
        .with_context(|| format!("pressure metric is not UTF-8: {}", path.display()))?;
    Ok(Some(value.trim().to_owned()))
}

fn parse_load1(loadavg: Option<&str>) -> Option<f64> {
    loadavg
        .and_then(|loadavg| loadavg.split_whitespace().next())
        .and_then(|load| load.parse::<f64>().ok())
        .filter(|load| load.is_finite() && *load >= 0.0)
}

fn first_lines(value: &str, count: usize) -> String {
    value.lines().take(count).collect::<Vec<_>>().join("\n")
}

fn collect_meminfo(metrics: &mut BTreeMap<String, String>, meminfo: &str) {
    for line in meminfo.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if consts::MEMINFO_FIELDS.contains(&name) {
            metrics.insert(format!("proc.meminfo.{name}"), value.trim().to_owned());
        }
    }
}

fn write_sample(
    writer: &mut BufWriter<File>,
    marker: Marker,
    timestamp_ms: u64,
    observation: &Observation,
    context: &SampleContext,
) -> Result<()> {
    let record = SampleRecord {
        marker,
        timestamp_ms,
        schema: consts::SCHEMA,
        load1: observation.load1,
        scope: Scope {
            proc_pressure: "host",
            cgroup_v2: &context.cgroup_scope,
        },
        cgroup_path: context.cgroup_path.as_deref(),
        metrics: &observation.metrics,
    };
    write_record(writer, &record)
}

fn append_end(
    path: &Path,
    previous_timestamp_ms: u64,
    sampler_healthy: bool,
    exit_code: Option<i32>,
) -> Result<()> {
    let timestamp_ms = next_timestamp_ms(previous_timestamp_ms, wall_timestamp_ms()?)?;
    let needs_delimiter = needs_record_delimiter(path)?;
    let file = OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("open pressure artifact for finalization {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    if needs_delimiter {
        writer
            .write_all(b"\n")
            .context("terminate incomplete pressure record")?;
    }
    let record = EndRecord {
        timestamp_ms,
        sampler_healthy,
        schema: consts::SCHEMA,
        marker: Marker::End,
        load1: None,
        metrics: BTreeMap::new(),
        primary_exit_code: exit_code,
    };
    write_record(&mut writer, &record)
}

fn needs_record_delimiter(path: &Path) -> Result<bool> {
    let mut file = File::open(path)
        .with_context(|| format!("inspect pressure artifact delimiter {}", path.display()))?;
    if file
        .metadata()
        .with_context(|| format!("inspect pressure artifact length {}", path.display()))?
        .len()
        == 0
    {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))
        .with_context(|| format!("seek pressure artifact delimiter {}", path.display()))?;
    let mut final_byte = [0_u8; 1];
    file.read_exact(&mut final_byte)
        .with_context(|| format!("read pressure artifact delimiter {}", path.display()))?;
    Ok(final_byte != *b"\n")
}

fn write_record(writer: &mut BufWriter<File>, record: &impl Serialize) -> Result<()> {
    let encoded = serde_json::to_vec(record).context("serialize pressure record")?;
    ensure!(
        encoded.len() <= consts::MAX_RECORD_BYTES,
        "pressure record exceeds {} bytes",
        consts::MAX_RECORD_BYTES,
    );
    writer
        .write_all(&encoded)
        .context("write pressure record")?;
    writer
        .write_all(b"\n")
        .context("write pressure record delimiter")?;
    writer.flush().context("flush pressure record")
}

fn wall_timestamp_ms() -> Result<u64> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("pressure timestamp predates the Unix epoch")?
        .as_millis();
    u64::try_from(milliseconds).context("pressure timestamp exceeds u64 milliseconds")
}

fn next_timestamp_ms(previous: u64, observed: u64) -> Result<u64> {
    let minimum = previous
        .checked_add(1)
        .ok_or_else(|| anyhow!("pressure timestamp overflow after {previous}"))?;
    Ok(observed.max(minimum))
}

fn append_unhealthy_end(path: &Path, primary_exit_code: Option<i32>) -> Result<()> {
    let mut file = File::open(path)
        .with_context(|| format!("open pressure artifact tail {}", path.display()))?;
    let file_len = file
        .metadata()
        .with_context(|| format!("inspect pressure artifact tail {}", path.display()))?
        .len();
    let tail_bytes = file_len.min(consts::MAX_RECOVERY_BYTES);
    let offset = i64::try_from(tail_bytes).context("pressure artifact tail offset exceeds i64")?;
    file.seek(SeekFrom::End(-offset))
        .with_context(|| format!("seek pressure artifact tail {}", path.display()))?;
    let tail_len = usize::try_from(tail_bytes).context("pressure artifact tail exceeds usize")?;
    let mut tail = Vec::with_capacity(tail_len);
    file.take(tail_bytes.saturating_add(1))
        .read_to_end(&mut tail)
        .with_context(|| format!("read pressure artifact tail {}", path.display()))?;
    let previous_timestamp_ms = tail
        .split(|byte| *byte == b'\n')
        .rev()
        .find_map(find_timestamp_ms)
        .ok_or_else(|| {
            anyhow!(
                "pressure artifact has no recoverable final timestamp: {}",
                path.display()
            )
        })?;
    append_end(path, previous_timestamp_ms, false, primary_exit_code)
}

fn find_timestamp_ms(line: &[u8]) -> Option<u64> {
    const FIELD: &[u8] = b"\"timestamp_ms\":";
    line.windows(FIELD.len())
        .position(|window| window == FIELD)
        .and_then(|index| line.get(index.saturating_add(FIELD.len())..))
        .and_then(|tail| {
            let digits = tail.iter().take_while(|byte| byte.is_ascii_digit()).count();
            (digits > 0).then(|| &tail[..digits])
        })
        .and_then(|digits| std::str::from_utf8(digits).ok())
        .and_then(|digits| digits.parse::<u64>().ok())
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

fn add_failure(failure: &mut Option<Error>, error: Error) {
    *failure = Some(match failure.take() {
        Some(primary) => primary.context(format!("additional pressure sampler failure: {error:#}")),
        None => error,
    });
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn start_record_matches_pressure_schema() {
        let temp = tempdir().expect("tempdir");
        let proc_path = temp.path().join("proc");
        let cgroup_path = temp.path().join("cgroup");
        fs::create_dir_all(proc_path.join("pressure")).expect("create proc fixture");
        fs::create_dir_all(&cgroup_path).expect("create cgroup fixture");
        fs::write(proc_path.join("loadavg"), "1.25 0.50 0.10 1/2 3\n").expect("write load fixture");
        fs::write(
            proc_path.join("pressure/cpu"),
            "some avg10=2.00 avg60=1.00 avg300=0.50 total=4\n",
        )
        .expect("write pressure fixture");
        let path = temp.path().join("pressure.jsonl");
        let context = SampleContext {
            proc_path,
            cgroup_path: Some(cgroup_path.clone()),
            cgroup_scope: "current_process_cgroup".to_owned(),
        };

        let sampler = Sampler::start_with_context(&path, context).expect("start sampler");
        sampler.finish(Some(0)).expect("finish sampler");
        let contents = fs::read_to_string(path).expect("read pressure artifact");
        let first = contents.lines().next().expect("start record");
        let record = serde_json::from_str::<Value>(first).expect("parse start record");

        assert_eq!(record["schema"], consts::SCHEMA);
        assert_eq!(record["marker"], "start");
        assert_eq!(record["load1"], 1.25);
        assert_eq!(record["scope"]["proc_pressure"], "host");
        assert_eq!(record["scope"]["cgroup_v2"], "current_process_cgroup");
        assert_eq!(record["cgroup_path"].as_str(), cgroup_path.to_str());
        assert_eq!(record["metrics"]["proc.loadavg"], "1.25 0.50 0.10 1/2 3");
        assert!(record.get("sampler_healthy").is_none());
        assert!(record.get("primary_exit_code").is_none());
    }

    #[test]
    fn timestamps_are_strict_even_when_wall_clock_does_not_advance() {
        assert_eq!(next_timestamp_ms(1_000, 1_000).expect("same clock"), 1_001);
        assert_eq!(next_timestamp_ms(1_000, 900).expect("older clock"), 1_001);
        assert_eq!(next_timestamp_ms(1_000, 2_000).expect("newer clock"), 2_000);
        assert!(next_timestamp_ms(u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn clean_short_run_contains_start_and_healthy_end() {
        let temp = tempdir().expect("tempdir");
        let proc_path = temp.path().join("proc");
        fs::create_dir(&proc_path).expect("create proc fixture");
        let path = temp.path().join("pressure.jsonl");
        let context = SampleContext {
            proc_path,
            cgroup_path: None,
            cgroup_scope: "unavailable".to_owned(),
        };

        let sampler = Sampler::start_with_context(&path, context).expect("start sampler");
        assert!(sampler.finish(Some(17)).expect("finish sampler"));
        let contents = fs::read_to_string(path).expect("read pressure artifact");
        let records = contents
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("parse pressure record"))
            .collect::<Vec<_>>();
        let first = records.first().expect("start record");
        let last = records.last().expect("end record");

        assert_eq!(first["marker"], "start");
        assert_eq!(first["cgroup_path"], Value::Null);
        assert_eq!(last["marker"], "end");
        assert_eq!(last["load1"], Value::Null);
        assert_eq!(last["metrics"], serde_json::json!({}));
        assert_eq!(last["sampler_healthy"], true);
        assert_eq!(last["primary_exit_code"], 17);
        assert!(
            last["timestamp_ms"].as_u64().expect("end timestamp")
                > first["timestamp_ms"].as_u64().expect("start timestamp")
        );
    }

    #[test]
    fn failed_initial_collection_appends_unhealthy_end() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("pressure.jsonl");
        let context = SampleContext {
            cgroup_path: None,
            cgroup_scope: "unavailable".to_owned(),
            proc_path: temp.path().join("missing-proc"),
        };

        let error = match Sampler::start_with_context(&path, context) {
            Ok(_) => panic!("missing proc root must fail startup"),
            Err(error) => error,
        };
        let contents = fs::read_to_string(path).expect("read pressure artifact");
        let record = serde_json::from_str::<Value>(contents.trim()).expect("parse end record");

        assert!(error.to_string().contains("initialize pressure sampler"));
        assert_eq!(record["marker"], "end");
        assert_eq!(record["sampler_healthy"], false);
        assert_eq!(record["primary_exit_code"], Value::Null);
    }

    #[test]
    fn metric_collection_parses_load_meminfo_and_bounded_sources() {
        let temp = tempdir().expect("tempdir");
        let proc_path = temp.path().join("proc");
        let cgroup_path = temp.path().join("cgroup");
        fs::create_dir_all(proc_path.join("pressure")).expect("create proc fixture");
        fs::create_dir_all(&cgroup_path).expect("create cgroup fixture");
        fs::write(proc_path.join("loadavg"), "3.50 2.00 1.00 2/40 7\n")
            .expect("write load fixture");
        fs::write(
            proc_path.join("meminfo"),
            "MemTotal: 100 kB\nMemAvailable: 75 kB\nIgnored: 9 kB\n",
        )
        .expect("write meminfo fixture");
        fs::write(
            proc_path.join("stat"),
            "cpu one\ncpu0 two\nintr three\nctxt four\nbtime five\nprocesses six\n",
        )
        .expect("write stat fixture");
        fs::write(
            proc_path.join("pressure/memory"),
            "some avg10=0.25 avg60=0.10 avg300=0.01 total=8\n",
        )
        .expect("write pressure fixture");
        fs::write(cgroup_path.join("memory.current"), "4096\n").expect("write cgroup fixture");
        let context = SampleContext {
            proc_path,
            cgroup_path: Some(cgroup_path),
            cgroup_scope: "current_process_cgroup".to_owned(),
        };

        let observation = collect(&context).expect("collect pressure metrics");

        assert_eq!(observation.load1, Some(3.5));
        assert_eq!(
            observation
                .metrics
                .get("proc.meminfo.MemTotal")
                .map(String::as_str),
            Some("100 kB")
        );
        assert_eq!(
            observation.metrics.get("proc.stat").map(String::as_str),
            Some("cpu one\ncpu0 two\nintr three\nctxt four\nbtime five")
        );
        assert_eq!(
            observation
                .metrics
                .get("cgroup.memory.current")
                .map(String::as_str),
            Some("4096")
        );
        assert!(!observation.metrics.contains_key("proc.meminfo.Ignored"));
    }
}
