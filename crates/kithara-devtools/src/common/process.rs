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
    pub(crate) program: &'a str,
    pub(crate) args: &'a [String],
    pub(crate) cwd: &'a Path,
    pub(crate) stdout_path: &'a Path,
    pub(crate) stderr_path: &'a Path,
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
                duration: start.elapsed(),
                status,
                timed_out: false,
            });
        }
        if start.elapsed() >= request.timeout {
            child.kill().map_err(ProcessError::Io)?;
            let status = child.wait().map_err(ProcessError::Io)?;
            return Ok(ProcessOutcome {
                duration: start.elapsed(),
                status,
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
    use std::{fs, time::Duration};

    use tempfile::tempdir;

    use super::*;

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
            timeout: Duration::from_secs(1),
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

    #[cfg(unix)]
    #[test]
    fn captures_output_and_exit_status() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let script = temp.path().join("fake-tool");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'native report\\n'\nprintf 'diagnostic\\n' >&2\nexit 3\n",
        )
        .expect("script");
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("permissions");
        let stdout = temp.path().join("stdout.log");
        let stderr = temp.path().join("stderr.log");
        let args = Vec::new();

        let outcome = run_process(&ProcessRequest {
            program: script.to_str().expect("UTF-8 path"),
            args: &args,
            cwd: temp.path(),
            stdout_path: &stdout,
            stderr_path: &stderr,
            timeout: Duration::from_secs(1),
        })
        .expect("process");

        assert_eq!(outcome.status.code(), Some(3));
        assert_eq!(
            fs::read_to_string(stdout).expect("stdout"),
            "native report\n"
        );
        assert_eq!(fs::read_to_string(stderr).expect("stderr"), "diagnostic\n");
        assert!(!outcome.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn terminates_timed_out_process() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let script = temp.path().join("slow-tool");
        fs::write(&script, "#!/bin/sh\nsleep 5\n").expect("script");
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("permissions");
        let args = Vec::new();

        let outcome = run_process(&ProcessRequest {
            program: script.to_str().expect("UTF-8 path"),
            args: &args,
            cwd: temp.path(),
            stdout_path: &temp.path().join("stdout.log"),
            stderr_path: &temp.path().join("stderr.log"),
            timeout: Duration::from_millis(30),
        })
        .expect("process");

        assert!(outcome.timed_out);
        assert!(outcome.duration < Duration::from_secs(2));
    }
}
