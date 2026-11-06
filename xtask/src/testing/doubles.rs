//! Test doubles for the external programs `xtask` spawns.

use std::{
    fs,
    path::{Path, PathBuf},
};
#[cfg(unix)]
use std::{
    process::Command,
    sync::{Mutex, MutexGuard, PoisonError},
};

/// Hold the process's stop signals for one test.
///
/// A stop signal reaches every [`Cancel`](crate::child::Cancel) installed in
/// the process, and `cargo test` runs every test in one process: a test that
/// raises one would cancel its neighbours. A test that installs a `Cancel`
/// holds this for as long as the `Cancel` lives.
#[cfg(unix)]
pub(crate) fn signals() -> MutexGuard<'static, ()> {
    static SIGNALS: Mutex<()> = Mutex::new(());
    SIGNALS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Put the workspace's `fake-tool` where the code under test will look.
///
/// A unit test cannot ask Cargo where a binary target landed, so it is located
/// relative to this test binary: `target/<profile>/deps/<test>` sits one level
/// below the directory Cargo writes binaries into. `--bins` builds the binary
/// as a harness rather than as itself, which is why the message names the
/// build command.
pub(crate) fn install_double(bin: &Path, role: &str) -> PathBuf {
    let source = std::env::current_exe()
        .expect("current test executable")
        .parent()
        .and_then(Path::parent)
        .expect("the test binary lives under the profile directory")
        .join(format!("fake-tool{}", std::env::consts::EXE_SUFFIX));
    assert!(
        source.is_file(),
        "build the fake tool first: cargo build -p xtask --bin fake-tool ({})",
        source.display()
    );
    fs::create_dir_all(bin).expect("create the tool directory");
    let destination = bin.join(format!("{role}{}", std::env::consts::EXE_SUFFIX));
    publish(&source, &destination);
    destination
}

/// A copy is written, and a test thread that forks while the copy is open for
/// writing hands that descriptor to its child; executing the copy then fails
/// with "Text file busy" until the child execs. A link writes nothing.
#[cfg(unix)]
fn publish(source: &Path, destination: &Path) {
    std::os::unix::fs::symlink(source, destination).expect("publish the fake tool");
}

#[cfg(not(unix))]
fn publish(source: &Path, destination: &Path) {
    fs::copy(source, destination).expect("publish the fake tool");
}

/// Install the workspace's `fake-tool` at `bin/<role>` running `body`, for a
/// double the code under test runs by that absolute path and whose behaviour
/// that code's environment cannot select.
///
/// The body is written beside the double as `<role>.sh`, and the double runs
/// it with `/bin/sh`: the body is only read, and the program executed is the
/// tool every other double already ran. A CI Mac holds the first run of an
/// executable it has not run before for seconds, longer than a bounded call
/// waits for its answer, so a newly written executable answering such a call
/// fails the call instead of answering it. A file that is only read cannot be
/// "Text file busy" either, so this process writes it.
#[cfg(unix)]
pub(crate) fn install_scripted_double(bin: &Path, role: &str, body: &str) -> PathBuf {
    let double = install_double(bin, role);
    let mut script = double.clone().into_os_string();
    script.push(".sh");
    fs::write(&script, body).expect("write the double's script");
    double
}

/// Install `body` as an executable script at `path`, for a double whose
/// behaviour the environment of the code under test cannot select.
///
/// Its first run waits on a host that checks every new executable, which a
/// call bounded by a shorter deadline does not outlast; a double installed
/// with [`install_scripted_double`] runs nothing newly written.
///
/// Writing it from this process would open the "Text file busy" window [`publish`]
/// avoids, and a rename cannot close it: the leaked descriptor names the inode.
/// A child shell writes it instead and is reaped before this returns, so no
/// process that could still hold a writable descriptor for it remains.
#[cfg(unix)]
pub(crate) fn install_script(path: &Path, body: &str) {
    let status = Command::new("sh")
        .args([
            "-c",
            r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#,
            "sh",
            body,
        ])
        .arg(path)
        .status()
        .expect("spawn the script writer");
    assert!(status.success(), "write the script {}", path.display());
}

/// Copy the program at `source` to `path`, for a double that must be a file of
/// its own rather than a link the test may later overwrite through.
///
/// A copy written from this process opens the window [`install_script`] closes,
/// so a child `cp` writes it and is reaped before this returns.
#[cfg(unix)]
pub(crate) fn install_copy(source: &Path, path: &Path) {
    let status = Command::new("cp")
        .arg(source)
        .arg(path)
        .status()
        .expect("spawn the program copier");
    assert!(
        status.success(),
        "copy {} to {}",
        source.display(),
        path.display()
    );
}

#[cfg(unix)]
#[test]
fn executable_alias_preserves_the_tool_role() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let executable = install_double(directory.path(), "launchctl");
    assert!(executable.is_file());
    let status = Command::new(executable)
        .arg("bootout")
        .env("KITHARA_TEST_RULES", "launchctl:bootout:*=7,*:*:*=9")
        .status()
        .expect("execute the published tool alias");
    assert_eq!(status.code(), Some(7));
}

/// A scripted double runs its script on its own arguments, while the program
/// executed is the built tool rather than anything newly written.
#[cfg(unix)]
#[test]
fn a_scripted_double_runs_its_body_through_the_built_tool() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let said = directory.path().join("said");
    let double = install_scripted_double(
        directory.path(),
        "tool",
        &format!("printf '%s' \"$*\" > '{}'\nexit 7\n", said.display()),
    );

    assert_eq!(
        fs::canonicalize(&double)
            .expect("the double exists")
            .file_name()
            .and_then(|name| name.to_str()),
        Some("fake-tool"),
        "the double is the built tool, not a newly written executable"
    );
    let status = Command::new(&double)
        .args(["a", "b"])
        .status()
        .expect("run the double");
    assert_eq!(status.code(), Some(7));
    assert_eq!(fs::read_to_string(&said).expect("the body ran"), "a b");
}

/// Only Linux refuses to execute a file some process can still write to, so
/// only Linux can tell this writer from a direct one.
#[cfg(target_os = "linux")]
#[test]
fn an_installed_script_runs_while_other_threads_fork() {
    use std::{
        sync::mpsc::{self, TryRecvError},
        thread,
    };

    let directory = tempfile::tempdir().expect("temporary directory");
    let (running, stopped) = mpsc::channel::<()>();
    thread::scope(move |scope| {
        scope.spawn(move || {
            while matches!(stopped.try_recv(), Err(TryRecvError::Empty)) {
                Command::new("true").status().expect("fork a bystander");
            }
        });
        for index in 0..200 {
            let script = directory.path().join(format!("script-{index}"));
            install_script(&script, "#!/bin/sh\nexit 7\n");
            let status = Command::new(&script)
                .status()
                .expect("execute the installed script");
            assert_eq!(status.code(), Some(7));
        }
        drop(running);
    });
}
