use std::{
    ffi::OsStr,
    fs::File,
    io,
    path::Path,
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const SECRET_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "CARGO_REGISTRY_TOKEN",
    "CI_JOB_TOKEN",
    "CODECOV_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GITLAB_TOKEN",
    "NPM_TOKEN",
    "OPENAI_API_KEY",
];

#[derive(Debug)]
pub(crate) enum ProcessError {
    MissingExecutable(String),
    Io(io::Error),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingExecutable(program) => {
                write!(
                    formatter,
                    "Quality Lab executable `{program}` is not installed"
                )
            }
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MissingExecutable(_) => None,
            Self::Io(error) => Some(error),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProcessOutcome {
    pub(crate) duration: Duration,
    pub(crate) status: ExitStatus,
    pub(crate) timed_out: bool,
}

pub(crate) struct ProcessRequest<'a> {
    pub(crate) cwd: &'a Path,
    pub(crate) stderr_path: &'a Path,
    pub(crate) stdout_path: &'a Path,
    pub(crate) args: &'a [String],
    pub(crate) program: &'a str,
    pub(crate) timeout: Duration,
}

pub(crate) fn run_process(request: &ProcessRequest<'_>) -> Result<ProcessOutcome, ProcessError> {
    run_process_with_env(request, &[])
}

pub(crate) fn run_process_with_env(
    request: &ProcessRequest<'_>,
    environment: &[(&str, &OsStr)],
) -> Result<ProcessOutcome, ProcessError> {
    let stdout = File::create(request.stdout_path).map_err(ProcessError::Io)?;
    let stderr = File::create(request.stderr_path).map_err(ProcessError::Io)?;
    let mut command = Command::new(request.program);
    command
        .args(request.args)
        .current_dir(request.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    remove_secret_environment(&mut command);
    command.envs(environment.iter().copied());
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ProcessError::MissingExecutable(request.program.to_owned())
        } else {
            ProcessError::Io(error)
        }
    })?;

    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(ProcessError::Io)? {
            return Ok(ProcessOutcome {
                status,
                duration: start.elapsed(),
                timed_out: false,
            });
        }
        if start.elapsed() >= request.timeout {
            child.kill().map_err(ProcessError::Io)?;
            let status = child.wait().map_err(ProcessError::Io)?;
            return Ok(ProcessOutcome {
                status,
                duration: start.elapsed(),
                timed_out: true,
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn remove_secret_environment(command: &mut Command) {
    for key in SECRET_ENV_KEYS {
        command.env_remove(key);
    }
}

#[cfg(test)]
mod tests {
    use std::{env, ffi::OsStr, fs, io, thread, time::Duration};

    use tempfile::tempdir;

    use super::*;

    /// Budget for the tests whose subject is not the budget.
    ///
    /// [`terminates_timed_out_process`] owns the timeout contract and proves it
    /// against a child that never finishes on its own. Everywhere else a
    /// reachable budget can only decide the outcome by firing, and a killed
    /// child reports no exit code — so a busy host would quietly substitute
    /// "the machine was slow" for the exit-status mapping under test.
    const NOT_UNDER_TEST: Duration = Duration::MAX;

    #[test]
    fn missing_executable_is_typed() {
        let temp = tempdir().expect("tempdir");
        let args = Vec::new();
        let error = run_process(&ProcessRequest {
            program: "quality-lab-command-that-does-not-exist",
            args: &args,
            cwd: temp.path(),
            stdout_path: &temp.path().join("stdout.log"),
            stderr_path: &temp.path().join("stderr.log"),
            timeout: NOT_UNDER_TEST,
        })
        .expect_err("missing executable");

        assert!(matches!(error, ProcessError::MissingExecutable(_)));
    }

    #[test]
    fn external_processes_do_not_inherit_secret_environment() {
        let mut command = Command::new("env");

        remove_secret_environment(&mut command);

        let removed = command
            .get_envs()
            .filter_map(|(key, value)| value.is_none().then_some(key))
            .collect::<Vec<_>>();
        for key in SECRET_ENV_KEYS {
            assert!(removed.iter().any(|removed| removed == key));
        }
    }

    const CHILD_ENV: &str = "DEVTOOLS_PROCESS_CHILD";
    const CHILD_EMIT: &str = "emit";
    const CHILD_SLEEP: &str = "sleep";
    const CHILD_EXIT_CODE: i32 = 3;
    const STDOUT_MARKER: &str = "devtools-process-stdout-marker";
    const STDERR_MARKER: &str = "devtools-process-stderr-marker";

    #[test]
    fn captures_output_and_exit_status() {
        let temp = tempdir().expect("tempdir");
        let stdout = temp.path().join("stdout.log");
        let stderr = temp.path().join("stderr.log");
        let executable = env::current_exe().expect("current test executable");
        let args = crate::common::child_test_args(module_path!(), "emit_child_streams");

        let outcome = run_process_with_env(
            &ProcessRequest {
                program: executable.to_str().expect("UTF-8 path"),
                args: &args,
                cwd: temp.path(),
                stdout_path: &stdout,
                stderr_path: &stderr,
                timeout: NOT_UNDER_TEST,
            },
            &[(CHILD_ENV, OsStr::new(CHILD_EMIT))],
        )
        .expect("process");

        let out = fs::read_to_string(&stdout).expect("stdout");
        let err = fs::read_to_string(&stderr).expect("stderr");
        assert_eq!(outcome.status.code(), Some(CHILD_EXIT_CODE));
        assert!(out.contains(STDOUT_MARKER), "stdout file: {out}");
        assert!(
            !out.contains(STDERR_MARKER),
            "stderr leaked into stdout: {out}"
        );
        assert!(err.contains(STDERR_MARKER), "stderr file: {err}");
        assert!(
            !err.contains(STDOUT_MARKER),
            "stdout leaked into stderr: {err}"
        );
        assert!(!outcome.timed_out);
    }

    #[test]
    fn terminates_timed_out_process() {
        let temp = tempdir().expect("tempdir");
        let executable = env::current_exe().expect("current test executable");
        let args = crate::common::child_test_args(module_path!(), "sleep_past_any_budget");

        let outcome = run_process_with_env(
            &ProcessRequest {
                program: executable.to_str().expect("UTF-8 path"),
                args: &args,
                cwd: temp.path(),
                stdout_path: &temp.path().join("stdout.log"),
                stderr_path: &temp.path().join("stderr.log"),
                timeout: Duration::from_millis(30),
            },
            &[(CHILD_ENV, OsStr::new(CHILD_SLEEP))],
        )
        .expect("process");

        assert!(outcome.timed_out);
        assert!(outcome.duration < Duration::from_secs(2));
    }

    #[test]
    #[ignore = "subprocess entrypoint"]
    fn emit_child_streams() {
        assert_eq!(env::var(CHILD_ENV).as_deref(), Ok(CHILD_EMIT));
        println!("{STDOUT_MARKER}");
        eprintln!("{STDERR_MARKER}");
        io::Write::flush(&mut io::stdout()).expect("flush stdout");
        io::Write::flush(&mut io::stderr()).expect("flush stderr");
        std::process::exit(CHILD_EXIT_CODE);
    }

    #[test]
    #[ignore = "subprocess entrypoint"]
    fn sleep_past_any_budget() {
        assert_eq!(env::var(CHILD_ENV).as_deref(), Ok(CHILD_SLEEP));
        thread::sleep(Duration::from_secs(5));
    }
}
