use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    path::{Path as FsPath, PathBuf},
    process::{Child, Command, Output},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::verdict::ChildFailure;
use tracing::{debug, info, warn};

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
    /// What `capture` answers, by program. A lane that reads a tool's version
    /// and compares it to a pin needs an answer to get past that check.
    replies: BTreeMap<String, String>,
}

impl Recording {
    pub(crate) fn with_reply(mut self, program: &str, reply: &str) -> Self {
        self.replies.insert(program.to_owned(), reply.to_owned());
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

    fn reply(&self, program: &str) -> Option<String> {
        let Mode::Record(recording) = &self.mode else {
            return None;
        };
        let recording = recording.lock().ok()?;
        Some(recording.replies.get(program).cloned().unwrap_or_default())
    }

    pub(crate) fn command(&self, program: impl AsRef<OsStr>) -> Command {
        self.command_in(program, "")
    }

    /// Where the builds this process runs leave their output. A lane that runs
    /// a binary it just built has to look where Cargo was told to write it,
    /// which is not the default directory on an executor.
    pub(crate) fn target_dir(&self) -> PathBuf {
        self.vars
            .get(OsStr::new("CARGO_TARGET_DIR"))
            .map_or_else(|| self.root.join("target"), PathBuf::from)
    }

    pub(crate) fn environment_path(&self, name: &str) -> Option<PathBuf> {
        self.vars
            .get(OsStr::new(name))
            .cloned()
            .or_else(|| env::var_os(name))
            .map(PathBuf::from)
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

    /// Record how long a step took. A step that never ran is not recorded, so
    /// the sum of these is the time the lane actually spent.
    fn time(&self, label: &str, elapsed: Duration, ok: bool) {
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
        if let Some(reply) = self.reply(program) {
            let mut command = self.command(program);
            command.args(args);
            self.record(Step::of(&command, label, &self.root));
            return Ok(reply);
        }
        let output = self
            .command(program)
            .args(args)
            .output()
            .with_context(|| format!("failed to start {label}"))?;
        output_text(output, label)
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
    /// the state already holds.
    pub(crate) fn ensure(
        &self,
        program: &str,
        args: &[&str],
        label: &str,
        already_satisfied: impl FnOnce(&Output) -> bool,
    ) -> Result<()> {
        let output = self
            .command(program)
            .args(args)
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
}
