use std::{
    collections::BTreeMap,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::lock::FileLock;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::report;
use crate::child;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Session {
    pub(super) adb: PathBuf,
    pub(super) serial: String,
    pub(super) package: String,
    pub(super) directory: String,
    pub(super) evidence: PathBuf,
    pub(super) environment: BTreeMap<String, String>,
    pub(super) binaries: BTreeMap<PathBuf, String>,
}

impl Session {
    pub(super) fn adb(&self) -> Command {
        let mut command = Command::new(&self.adb);
        command.args(["-s", &self.serial]);
        command
    }

    pub(super) fn shell(
        &self,
        args: &[&str],
        cancel: Option<&child::Cancel>,
        timeout: Duration,
    ) -> Result<Output> {
        self.shell_raw(&shell_command(args), cancel, timeout)
    }

    pub(super) fn shell_raw(
        &self,
        command: &str,
        cancel: Option<&child::Cancel>,
        timeout: Duration,
    ) -> Result<Output> {
        child::output(self.adb().arg("shell").arg(command), cancel, timeout)
    }

    pub(super) fn control(&self, args: &[&str], cancel: Option<&child::Cancel>) -> Result<Output> {
        let output = self.shell(args, cancel, Duration::from_secs(30))?;
        if !output.status.success() {
            bail!(
                "adb control failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(output)
    }

    pub(super) fn stop(&self) -> Result<()> {
        self.control(&["am", "force-stop", &self.package], None)
            .map(|_| ())
    }
}

pub(crate) fn run(session_path: &Path, binary: &Path, args: &[String]) -> Result<i32> {
    let session: Session = serde_json::from_slice(&fs::read(session_path)?)?;
    let library = session.binaries.get(binary).with_context(|| {
        format!(
            "{} is absent from Android binary inventory",
            binary.display()
        )
    })?;
    let cancel = child::Cancel::install()?;
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(session.evidence.join("device.lock"))?;
    let _lock = FileLock::exclusive(lock)?;
    child::check(Some(&cancel))?;

    let Some(test) = exact_test(args) else {
        if let Some(path) = list_path(&session.evidence, binary, args) {
            let output = listed(&path, &mut || {
                let invocation = invoke(&session, &cancel, binary, library, args)?;
                if invocation.code != 0 {
                    std::io::stderr().lock().write_all(&invocation.response)?;
                    bail!("listing {} on device failed", binary.display());
                }
                invocation
                    .libtest
                    .with_context(|| format!("{} produced no list output", binary.display()))
            })?;
            std::io::stdout().lock().write_all(&output)?;
            return Ok(0);
        }
        let invocation = invoke(&session, &cancel, binary, library, args)?;
        if let Some(libtest) = &invocation.libtest {
            std::io::stdout().lock().write_all(libtest)?;
        }
        if invocation.code != 0 {
            std::io::stderr().lock().write_all(&invocation.response)?;
        }
        return Ok(invocation.code);
    };
    let answer = resolve(
        &report_path(&session.evidence, binary),
        test,
        &mut |args: &[String]| {
            let invocation = invoke(&session, &cancel, binary, library, args)?;
            let Some(libtest) = invocation.libtest else {
                bail!("{} produced no libtest report on device", binary.display());
            };
            Ok(String::from_utf8_lossy(&libtest).into_owned())
        },
    )?;
    std::io::stdout()
        .lock()
        .write_all(answer.output.as_bytes())?;
    Ok(answer.code)
}

/// The shape nextest uses to ask for one test; every other shape is a list
/// request that the device answers directly.
fn exact_test(args: &[String]) -> Option<&str> {
    match args {
        [exact, name, nocapture] if exact == "--exact" && nocapture == "--nocapture" => {
            Some(name.as_str())
        }
        _ => None,
    }
}

fn report_path(evidence: &Path, binary: &Path) -> PathBuf {
    let key = hex::encode(Sha256::digest(binary.display().to_string().as_bytes()));
    evidence.join("reports").join(format!("{key}.json"))
}

/// nextest asks each binary for the same two list shapes in both its list and
/// its run phase, and every ask is a fresh runner process.
fn list_path(evidence: &Path, binary: &Path, args: &[String]) -> Option<PathBuf> {
    if !args.iter().any(|arg| arg == "--list") {
        return None;
    }
    let mut key = Sha256::new();
    key.update(binary.display().to_string().as_bytes());
    for arg in args {
        key.update([0]);
        key.update(arg.as_bytes());
    }
    Some(
        evidence
            .join("lists")
            .join(format!("{}.txt", hex::encode(key.finalize()))),
    )
}

fn listed(path: &Path, invoke: &mut dyn FnMut() -> Result<Vec<u8>>) -> Result<Vec<u8>> {
    if path.is_file() {
        return Ok(fs::read(path)?);
    }
    let output = invoke()?;
    store(path, &output)?;
    Ok(output)
}

/// Publish only complete JSON so cancellation cannot expose a partial cache file.
fn store(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let staged = path.with_extension("staged");
    fs::write(&staged, bytes)?;
    fs::rename(&staged, path)?;
    Ok(())
}

#[derive(Debug)]
struct Answer {
    code: i32,
    output: String,
}

/// nextest spawns one runner process per test, so the verdicts a binary's
/// single batched invocation produced are held on disk under the device lock.
fn resolve(
    path: &Path,
    test: &str,
    invoke: &mut dyn FnMut(&[String]) -> Result<String>,
) -> Result<Answer> {
    let mut report: report::Report = if path.is_file() {
        serde_json::from_slice(&fs::read(path)?)?
    } else {
        report::Report::default()
    };
    while !report.tests.contains_key(test) && !report.finished {
        let reported = report.tests.len();
        report = extend(report, invoke)?;
        store(path, &serde_json::to_vec(&report)?)?;
        if !report.finished && report.tests.len() == reported {
            bail!("device batch made no progress toward a verdict for {test}");
        }
    }
    let Some(outcome) = report.tests.get(test) else {
        bail!("no device invocation reported a verdict for {test}");
    };
    let code = match outcome.verdict {
        report::Verdict::Passed => 0,
        report::Verdict::Failed => 1,
        report::Verdict::Ignored => bail!("{test} is ignored on device but was asked to run"),
    };
    Ok(Answer {
        code,
        output: outcome.output.clone(),
    })
}

/// A device process that died mid-batch leaves the test it was running without
/// a verdict; the remainder runs in a fresh process that skips what is known.
fn extend(
    previous: report::Report,
    invoke: &mut dyn FnMut(&[String]) -> Result<String>,
) -> Result<report::Report> {
    let mut args = vec!["--test-threads=1".to_owned()];
    if !previous.tests.is_empty() {
        args.push("--exact".to_owned());
        for name in previous.tests.keys() {
            args.push("--skip".to_owned());
            args.push(name.clone());
        }
    }
    let mut next = report::parse(&invoke(&args)?);
    if let Some(name) = next.in_flight.take() {
        next.tests.insert(
            name,
            report::Outcome {
                verdict: report::Verdict::Failed,
                output: "the device process died while this test was running".to_owned(),
            },
        );
    }
    let mut tests = previous.tests;
    tests.append(&mut next.tests);
    Ok(report::Report {
        tests,
        in_flight: None,
        finished: next.finished,
    })
}

/// One device shell carries the force-stop, the instrumentation call and the
/// read-back of the device log; the marker separates the two outputs and
/// carries the instrumentation's own exit status.
fn batch_command(package: &str, request: &str, remote: &str, marker: &str) -> String {
    [
        shell_command(&["am", "force-stop", package]),
        shell_command(&[
            "am",
            "instrument",
            "-w",
            "-r",
            "-e",
            "request",
            request,
            &format!("{package}/.NativeInstrumentation"),
        ]),
        format!("echo {marker} $?"),
        shell_command(&["run-as", package, "cat", remote]),
    ]
    .join("; ")
}

fn cleanup_command(package: &str, remote: &str) -> String {
    [
        shell_command(&["am", "force-stop", package]),
        shell_command(&["run-as", package, "rm", "-f", remote]),
    ]
    .join("; ")
}

struct Folded {
    response: Vec<u8>,
    status: i32,
    log: Vec<u8>,
}

fn split_response(output: &[u8], marker: &str) -> Option<Folded> {
    let needle = format!("{marker} ");
    let at = output
        .windows(needle.len())
        .position(|window| window == needle.as_bytes())?;
    let tail = &output[at + needle.len()..];
    let end = tail.iter().position(|byte| *byte == b'\n')?;
    let status = std::str::from_utf8(&tail[..end])
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Folded {
        response: output[..at].to_vec(),
        status,
        log: tail[end + 1..].to_vec(),
    })
}

struct Invocation {
    code: i32,
    libtest: Option<Vec<u8>>,
    response: Vec<u8>,
}

fn invoke(
    session: &Session,
    cancel: &child::Cancel,
    binary: &Path,
    library: &str,
    args: &[String],
) -> Result<Invocation> {
    let id = format!(
        "{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    );
    let evidence = session.evidence.join(&id);
    fs::create_dir(&evidence)?;
    let remote = format!("{}/{}.log", session.directory, id);
    let invocation: Result<Invocation> = (|| {
        let mut arguments = vec![binary.display().to_string()];
        arguments.extend_from_slice(args);
        let mut environment = session.environment.clone();
        for (name, value) in std::env::vars() {
            if (name.starts_with("NEXTEST_")
                && !matches!(name.as_str(), "NEXTEST_TEST_NAME" | "NEXTEST_ATTEMPT_ID"))
                || matches!(name.as_str(), "RUST_BACKTRACE" | "RUST_LOG")
            {
                environment.insert(name, value);
            }
        }
        let environment: Vec<_> = environment
            .iter()
            .flat_map(|(name, value)| [name.as_str(), value.as_str()])
            .collect();
        let request = json!({"library": library, "args": arguments, "log": remote, "directory": session.directory, "environment": environment});
        fs::write(
            evidence.join("request.json"),
            serde_json::to_vec_pretty(&request)?,
        )?;
        let marker = format!("KITHARA_{}", id.replace('-', "_"));
        let output = session.shell_raw(
            &batch_command(&session.package, &request.to_string(), &remote, &marker),
            Some(cancel),
            Duration::from_secs(3600),
        )?;
        let folded = split_response(&output.stdout, &marker);
        let response = folded
            .as_ref()
            .map_or_else(|| output.stdout.clone(), |folded| folded.response.clone());
        let response = [&response[..], &output.stderr[..]].concat();
        fs::write(evidence.join("instrumentation.log"), &response)?;
        let code = exit_code(
            &String::from_utf8_lossy(&response),
            folded.as_ref().is_some_and(|folded| folded.status == 0),
        );
        let libtest = folded
            .filter(|_| output.status.success())
            .map(|folded| folded.log);
        if let Some(libtest) = &libtest {
            fs::write(evidence.join("libtest.log"), libtest)?;
        } else {
            bail!("{} left no device log at {remote}", binary.display());
        }
        Ok(Invocation {
            code,
            libtest,
            response,
        })
    })();
    let invocation = invocation.or_else(|error| recover(session, &evidence, &remote, error));
    let cleanup = session.shell_raw(
        &cleanup_command(&session.package, &remote),
        None,
        Duration::from_secs(30),
    );
    let invocation = invocation?;
    let cleanup = cleanup?;
    if !cleanup.status.success() {
        bail!(
            "adb cleanup failed: {}",
            String::from_utf8_lossy(&cleanup.stderr)
        );
    }
    Ok(invocation)
}

fn recover(
    session: &Session,
    evidence: &Path,
    remote: &str,
    error: anyhow::Error,
) -> Result<Invocation> {
    fs::write(evidence.join("interruption.txt"), format!("{error:#}"))?;
    session.stop()?;
    let output = session.control(&["run-as", &session.package, "cat", remote], None)?;
    let log = String::from_utf8_lossy(&output.stdout);
    if report::parse(&log).in_flight.is_none() {
        return Err(error);
    }
    fs::write(evidence.join("libtest.log"), &output.stdout)?;
    Ok(Invocation {
        code: 1,
        libtest: Some(output.stdout),
        response: output.stderr,
    })
}

fn exit_code(output: &str, shell_success: bool) -> i32 {
    let code = output.lines().find_map(|line| {
        line.strip_prefix("INSTRUMENTATION_RESULT: rust_exit_code=")
            .and_then(|code| code.trim().parse::<i32>().ok())
    });
    if shell_success
        && code == Some(0)
        && output
            .lines()
            .any(|line| line.trim() == "INSTRUMENTATION_CODE: -1")
    {
        0
    } else {
        code.filter(|code| *code > 0).unwrap_or(1)
    }
}

fn shell_command(args: &[&str]) -> String {
    args.iter()
        .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[cfg(unix)]
    #[test]
    fn an_interrupted_batch_keeps_completed_verdicts_and_fails_its_running_test() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        let trace = dir.path().join("commands");
        let log = dir.path().join("remote.log");
        fs::write(&log, INTERRUPTED).unwrap();
        fs::write(
            &adb,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$4\" >> {}\ncase \"$4\" in *\"'cat'\"*) {};; esac\n",
                shell_command(&[trace.to_str().unwrap()]),
                shell_command(&["cat", log.to_str().unwrap()]),
            ),
        )
        .unwrap();
        fs::set_permissions(&adb, fs::Permissions::from_mode(0o755)).unwrap();
        let session = Session {
            adb,
            serial: "test-device".into(),
            package: "test.package".into(),
            directory: String::new(),
            evidence: dir.path().to_owned(),
            environment: BTreeMap::new(),
            binaries: BTreeMap::new(),
        };
        let recovered = recover(
            &session,
            dir.path(),
            "/remote.log",
            anyhow::anyhow!("batch deadline"),
        )
        .unwrap();
        let commands = fs::read_to_string(trace).unwrap();
        assert!(commands.find("force-stop").unwrap() < commands.find("cat").unwrap());
        let output = String::from_utf8(recovered.libtest.unwrap()).unwrap();
        let path = dir.path().join("report.json");
        let calls = Cell::new(0);
        assert_eq!(
            resolved(&path, "aaa_quick", &calls, &output).unwrap().code,
            0
        );
        assert_eq!(
            resolved(&path, "bbb_slow", &calls, &output).unwrap().code,
            1
        );
        assert_eq!(calls.get(), 1);
        assert!(
            fs::read_to_string(dir.path().join("interruption.txt"))
                .unwrap()
                .contains("batch deadline")
        );
    }

    #[test]
    fn a_crashed_libtest_is_not_adb_shell_success() {
        assert_eq!(
            exit_code(
                "INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0\n",
                true
            ),
            1
        );
        assert_eq!(
            exit_code(
                "INSTRUMENTATION_RESULT: rust_exit_code=0\nINSTRUMENTATION_CODE: -1\n",
                true
            ),
            0
        );
        assert_eq!(
            exit_code(
                "INSTRUMENTATION_RESULT: rust_exit_code=118\nINSTRUMENTATION_CODE: 0\n",
                true
            ),
            118
        );
        assert_eq!(
            exit_code("INSTRUMENTATION_RESULT: rust_exit_code=0\n", true),
            1
        );
    }

    use fixture::{BATCH, INTERRUPTED, REMAINDER};

    mod fixture {
        pub(super) const BATCH: &str = include_str!("../../../tests/fixtures/libtest-batch.txt");
        pub(super) const INTERRUPTED: &str =
            include_str!("../../../tests/fixtures/libtest-interrupted.txt");
        pub(super) const REMAINDER: &str =
            include_str!("../../../tests/fixtures/libtest-remainder.txt");
    }

    fn resolved(path: &Path, test: &str, calls: &Cell<usize>, batch: &str) -> Result<Answer> {
        resolve(path, test, &mut |args: &[String]| {
            calls.set(calls.get() + 1);
            assert_eq!(args, ["--test-threads=1"]);
            Ok(batch.to_owned())
        })
    }

    #[test]
    fn a_batched_binary_answers_its_later_tests_without_the_device() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reports/binary.json");
        let calls = Cell::new(0);

        let first = resolved(&path, "alpha_passes", &calls, BATCH).unwrap();
        let second = resolved(&path, "beta_fails", &calls, BATCH).unwrap();
        let third = resolved(&path, "delta_passes", &calls, BATCH).unwrap();

        assert_eq!(calls.get(), 1);
        assert_eq!(first.code, 0);
        assert_eq!(second.code, 1);
        assert_eq!(third.code, 0);
        assert!(second.output.contains("beta went wrong"));
    }

    #[test]
    fn a_test_no_invocation_reported_is_named_rather_than_answered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reports/binary.json");
        let calls = Cell::new(0);

        let error = resolved(&path, "epsilon_absent", &calls, BATCH).unwrap_err();

        assert!(
            error.to_string().contains("epsilon_absent"),
            "{error} does not name the test"
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_batch_without_test_progress_is_not_restarted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reports/binary.json");
        let calls = Cell::new(0);
        let error = resolved(&path, "missing", &calls, "startup failed").unwrap_err();

        assert!(error.to_string().contains("missing"));
        assert_eq!(calls.get(), 1);
    }

    /// libtest exits 101 on any failure, so the instrumentation reports a
    /// crashed process for every binary holding one.
    #[test]
    fn a_crashed_batch_still_reports_its_passing_tests() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reports/binary.json");
        let calls = Cell::new(0);

        assert_eq!(
            exit_code(
                "INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0\n",
                true
            ),
            1
        );
        let passed = resolved(&path, "alpha_passes", &calls, BATCH).unwrap();

        assert_eq!(passed.code, 0);
        assert_eq!(passed.output, "");
    }

    #[test]
    fn an_interrupted_binary_fails_the_test_in_flight_and_runs_the_remainder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reports/binary.json");
        let requests = RefCell::new(Vec::new());
        let mut device = |args: &[String]| {
            requests.borrow_mut().push(args.to_vec());
            Ok(if requests.borrow().len() == 1 {
                INTERRUPTED.to_owned()
            } else {
                REMAINDER.to_owned()
            })
        };

        let remainder = resolve(&path, "ccc_quick", &mut device).unwrap();
        let flight = resolve(&path, "bbb_slow", &mut device).unwrap();

        assert_eq!(flight.code, 1);
        assert_eq!(remainder.code, 0);
        assert_eq!(requests.borrow()[0], ["--test-threads=1"]);
        assert_eq!(
            requests.borrow()[1],
            [
                "--test-threads=1",
                "--exact",
                "--skip",
                "aaa_quick",
                "--skip",
                "bbb_slow"
            ]
        );
    }

    #[test]
    fn a_binarys_list_shapes_cost_one_device_invocation_each() {
        let dir = tempfile::tempdir().unwrap();
        let binary = Path::new("/cargo/deps/queue-83aa2c917ee17390");
        let terse: Vec<String> = ["--list", "--format", "terse"].map(str::to_owned).into();
        let ignored: Vec<String> = ["--list", "--format", "terse", "--ignored"]
            .map(str::to_owned)
            .into();
        let calls = Cell::new(0);
        let ask = |args: &[String]| {
            let path = list_path(dir.path(), binary, args).unwrap();
            listed(&path, &mut || {
                calls.set(calls.get() + 1);
                Ok(args.join(" ").into_bytes())
            })
        };

        for _ in 0..2 {
            assert_eq!(ask(&terse).unwrap(), terse.join(" ").into_bytes());
            assert_eq!(ask(&ignored).unwrap(), ignored.join(" ").into_bytes());
        }

        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn two_binaries_that_share_a_file_stem_do_not_share_a_list() {
        let first = Path::new("/cargo/deps/queue-83aa2c917ee17390");
        let second = Path::new("/other/deps/queue-83aa2c917ee17390");
        let args: Vec<String> = ["--list", "--format", "terse"].map(str::to_owned).into();

        assert_ne!(
            list_path(Path::new("/evidence"), first, &args),
            list_path(Path::new("/evidence"), second, &args)
        );
        assert_eq!(list_path(Path::new("/evidence"), first, &[]), None);
    }

    #[test]
    fn one_device_shell_brackets_the_instrumentation_call() {
        let command = batch_command(
            "com.kithara.nativetest",
            "{\"library\":\"libx.so\"}",
            "/data/run/x.log",
            "KITHARA_1_2",
        );

        assert_eq!(command.matches("; ").count(), 3);
        assert_eq!(command.matches("'am' 'instrument'").count(), 1);
        assert!(command.starts_with("'am' 'force-stop' 'com.kithara.nativetest'; "));
        assert!(command.contains("; echo KITHARA_1_2 $?; "));
        assert!(command.ends_with("'run-as' 'com.kithara.nativetest' 'cat' '/data/run/x.log'"));
        assert_eq!(
            cleanup_command("com.kithara.nativetest", "/data/run/x.log")
                .matches("; ")
                .count(),
            1
        );
    }

    #[test]
    fn the_marker_separates_the_device_log_from_the_instrumentation_output() {
        let folded = split_response(
            b"INSTRUMENTATION_RESULT: rust_exit_code=0\nINSTRUMENTATION_CODE: -1\nKITHARA_1_2 0\n\nrunning 1 test\ntest a ... ok\n",
            "KITHARA_1_2",
        )
        .unwrap();

        assert_eq!(folded.status, 0);
        assert_eq!(folded.log, b"\nrunning 1 test\ntest a ... ok\n");
        assert!(!folded.response.ends_with(b"KITHARA_1_2 0\n"));
        assert_eq!(
            exit_code(
                &String::from_utf8_lossy(&folded.response),
                folded.status == 0
            ),
            0
        );
        assert!(split_response(b"INSTRUMENTATION_CODE: 0\n", "KITHARA_1_2").is_none());
    }

    #[test]
    fn instrumentation_arguments_remain_literal_shell_arguments() {
        assert_eq!(
            shell_command(&["am", "a'b", "$(touch file)"]),
            "'am' 'a'\\''b' '$(touch file)'"
        );
    }

    #[test]
    fn a_cache_file_appears_only_once_it_is_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reports/binary.json");

        store(&path, b"first").unwrap();
        store(&path, b"second").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"second");
        let left: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["binary.json"]);
    }
}
