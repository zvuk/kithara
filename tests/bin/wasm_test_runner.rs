//! Custom WASM test runner that starts fixture server automatically.
//!
//! Cargo invokes this as: `wasm_test_runner <test.wasm> [filter] [--options...]`
//!
//! It starts the fixture server in-process (as a tokio task), waits for it
//! to be ready, then delegates to `wasm-bindgen-test-runner` with all args
//! forwarded. When the process exits, the fixture server dies with it.

#[cfg(not(target_arch = "wasm32"))]
use std::{env, process, time::Instant};

#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::time::{Duration, sleep};

// On wasm32, provide a no-op main.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
async fn run_wasm_bindgen_runner(args: &[String], runner_timeout_secs: u64) -> process::ExitStatus {
    let mut child = process::Command::new("wasm-bindgen-test-runner")
        .env("WASM_BINDGEN_USE_BROWSER", "1")
        .env_remove("WASM_BINDGEN_USE_NO_MODULE")
        .args(args)
        .spawn()
        .expect("wasm-bindgen-test-runner not found in PATH");

    if runner_timeout_secs == 0 {
        return child
            .wait()
            .expect("failed waiting for wasm-bindgen-test-runner");
    }

    let deadline = Instant::now() + Duration::from_secs(runner_timeout_secs);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "wasm-bindgen-test-runner exceeded {runner_timeout_secs} seconds and was killed (likely browser/renderer hang). Override with WASM_TEST_RUNNER_TIMEOUT_SECS."
            );
        }
        sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn default_runner_timeout_secs(args: &[String]) -> u64 {
    if let Ok(raw) = env::var("WASM_TEST_RUNNER_TIMEOUT_SECS")
        && let Ok(parsed) = raw.parse::<u64>()
    {
        return parsed;
    }

    // `cargo test ... <filter>` forwards `<wasm> <filter> [--options]`.
    // For filtered runs, prefer a shorter default to fail fast on hangs.
    let has_filter = args.get(1).is_some_and(|arg| !arg.starts_with('-'));
    if has_filter { 180 } else { 1800 }
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let port: u16 = env::var("FIXTURE_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3333);
    let fixture_startup_timeout_secs: u64 = env::var("FIXTURE_STARTUP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let runner_timeout_secs = default_runner_timeout_secs(&args);
    let health_url = format!("http://127.0.0.1:{port}/health");
    eprintln!("wasm-bindgen watchdog timeout: {runner_timeout_secs}s");

    // Check if server is already running (developer started it manually)
    let already_running = reqwest::get(&health_url).await.is_ok();

    if !already_running {
        // Start fixture server as a background tokio task (in this process).
        // Import the server module from fixture_server binary is not possible
        // directly, so we start it as a subprocess.
        let fixture_bin = env::current_exe()
            .ok()
            .and_then(|p| {
                let dir = p.parent()?;
                Some(dir.join("fixture_server"))
            })
            .expect("could not find fixture_server binary next to wasm_test_runner");

        let mut child = process::Command::new(&fixture_bin)
            .env("FIXTURE_PORT", port.to_string())
            .stdout(process::Stdio::piped())
            .stderr(process::Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| {
                panic!(
                    "failed to start fixture_server at {}: {e}",
                    fixture_bin.display()
                )
            });

        // Wait for readiness (default: up to 30 seconds; configurable via env).
        let mut ready = false;
        let max_attempts = fixture_startup_timeout_secs.saturating_mul(10).max(1);
        for _ in 0..max_attempts {
            if let Ok(Some(status)) = child.try_wait() {
                let output = child.wait_with_output().ok();
                let stdout = output
                    .as_ref()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "<empty>".to_string());
                let stderr = output
                    .as_ref()
                    .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "<empty>".to_string());
                panic!(
                    "fixture_server exited before becoming ready (status: {status})\nfixture_server stdout:\n{stdout}\nfixture_server stderr:\n{stderr}"
                );
            }

            if reqwest::get(&health_url).await.is_ok() {
                ready = true;
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
        if !ready {
            let _ = child.kill();
            let output = child.wait_with_output().ok();
            let stdout = output
                .as_ref()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "<empty>".to_string());
            let stderr = output
                .as_ref()
                .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "<empty>".to_string());
            panic!(
                "fixture server did not become ready within {fixture_startup_timeout_secs} seconds\nfixture_server stdout:\n{stdout}\nfixture_server stderr:\n{stderr}"
            );
        }

        eprintln!("Fixture server started on port {port}");

        // Run wasm-bindgen-test-runner under a host-side watchdog timeout.
        let status = run_wasm_bindgen_runner(&args, runner_timeout_secs).await;

        // Kill fixture server
        let _ = child.kill();
        let _ = child.wait();

        process::exit(status.code().unwrap_or(1));
    } else {
        eprintln!("Fixture server already running on port {port}");

        // Delegate to wasm-bindgen-test-runner with all args forwarded.
        let status = run_wasm_bindgen_runner(&args, runner_timeout_secs).await;

        process::exit(status.code().unwrap_or(1));
    }
}
