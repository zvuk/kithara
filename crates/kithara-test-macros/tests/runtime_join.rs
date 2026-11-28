#![cfg(all(not(target_arch = "wasm32"), not(miri)))]

use std::{env, fs, future::Future, path::PathBuf, pin::Pin, process::Command, task::Poll};

use kithara_test_utils::{kithara, kithara_platform as platform, temp_dir};

#[kithara::fixture]
fn runtime_fixture() -> platform::tokio::runtime::Handle {
    platform::tokio::runtime::Handle::current()
}

#[kithara::test(native, tokio)]
async fn a_borrowed_fixture_uses_the_owned_runtime(
    #[by_ref] runtime_fixture: &platform::tokio::runtime::Handle,
) {
    assert_eq!(
        runtime_fixture
            .spawn(async { 7 })
            .await
            .expect("runtime task"),
        7
    );
}

async fn started_work(
    hang: bool,
    delay: platform::time::Duration,
) -> (
    platform::tokio::task::JoinHandle<()>,
    std::sync::mpsc::Sender<()>,
) {
    let owned = PathBuf::from(env::var_os("KITHARA_JOIN_OWNED").expect("owned path"));
    let witness = PathBuf::from(env::var_os("KITHARA_JOIN_WITNESS").expect("witness path"));
    fs::create_dir(&owned).expect("create owned directory");
    fs::write(owned.join("value"), b"owned until work finishes").expect("write owned value");
    let (started, ready) = platform::tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let task = platform::tokio::task::spawn_sync(move || {
        started.send(()).expect("work started");
        let _ = released.recv();
        if hang {
            loop {
                std::hint::spin_loop();
            }
        }
        kithara_test_utils::test::wall_sleep(delay);
        fs::remove_dir_all(&owned).expect("dispose owned directory");
        fs::write(witness, b"complete").expect("report completed disposal");
    });
    ready.await.expect("blocking work actually started");
    (task, release)
}

#[kithara::test(native, tokio)]
#[ignore]
#[case::success("success")]
#[case::failure("failure")]
#[case::panic("panic")]
#[case::cancel("cancel")]
#[case::timeout("timeout")]
#[case::deadline("deadline")]
async fn blocking_fixture(#[case] mode: &str) -> Result<(), &'static str> {
    let delay = platform::time::Duration::from_millis(if mode == "deadline" { 1250 } else { 250 });
    let (task, release) = started_work(false, delay).await;
    match mode {
        "success" | "deadline" => {
            drop(task);
            drop(release);
            Ok(())
        }
        "failure" => {
            drop(task);
            drop(release);
            Err("forced return failure")
        }
        "panic" => panic!("forced body panic"),
        "cancel" => {
            let mut waiting = Box::pin(task);
            std::future::poll_fn(|cx| {
                assert!(Pin::new(&mut waiting).poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(waiting);
            drop(release);
            Ok(())
        }
        "timeout" => {
            std::future::pending::<()>().await;
            drop(task);
            drop(release);
            Ok(())
        }
        _ => unreachable!(),
    }
}

#[kithara::test(native, tokio)]
#[ignore]
async fn blocked_runtime_fixture() {
    let (task, release) = started_work(true, platform::time::Duration::from_millis(250)).await;
    std::future::pending::<()>().await;
    drop(task);
    drop(release);
}

fn child(name: &str, owned: &std::path::Path, witness: &std::path::Path) -> std::process::Output {
    let mut command = Command::new(env::current_exe().expect("test executable"));
    command
        .args([
            "--exact",
            name,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("KITHARA_JOIN_OWNED", owned)
        .env("KITHARA_JOIN_WITNESS", witness);
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("NEXTEST_") {
            command.env_remove(key);
        }
    }
    let output = command.output().expect("owned child test completes");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
        "child selection must be exact: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    output
}

#[test]
fn started_blocking_work_is_joined_on_all_test_exits() {
    for (mode, passes) in [
        ("success", true),
        ("failure", false),
        ("panic", false),
        ("cancel", true),
        ("timeout", false),
        ("deadline", false),
    ] {
        let root = temp_dir();
        let owned = root.path().join("owned");
        let witness = root.path().join("complete");
        let output = child(&format!("blocking_fixture_{mode}"), &owned, &witness);
        let finished = witness.is_file();
        let disposed = !owned.exists();
        eprintln!(
            "RUNTIME_JOIN_CASE mode={mode} status={} finished={finished} disposed={disposed}",
            output.status
        );
        assert_eq!(
            output.status.success(),
            passes,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            finished && disposed,
            "started work escaped {mode}: finished={finished}, disposed={disposed}; {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn hard_watchdog_bounds_a_blocked_runtime_shutdown() {
    let root = temp_dir();
    let output = child(
        "blocked_runtime_fixture",
        &root.path().join("owned"),
        &root.path().join("complete"),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("RUNTIME_JOIN_WATCHDOG status={}\n{stderr}", output.status);
    assert!(!output.status.success());
    assert!(stderr.contains("HARD TIMEOUT"), "{stderr}");
    assert!(stderr.contains("runtime shutdown blocked"), "{stderr}");
}
