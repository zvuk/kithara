use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, RecvTimeoutError, SyncSender},
    thread::{self, ScopedJoinHandle},
    time::Duration,
};

use anyhow::{Context, Error, Result, anyhow};

struct Consts;

impl Consts {
    /// Caps queued output at 128 `KiB`; full queues apply backpressure to readers.
    const CHANNEL_DEPTH: usize = 8;
    /// Bounds reader-failure detection latency while the sibling stream is idle.
    const READER_POLL_INTERVAL: Duration = Duration::from_millis(20);
    /// Amortizes reads without accumulating a complete stress log in memory.
    const READ_BUFFER_BYTES: usize = 16 * 1024;
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    const fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

struct Chunk {
    stream: Stream,
    bytes: Vec<u8>,
}

struct OutputWriter<'a> {
    log_path: &'a Path,
    log: File,
    stderr: io::StderrLock<'a>,
    stdout: io::StdoutLock<'a>,
}

impl OutputWriter<'_> {
    fn flush_log(&mut self) -> Result<()> {
        self.log
            .flush()
            .with_context(|| format!("flush stress log {}", self.log_path.display()))
    }

    fn flush_stderr(&mut self) -> Result<()> {
        self.stderr.flush().context("flush child stderr")
    }

    fn flush_stdout(&mut self) -> Result<()> {
        self.stdout.flush().context("flush child stdout")
    }

    fn write_log(&mut self, bytes: &[u8]) -> Result<()> {
        self.log
            .write_all(bytes)
            .with_context(|| format!("write stress log {}", self.log_path.display()))
    }

    fn write_stderr(&mut self, bytes: &[u8]) -> Result<()> {
        self.stderr
            .write_all(bytes)
            .context("stream child stderr")?;
        self.stderr.flush().context("flush child stderr")
    }

    fn write_stdout(&mut self, bytes: &[u8]) -> Result<()> {
        self.stdout
            .write_all(bytes)
            .context("stream child stdout")?;
        self.stdout.flush().context("flush child stdout")
    }
}

/// Runs a command while relaying and recording both of its output streams.
///
/// # Errors
///
/// Returns an error when the log cannot be created, the child cannot be
/// started or reaped, or any output reader or writer fails.
pub(super) fn run(command: &mut Command, log_path: &Path) -> Result<ExitStatus> {
    let log = create_log(log_path)?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().context("start captured stress command")?;
    let Some(stdout) = child.stdout.take() else {
        return fail_after_spawn(
            &mut child,
            anyhow!("captured stress command has no stdout pipe"),
        );
    };
    let Some(stderr) = child.stderr.take() else {
        return fail_after_spawn(
            &mut child,
            anyhow!("captured stress command has no stderr pipe"),
        );
    };

    let parent_stdout = io::stdout();
    let parent_stderr = io::stderr();
    let mut writer = OutputWriter {
        log,
        log_path,
        stderr: parent_stderr.lock(),
        stdout: parent_stdout.lock(),
    };

    thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(Consts::CHANNEL_DEPTH);
        let stdout_sender = sender.clone();
        let mut stdout_reader =
            Some(scope.spawn(move || read_stream(stdout, Stream::Stdout, &stdout_sender)));
        let mut stderr_reader =
            Some(scope.spawn(move || read_stream(stderr, Stream::Stderr, &sender)));
        let mut failure = None;
        let mut log_available = true;
        let mut stderr_available = true;
        let mut stdout_available = true;

        loop {
            match receiver.recv_timeout(Consts::READER_POLL_INTERVAL) {
                Ok(chunk) => {
                    if log_available && let Err(error) = writer.write_log(&chunk.bytes) {
                        log_available = false;
                        record_failure(&mut failure, &mut child, error);
                    }
                    match chunk.stream {
                        Stream::Stdout if stdout_available => {
                            if let Err(error) = writer.write_stdout(&chunk.bytes) {
                                stdout_available = false;
                                record_failure(&mut failure, &mut child, error);
                            }
                        }
                        Stream::Stderr if stderr_available => {
                            if let Err(error) = writer.write_stderr(&chunk.bytes) {
                                stderr_available = false;
                                record_failure(&mut failure, &mut child, error);
                            }
                        }
                        Stream::Stdout | Stream::Stderr => {}
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            if let Some(error) = poll_reader(&mut stdout_reader, Stream::Stdout) {
                record_failure(&mut failure, &mut child, error);
            }
            if let Some(error) = poll_reader(&mut stderr_reader, Stream::Stderr) {
                record_failure(&mut failure, &mut child, error);
            }
        }

        if let Some(error) = join_reader(&mut stdout_reader, Stream::Stdout) {
            record_failure(&mut failure, &mut child, error);
        }
        if let Some(error) = join_reader(&mut stderr_reader, Stream::Stderr) {
            record_failure(&mut failure, &mut child, error);
        }
        if log_available && let Err(error) = writer.flush_log() {
            record_failure(&mut failure, &mut child, error);
        }
        if stdout_available && let Err(error) = writer.flush_stdout() {
            record_failure(&mut failure, &mut child, error);
        }
        if stderr_available && let Err(error) = writer.flush_stderr() {
            record_failure(&mut failure, &mut child, error);
        }

        failure.map_or_else(
            || child.wait().context("wait for captured stress command"),
            Err,
        )
    })
}

/// Runs a command whose stdout is already routed, while relaying and recording stderr.
///
/// # Errors
///
/// Returns an error when the child or either stderr destination fails.
pub(super) fn run_stderr(command: &mut Command, log_path: &Path) -> Result<ExitStatus> {
    let mut log = create_log(log_path)?;
    let parent_stderr = io::stderr();
    let mut destination = parent_stderr.lock();
    run_stderr_to(command, log_path, &mut log, &mut destination)
}

fn run_stderr_to(
    command: &mut Command,
    log_path: &Path,
    log: &mut impl Write,
    destination: &mut impl Write,
) -> Result<ExitStatus> {
    command.stderr(Stdio::piped());
    let mut child = command.spawn().context("start captured stress command")?;
    let Some(mut stderr) = child.stderr.take() else {
        return fail_after_spawn(
            &mut child,
            anyhow!("captured stress command has no stderr pipe"),
        );
    };
    let mut buffer = [0_u8; Consts::READ_BUFFER_BYTES];
    loop {
        let count = match stderr.read(&mut buffer).context("read child stderr") {
            Ok(count) => count,
            Err(error) => return fail_after_spawn(&mut child, error),
        };
        if count == 0 {
            break;
        }
        if let Err(error) = log
            .write_all(&buffer[..count])
            .with_context(|| format!("write stress log {}", log_path.display()))
        {
            return fail_after_spawn(&mut child, error);
        }
        if let Err(error) = destination
            .write_all(&buffer[..count])
            .context("stream child stderr")
        {
            return fail_after_spawn(&mut child, error);
        }
    }
    if let Err(error) = log
        .flush()
        .with_context(|| format!("flush stress log {}", log_path.display()))
    {
        return fail_after_spawn(&mut child, error);
    }
    if let Err(error) = destination.flush().context("flush child stderr") {
        return fail_after_spawn(&mut child, error);
    }
    child.wait().context("wait for captured stress command")
}

fn create_log(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create stress log directory {}", parent.display()))?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open stress log {}", path.display()))
}

fn read_stream<R: Read>(mut reader: R, stream: Stream, sender: &SyncSender<Chunk>) -> Result<()> {
    let mut buffer = [0_u8; Consts::READ_BUFFER_BYTES];
    loop {
        let count = reader
            .read(&mut buffer)
            .with_context(|| format!("read child {}", stream.name()))?;
        if count == 0 {
            return Ok(());
        }
        sender
            .send(Chunk {
                stream,
                bytes: buffer[..count].to_vec(),
            })
            .map_err(|_| anyhow!("forward child {} to output writer", stream.name()))?;
    }
}

fn poll_reader(
    handle: &mut Option<ScopedJoinHandle<'_, Result<()>>>,
    stream: Stream,
) -> Option<Error> {
    if handle.as_ref().is_some_and(ScopedJoinHandle::is_finished) {
        return join_reader(handle, stream);
    }
    None
}

fn join_reader(
    handle: &mut Option<ScopedJoinHandle<'_, Result<()>>>,
    stream: Stream,
) -> Option<Error> {
    let handle = handle.take()?;
    match handle.join() {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.context(format!("child {} reader failed", stream.name()))),
        Err(_) => Some(anyhow!("child {} reader thread panicked", stream.name())),
    }
}

fn record_failure(failure: &mut Option<Error>, child: &mut Child, error: Error) {
    if failure.is_some() {
        return;
    }
    let error = match terminate_child(child) {
        Ok(()) => error,
        Err(cleanup) => error.context(format!(
            "child cleanup also failed after output capture failure: {cleanup:#}"
        )),
    };
    *failure = Some(error);
}

fn fail_after_spawn<T>(child: &mut Child, error: Error) -> Result<T> {
    let error = match terminate_child(child) {
        Ok(()) => error,
        Err(cleanup) => error.context(format!(
            "child cleanup also failed after pipe setup failure: {cleanup:#}"
        )),
    };
    Err(error)
}

fn terminate_child(child: &mut Child) -> Result<()> {
    let kill_error = child.kill().err();
    match child.wait() {
        Ok(_) => Ok(()),
        Err(wait_error) => {
            let error = Error::new(wait_error).context("wait for failed captured stress command");
            match kill_error {
                Some(kill_error) => Err(error.context(format!(
                    "terminate failed captured stress command: {kill_error}"
                ))),
                None => Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        io::{Read as _, Write as _},
        process,
    };

    use tempfile::tempdir;

    use super::*;

    const CHILD_ENV: &str = "DEVTOOLS_STRESS_OUTPUT_CHILD";
    const CHILD_COMPLETION_FILE: &str = "DEVTOOLS_STRESS_OUTPUT_COMPLETION_FILE";
    const CHILD_ENV_VALUE: &str = "emit";
    const BLOCK_CHILD_ENV_VALUE: &str = "emit-then-block";
    const CHILD_EXIT_CODE: i32 = 23;
    const STDERR_MARKER: &[u8] = b"stress-output-stderr-marker\n";
    const STDOUT_MARKER: &[u8] = b"stress-output-stdout-marker\n";

    #[test]
    fn captures_both_streams_without_changing_exit_status() {
        let temp = tempdir().expect("tempdir");
        let log_path = temp.path().join("nested/stress.log");
        let executable = env::current_exe().expect("current test executable");
        let mut command = Command::new(executable);
        command
            .args(crate::common::child_test_args(
                module_path!(),
                "emit_child_output",
            ))
            .env(CHILD_ENV, CHILD_ENV_VALUE);

        let status = run(&mut command, &log_path).expect("capture child output");

        assert_eq!(status.code(), Some(CHILD_EXIT_CODE));
        let log = fs::read(log_path).expect("read combined log");
        assert!(contains(&log, STDOUT_MARKER));
        assert!(contains(&log, STDERR_MARKER));
    }

    #[test]
    fn stderr_sink_failure_terminates_and_reaps_child() {
        let temp = tempdir().expect("tempdir");
        let completion = temp.path().join("completed");
        let executable = env::current_exe().expect("current test executable");
        let mut command = Command::new(executable);
        command
            .args(crate::common::child_test_args(
                module_path!(),
                "emit_then_block",
            ))
            .stdin(Stdio::piped())
            .env(CHILD_ENV, BLOCK_CHILD_ENV_VALUE)
            .env(CHILD_COMPLETION_FILE, &completion);
        let mut failing_log = FailingWriter;
        let mut destination = Vec::new();

        let error = run_stderr_to(
            &mut command,
            Path::new("failing.log"),
            &mut failing_log,
            &mut destination,
        )
        .expect_err("sink failure must fail capture");

        assert!(error.to_string().contains("write stress log"));
        assert!(!completion.exists());
    }

    #[test]
    #[ignore = "subprocess entrypoint"]
    fn emit_child_output() {
        assert_eq!(env::var(CHILD_ENV).as_deref(), Ok(CHILD_ENV_VALUE));
        {
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(STDOUT_MARKER)
                .expect("write stdout marker");
            stdout.flush().expect("flush stdout marker");
        }
        {
            let mut stderr = io::stderr().lock();
            stderr
                .write_all(STDERR_MARKER)
                .expect("write stderr marker");
            stderr.flush().expect("flush stderr marker");
        }
        process::exit(CHILD_EXIT_CODE);
    }

    #[test]
    #[ignore = "subprocess entrypoint"]
    fn emit_then_block() {
        assert_eq!(env::var(CHILD_ENV).as_deref(), Ok(BLOCK_CHILD_ENV_VALUE));
        let mut stderr = io::stderr().lock();
        stderr
            .write_all(STDERR_MARKER)
            .expect("write stderr marker");
        stderr.flush().expect("flush stderr marker");
        drop(stderr);
        let mut input = Vec::new();
        io::stdin()
            .read_to_end(&mut input)
            .expect("wait for parent pipe closure");
        fs::write(
            env::var_os(CHILD_COMPLETION_FILE).expect("completion path"),
            b"completed\n",
        )
        .expect("write completion marker");
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("injected sink failure"))
        }

        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("injected sink failure"))
        }
    }

    fn contains(bytes: &[u8], needle: &[u8]) -> bool {
        bytes.windows(needle.len()).any(|window| window == needle)
    }
}
