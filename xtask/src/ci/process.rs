use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    io::{Read as _, Seek as _},
    path::{Path as FsPath, PathBuf},
    process::{Child, Command, Output},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::verdict::ChildFailure;
use tracing::{debug, info, warn};

use crate::consts;

/// One thing a lane asked the executor to do, captured instead of done. The
/// snapshot built from these is what says a lane still resolves to the command
/// it resolved to before, without spending a CI run to find out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) label: String,
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) relative_dir: String,
}

#[derive(Debug, Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Recording {
    #[field(get, vis = "pub(crate)")]
    steps: Vec<Step>,
    /// What `capture` answers, by the command prefix it names. A lane that
    /// reads a tool's version and compares it to a pin needs an answer to get
    /// past that check, and one tool asked two questions needs two answers.
    replies: Vec<(Vec<String>, String)>,
}

impl Recording {
    pub(crate) fn with_reply(mut self, prefix: &[&str], reply: &str) -> Self {
        self.replies.push((
            prefix.iter().map(|part| (*part).to_owned()).collect(),
            reply.to_owned(),
        ));
        self
    }
}

enum Mode {
    Run,
    Record(Mutex<Recording>),
}

/// How long one step of a lane took, and whether it got there.
#[derive(Debug)]
pub(crate) struct Timing {
    pub(crate) elapsed: Duration,
    pub(crate) label: String,
    pub(crate) ok: bool,
}

/// Something a cache layer did during this job.
///
/// A lane's wall time is mostly decided by what the caches carried, and until
/// now a log said only that a step ran. These entries say which layer acted,
/// what it did, and about what - so a pipeline run is itself the evidence,
/// rather than a local test standing in for one.
#[derive(Debug)]
pub(crate) struct CacheNote {
    pub(crate) detail: String,
    pub(crate) event: &'static str,
    pub(crate) layer: &'static str,
}

/// What the job did, in the order it did it.
#[derive(Debug, Default)]
pub(crate) struct Journal {
    caches: Vec<CacheNote>,
    steps: Vec<Timing>,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Process {
    #[field(get(
        vis = "pub(crate)",
        doc = "The checkout every command of this process runs against."
    ))]
    root: PathBuf,
    vars: BTreeMap<OsString, OsString>,
    mode: Mode,
    journal: Mutex<Journal>,
}

impl Process {
    pub(crate) fn new(root: &FsPath, vars: BTreeMap<OsString, OsString>) -> Self {
        Self {
            root: root.to_path_buf(),
            vars,
            mode: Mode::Run,
            journal: Mutex::new(Journal::default()),
        }
    }

    /// A process that captures what a lane asks for instead of running it.
    /// Requirement checks answer yes: the point is the shape of the work, and a
    /// machine that lacks the executor's toolchain still has to record it.
    pub(crate) fn recording(root: &FsPath, recording: Recording) -> Self {
        Self {
            root: root.to_path_buf(),
            vars: BTreeMap::new(),
            mode: Mode::Record(Mutex::new(recording)),
            journal: Mutex::new(Journal::default()),
        }
    }

    pub(crate) fn recorded(self) -> Option<Recording> {
        match self.mode {
            Mode::Run => None,
            Mode::Record(recording) => recording.into_inner().ok(),
        }
    }

    pub(crate) const fn is_recording(&self) -> bool {
        matches!(&self.mode, Mode::Record(_))
    }

    fn record(&self, step: Step) -> bool {
        let Mode::Record(recording) = &self.mode else {
            return false;
        };
        if let Ok(mut recording) = recording.lock() {
            recording.steps.push(step);
        }
        true
    }

    fn reply(&self, program: &str, args: &[&str]) -> Option<String> {
        let Mode::Record(recording) = &self.mode else {
            return None;
        };
        let recording = recording.lock().ok()?;
        let command: Vec<&str> = std::iter::once(program)
            .chain(args.iter().copied())
            .collect();
        Some(
            recording
                .replies
                .iter()
                .filter(|(prefix, _)| {
                    prefix.len() <= command.len()
                        && prefix.iter().zip(&command).all(|(want, part)| want == part)
                })
                .max_by_key(|(prefix, _)| prefix.len())
                .map(|(_, reply)| reply.clone())
                .unwrap_or_default(),
        )
    }

    pub(crate) fn command(&self, program: impl AsRef<OsStr>) -> Command {
        self.command_in(program, "")
    }

    /// Where the builds this process runs leave their output. A lane that runs
    /// a binary it just built has to look where Cargo was told to write it:
    /// wherever the executor named, or the checkout's `target` when it named
    /// none.
    pub(crate) fn target_dir(&self) -> PathBuf {
        self.vars
            .get(OsStr::new("CARGO_TARGET_DIR"))
            .map_or_else(|| self.root.join("target"), PathBuf::from)
    }

    pub(crate) fn environment_var(&self, name: &str) -> Option<OsString> {
        self.vars
            .get(OsStr::new(name))
            .cloned()
            .or_else(|| env::var_os(name))
    }

    pub(crate) fn environment_path(&self, name: &str) -> Option<PathBuf> {
        self.environment_var(name).map(PathBuf::from)
    }

    /// Where this process's Cargo keeps its registry and git checkouts.
    pub(crate) fn cargo_home(&self) -> Option<PathBuf> {
        cargo_home(|name| self.environment_path(name))
    }

    /// A command that runs inside a subdirectory of the checkout. Build tools
    /// that locate their project by walking up from the working directory —
    /// Gradle looks for the settings file — need the directory that owns them,
    /// not the workspace root.
    pub(crate) fn command_in(&self, program: impl AsRef<OsStr>, relative: &str) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(self.root.join(relative))
            .envs(&self.vars);
        command
    }

    pub(crate) fn run(&self, program: &str, args: &[&str], label: &str) -> Result<()> {
        let mut command = self.command(program);
        command.args(args);
        self.run_command(&mut command, label)
    }

    pub(crate) fn run_command(&self, command: &mut Command, label: &str) -> Result<()> {
        if self.record(Step::of(command, label, &self.root)) {
            return Ok(());
        }
        info!(step = label, root = %self.root.display(), "starting");
        let started = Instant::now();
        let status = command
            .status()
            .with_context(|| format!("failed to start {label}"))?;
        let elapsed = started.elapsed();
        self.time(label, elapsed, status.success());
        info!(step = label, seconds = elapsed.as_secs_f64(), "done");
        if !status.success() {
            return Err(ChildFailure::inherited(label.to_owned(), status.code()));
        }
        Ok(())
    }

    /// Runs `command` with both of its streams in one file and returns what it
    /// wrote, for a step whose verdict is read from its output. A file, not a
    /// pipe: a server the command leaves running would hold a pipe open, and
    /// reading it would never end. A failure carries the output's last lines.
    pub(crate) fn transcript(&self, command: &mut Command, label: &str) -> Result<String> {
        if self.record(Step::of(command, label, &self.root)) {
            return Ok(String::new());
        }
        let mut file = tempfile::tempfile().context("creating a transcript file")?;
        command
            .stdout(file.try_clone().context("sharing the transcript file")?)
            .stderr(file.try_clone().context("sharing the transcript file")?);
        info!(step = label, root = %self.root.display(), "starting");
        let started = Instant::now();
        let status = command
            .status()
            .with_context(|| format!("failed to start {label}"))?;
        let elapsed = started.elapsed();
        self.time(label, elapsed, status.success());
        info!(step = label, seconds = elapsed.as_secs_f64(), "done");
        let mut bytes = Vec::new();
        file.rewind()
            .and_then(|()| file.read_to_end(&mut bytes))
            .context("reading the transcript")?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if !status.success() {
            let lines: Vec<&str> = text.lines().collect();
            let tail = lines
                .get(lines.len().saturating_sub(consts::TRANSCRIPT_TAIL_LINES)..)
                .unwrap_or_default()
                .join("\n");
            return Err(ChildFailure::captured(
                label.to_owned(),
                status.code(),
                tail,
            ));
        }
        Ok(text)
    }

    /// Record how long a step took. A step that never ran is not recorded, so
    /// the sum of these is the time the lane actually spent.
    pub(super) fn time(&self, label: &str, elapsed: Duration, ok: bool) {
        if let Ok(mut journal) = self.journal.lock() {
            journal.steps.push(Timing {
                elapsed,
                label: label.to_owned(),
                ok,
            });
        }
    }

    /// Record what a cache layer did, for the summary at the end of the job.
    pub(crate) fn note_cache(&self, layer: &'static str, event: &'static str, detail: String) {
        info!(cache = layer, event, detail = %detail, "cache");
        if let Ok(mut journal) = self.journal.lock() {
            journal.caches.push(CacheNote {
                detail,
                event,
                layer,
            });
        }
    }

    /// Numeric step observations, without command labels, arguments or environment.
    pub(crate) fn timing_evidence(&self) -> Option<serde_json::Value> {
        let (step_count, steps) = {
            let journal = self.journal.lock().ok()?;
            let steps: Vec<_> = journal.steps.iter().take(128).map(|step| {
            serde_json::json!({"elapsed_ms": step.elapsed.as_secs_f64() * 1000.0, "ok": step.ok})
        }).collect();
            (journal.steps.len(), steps)
        };
        Some(serde_json::json!({"step_count": step_count, "steps": steps}))
    }

    /// Print what the job spent and what its caches carried.
    ///
    /// Steps come out slowest first, because the question a CI log is read
    /// with is where the time went. The cache lines come out in the order the
    /// layers acted, because that order is the story: what was restored
    /// before the work, what the work reused, what was written after it.
    pub(crate) fn report_journal(&self, lane: &str) {
        let Ok(journal) = self.journal.lock() else {
            return;
        };
        let total: Duration = journal.steps.iter().map(|step| step.elapsed).sum();
        let mut steps: Vec<&Timing> = journal.steps.iter().collect();
        steps.sort_by_key(|step| std::cmp::Reverse(step.elapsed));
        println!(
            "lane {lane}: {:.1} s across {} step(s)",
            total.as_secs_f64(),
            steps.len()
        );
        for step in steps {
            let share = if total.is_zero() {
                0.0
            } else {
                100.0 * step.elapsed.as_secs_f64() / total.as_secs_f64()
            };
            println!(
                "  {:>9.1} s  {:>5.1}%  {}{}",
                step.elapsed.as_secs_f64(),
                share,
                step.label,
                if step.ok { "" } else { "  (failed)" }
            );
        }
        if journal.caches.is_empty() {
            println!("lane {lane}: no cache layer reported anything");
            return;
        }
        println!("lane {lane}: cache layers");
        for note in &journal.caches {
            println!("  {:<18} {:<12} {}", note.layer, note.event, note.detail);
        }
    }

    pub(crate) fn capture(&self, program: &str, args: &[&str], label: &str) -> Result<String> {
        let mut command = self.command(program);
        command.args(args);
        self.capture_command(&mut command, label)
    }

    pub(crate) fn capture_command(&self, command: &mut Command, label: &str) -> Result<String> {
        if self.is_recording() {
            let program = command.get_program().to_string_lossy();
            let arguments: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_string_lossy())
                .collect();
            let args: Vec<_> = arguments.iter().map(AsRef::as_ref).collect();
            self.record(Step::of(command, label, &self.root));
            return Ok(self.reply(&program, &args).unwrap_or_default());
        }
        let output = command
            .output()
            .with_context(|| format!("failed to start {label}"))?;
        output_text(output, label)
    }

    /// Capture a bounded report without retaining or printing the tool's stderr.
    pub(super) fn capture_bounded(
        &self,
        program: &str,
        args: &[&str],
        label: &str,
        limit: usize,
    ) -> Result<String> {
        self.capture_bounded_command(
            self.command(program).args(args),
            label,
            Duration::from_secs(5),
            limit,
        )
    }

    pub(super) fn capture_bounded_command(
        &self,
        command: &mut Command,
        label: &str,
        deadline: Duration,
        limit: usize,
    ) -> Result<String> {
        if self.is_recording() {
            return self.capture_command(command, label);
        }
        let started = Instant::now();
        let output = crate::child::bounded_output(command, deadline, limit);
        self.time(
            label,
            started.elapsed(),
            output.as_ref().is_ok_and(|value| value.status.success()),
        );
        let output = output?;
        if !output.status.success() {
            return Err(ChildFailure::inherited(
                label.to_owned(),
                output.status.code(),
            ));
        }
        String::from_utf8(output.stdout).with_context(|| format!("{label} was not UTF-8"))
    }

    /// The platform a lane refuses to run anywhere but on. Recorded rather than
    /// enforced while recording: the shape of a macOS lane is worth capturing
    /// from a Linux runner too.
    pub(crate) fn require_os<S: AsRef<str>>(&self, expected: &[S], label: &str) -> Result<()> {
        let expected: Vec<String> = expected.iter().map(|os| os.as_ref().to_owned()).collect();
        if self.record(Step::requirement(label, "os", &expected)) {
            return Ok(());
        }
        require_os(&expected, label)
    }

    /// What a predecessor job has to have left behind. Recorded rather than
    /// checked while recording, for the same reason the platform is: whether
    /// this checkout happens to hold the file says nothing about the lane.
    pub(crate) fn require_left_behind(&self, paths: &[String], by: &str) -> Result<()> {
        if self.record(Step::requirement(by, "left behind", paths)) {
            return Ok(());
        }
        for relative in paths {
            let path = self.root.join(relative);
            if !path.exists() {
                bail!("the {by} job did not leave {}", path.display());
            }
        }
        Ok(())
    }

    /// Start a long-running child the lane owns for its duration. Recorded
    /// rather than spawned while recording: a server that binds a port is a
    /// side effect, and whether the binary happens to be built is not part of
    /// what the lane asks for.
    pub(crate) fn spawn(&self, command: &mut Command, label: &str) -> Result<Option<Child>> {
        if self.record(Step::of(command, label, &self.root)) {
            return Ok(None);
        }
        let child = command
            .spawn()
            .with_context(|| format!("starting {label}"))?;
        Ok(Some(child))
    }

    pub(crate) fn best_effort(&self, program: &str, args: &[&str], label: &str) {
        if let Err(error) = self.run(program, args, label) {
            warn!(step = label, %error, "best-effort step failed");
        }
    }

    /// Reach a state, accepting only a caller-classified refusal that proves
    /// the state already holds. Recorded rather than reached while recording:
    /// the state is this machine's, and the lane is what the snapshot pins.
    pub(crate) fn ensure(
        &self,
        program: &str,
        args: &[&str],
        label: &str,
        already_satisfied: impl FnOnce(&Output) -> bool,
    ) -> Result<()> {
        let mut command = self.command(program);
        command.args(args);
        if self.record(Step::of(&command, label, &self.root)) {
            return Ok(());
        }
        let output = command
            .output()
            .with_context(|| format!("failed to start {label}"))?;
        if output.status.success() {
            info!(step = label, "done");
            return Ok(());
        }
        if already_satisfied(&output) {
            debug!(step = label, "already so");
            return Ok(());
        }
        output_text(output, label).map(drop)
    }

    pub(crate) fn require_tools(&self, tools: &[&str]) -> Result<()> {
        let owned: Vec<String> = tools.iter().map(|tool| (*tool).to_owned()).collect();
        if self.record(Step::requirement("required tools", "tools", &owned)) {
            return Ok(());
        }
        for tool in tools {
            if self.find_executable(tool).is_none() {
                bail!("required CI command is missing: {tool}");
            }
        }
        Ok(())
    }

    pub(crate) fn resolve_program(&self, program: &str) -> Result<PathBuf> {
        self.find_executable(program)
            .with_context(|| format!("required CI command is missing: {program}"))
    }

    fn find_executable(&self, program: &str) -> Option<PathBuf> {
        let path = FsPath::new(program);
        if path.components().count() > 1 {
            return path.is_file().then(|| path.to_path_buf());
        }

        let search = self
            .vars
            .get(OsStr::new("PATH"))
            .cloned()
            .or_else(|| env::var_os("PATH"))?;
        let extensions = executable_extensions(&self.vars);
        env::split_paths(&search).find_map(|directory| {
            extensions.iter().find_map(|extension| {
                let candidate = directory.join(format!("{program}{extension}"));
                candidate.is_file().then_some(candidate)
            })
        })
    }
}

impl Step {
    fn of(command: &Command, label: &str, root: &FsPath) -> Self {
        Self {
            label: label.to_owned(),
            program: command.get_program().to_string_lossy().into_owned(),
            args: command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
            env: command
                .get_envs()
                .filter_map(|(key, value)| {
                    value.map(|value| {
                        (
                            key.to_string_lossy().into_owned(),
                            value.to_string_lossy().into_owned(),
                        )
                    })
                })
                .collect(),
            relative_dir: command
                .get_current_dir()
                .and_then(|dir| dir.strip_prefix(root).ok())
                .map(|dir| dir.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    fn requirement(label: &str, kind: &str, values: &[String]) -> Self {
        Self {
            label: label.to_owned(),
            program: format!("<require {kind}>"),
            args: values.to_vec(),
            env: BTreeMap::new(),
            relative_dir: String::new(),
        }
    }
}

pub(crate) fn require_os(expected: &[String], label: &str) -> Result<()> {
    if !expected.iter().any(|os| os == env::consts::OS) {
        bail!(
            "{label} lane requires {}, current platform is {}",
            expected.join(" or "),
            env::consts::OS
        );
    }
    Ok(())
}

fn executable_extensions(vars: &BTreeMap<OsString, OsString>) -> Vec<String> {
    if cfg!(windows) {
        vars.get(OsStr::new("PATHEXT"))
            .cloned()
            .or_else(|| env::var_os("PATHEXT"))
            .map_or_else(
                || vec![".exe".into(), ".cmd".into(), ".bat".into()],
                |value| {
                    value
                        .to_string_lossy()
                        .split(';')
                        .map(str::to_ascii_lowercase)
                        .collect()
                },
            )
    } else {
        vec![String::new()]
    }
}

/// The directory Cargo itself settles on: the one it was named, else `.cargo`
/// in the home directory. An image that stops naming it still has one, and a
/// cache layer restored anywhere else is one Cargo never reads.
fn cargo_home(lookup: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    lookup("CARGO_HOME").or_else(|| {
        lookup("HOME")
            .or_else(|| lookup("USERPROFILE"))
            .map(|home| home.join(".cargo"))
    })
}

fn output_text(output: Output, label: &str) -> Result<String> {
    if !output.status.success() {
        return Err(ChildFailure::captured(
            label.to_owned(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("{label} produced non-UTF-8 standard output"))
        .map(|text| text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts;

    fn fixture_ensure(
        process: &Process,
        script: &str,
        already_satisfied: impl FnOnce(&Output) -> bool,
    ) -> Result<()> {
        if cfg!(windows) {
            process.ensure("cmd", &["/C", script], "fixture state", already_satisfied)
        } else {
            process.ensure("sh", &["-c", script], "fixture state", already_satisfied)
        }
    }

    #[test]
    fn failed_output_keeps_command_context() {
        let output = if cfg!(windows) {
            Command::new("cmd")
                .args(["/C", "exit", "7"])
                .output()
                .unwrap()
        } else {
            Command::new("sh").args(["-c", "exit 7"]).output().unwrap()
        };
        let error = output_text(output, "fixture command").unwrap_err();
        assert!(error.downcast_ref::<ChildFailure>().is_some());
        assert!(error.to_string().contains("fixture command"));
        assert!(error.to_string().contains('7'));
    }

    #[test]
    fn inherited_failure_is_typed() {
        let process = Process::new(FsPath::new("."), BTreeMap::new());
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/C", "exit", "7"]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 7"]);
            command
        };

        let error = process
            .run_command(&mut command, "fixture command")
            .unwrap_err();

        assert!(error.downcast_ref::<ChildFailure>().is_some());
        assert_eq!(error.to_string(), "fixture command failed (exit code 7)");
    }

    #[test]
    fn environment_paths_fall_back_to_the_executor_environment() {
        let process = Process::new(FsPath::new("."), BTreeMap::new());

        assert_eq!(
            process.environment_path("PATH").as_deref(),
            env::var_os("PATH").as_deref().map(FsPath::new)
        );
    }

    #[test]
    fn ensure_accepts_only_an_explicitly_classified_state() {
        let process = Process::new(FsPath::new("."), BTreeMap::new());
        let script = if cfg!(windows) {
            "echo already 1>&2 & exit /B 7"
        } else {
            "printf already >&2; exit 7"
        };

        fixture_ensure(&process, script, |output| {
            output.status.code() == Some(consts::FIXTURE_FAILURE_EXIT_CODE)
        })
        .unwrap();
    }

    #[test]
    fn ensure_accepts_success_without_classifying_it() {
        let process = Process::new(FsPath::new("."), BTreeMap::new());
        let script = if cfg!(windows) { "exit /B 0" } else { "exit 0" };

        fixture_ensure(&process, script, |_| panic!("success needs no classifier")).unwrap();
    }

    #[test]
    fn ensure_rejects_an_unclassified_failure_with_context() {
        let process = Process::new(FsPath::new("."), BTreeMap::new());
        let script = if cfg!(windows) {
            "echo unexpected 1>&2 & exit /B 7"
        } else {
            "printf unexpected >&2; exit 7"
        };

        let error = fixture_ensure(&process, script, |_| false).unwrap_err();

        assert!(error.downcast_ref::<ChildFailure>().is_some());
        assert!(error.to_string().contains("fixture state"));
        assert!(error.to_string().contains('7'));
        assert!(error.to_string().contains("unexpected"));
    }

    #[test]
    fn an_image_that_names_no_cargo_home_uses_the_one_in_home() {
        let environment = BTreeMap::from([("HOME", "/home/runner")]);

        let home = cargo_home(|name| environment.get(name).map(PathBuf::from));

        assert_eq!(home, Some(PathBuf::from("/home/runner/.cargo")));
    }

    #[test]
    fn a_named_cargo_home_is_the_one_used() {
        let environment =
            BTreeMap::from([("CARGO_HOME", "/cache/cargo"), ("HOME", "/home/runner")]);

        let home = cargo_home(|name| environment.get(name).map(PathBuf::from));

        assert_eq!(home, Some(PathBuf::from("/cache/cargo")));
    }

    /// One tool asked two questions needs two answers: `xcrun` both creates a
    /// simulator and reads a result bundle. The longest prefix that names the
    /// command answers it.
    #[test]
    fn a_reply_answers_the_longest_command_prefix_it_names() {
        let process = Process::recording(
            FsPath::new("."),
            Recording::default()
                .with_reply(&["xcrun"], "bundle")
                .with_reply(&["xcrun", "simctl", "create"], "device"),
        );

        let created = process
            .capture("xcrun", &["simctl", "create", "name", "type"], "create")
            .unwrap();
        let read = process
            .capture("xcrun", &["xcresulttool", "get"], "read")
            .unwrap();

        assert_eq!(created, "device");
        assert_eq!(read, "bundle");
    }

    /// Reaching a state changes the machine like any other step, so a
    /// recording captures it rather than running it.
    #[test]
    fn a_recording_captures_an_ensure_instead_of_running_it() {
        let process = Process::recording(FsPath::new("."), Recording::default());

        process
            .ensure(
                "kithara-command-that-does-not-exist",
                &["state"],
                "reach a state",
                |_| false,
            )
            .unwrap();

        let recorded = process.recorded().unwrap();
        let programs: Vec<&str> = recorded
            .steps()
            .iter()
            .map(|step| step.program.as_str())
            .collect();
        assert_eq!(programs, ["kithara-command-that-does-not-exist"]);
    }

    #[test]
    fn ensure_rejects_a_command_that_cannot_start() {
        let process = Process::new(FsPath::new("."), BTreeMap::new());

        let error = process
            .ensure(
                "kithara-command-that-does-not-exist",
                &[],
                "missing fixture",
                |_| false,
            )
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("failed to start missing fixture")
        );
    }

    /// A step read from its output hands both streams over in the order they
    /// were written, and a failure keeps the tail that says why, not the head.
    #[cfg(unix)]
    #[test]
    fn a_transcript_holds_both_streams_and_a_failure_keeps_its_tail() {
        let process = Process::new(FsPath::new("/"), BTreeMap::new());

        let text = process
            .transcript(
                Command::new("sh").args(["-c", "echo out; echo err >&2"]),
                "both",
            )
            .unwrap();
        let error = process
            .transcript(
                Command::new("sh").args([
                    "-c",
                    "echo first; for _ in $(seq 40); do echo -; done; echo last >&2; exit 3",
                ]),
                "failing",
            )
            .unwrap_err();

        assert_eq!(text, "out\nerr\n");
        let failure = error
            .downcast_ref::<ChildFailure>()
            .expect("a child failure");
        assert!(failure.to_string().contains("last"), "{failure}");
        assert!(!failure.to_string().contains("first"), "{failure}");
    }
}
