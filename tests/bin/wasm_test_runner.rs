#[cfg(not(target_arch = "wasm32"))]
use std::{
    env,
    ffi::OsString,
    fs,
    path::PathBuf,
    process::{self, Child, Command, Stdio},
    time::Instant,
};

#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::time::{Duration, sleep};
#[cfg(not(target_arch = "wasm32"))]
use tempfile::TempDir;

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
async fn run_wasm_bindgen_runner(args: &[String], runner_timeout_secs: u64) -> process::ExitStatus {
    let shim_dir = install_worker_shim_alias().expect("failed to prepare worker shim alias");
    let mut runner = Command::new("wasm-bindgen-test-runner");
    runner
        .current_dir(shim_dir.path())
        .env("WASM_BINDGEN_USE_BROWSER", "1")
        .env_remove("WASM_BINDGEN_USE_NO_MODULE")
        .args(args);
    if env::var_os("CHROMEDRIVER").is_none()
        && let Some(chromedriver) = find_in_path("chromedriver")
    {
        runner.env("CHROMEDRIVER", chromedriver);
    }
    let mut child = runner
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
fn find_in_path(binary: &str) -> Option<OsString> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
        .map(PathBuf::into_os_string)
}

#[cfg(not(target_arch = "wasm32"))]
fn install_worker_shim_alias() -> Result<TempDir, std::io::Error> {
    let dir = tempfile::tempdir()?;
    let alias = dir.path().join("kithara-ffi.js");
    fs::write(
        alias,
        "export * from './wasm-bindgen-test.js';\n\
         export { default } from './wasm-bindgen-test.js';\n",
    )?;

    let webdriver_config = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webdriver.json");
    if webdriver_config.exists() {
        fs::copy(&webdriver_config, dir.path().join("webdriver.json"))?;
    }

    Ok(dir)
}

#[cfg(not(target_arch = "wasm32"))]
fn default_runner_timeout_secs(args: &[String]) -> u64 {
    if let Ok(raw) = env::var("WASM_TEST_RUNNER_TIMEOUT_SECS")
        && let Ok(parsed) = raw.parse::<u64>()
    {
        return parsed;
    }

    let has_filter = args.get(1).is_some_and(|arg| !arg.starts_with('-'));
    if has_filter { 180 } else { 1800 }
}

#[cfg(not(target_arch = "wasm32"))]
async fn http_ready(url: &str) -> bool {
    reqwest::get(url).await.is_ok()
}

#[cfg(not(target_arch = "wasm32"))]
async fn spawn_server_binary(
    name: &'static str,
    binary_name: &'static str,
    port_env_name: &'static str,
    port: u16,
    health_url: &str,
    startup_timeout_secs: u64,
) -> Child {
    let server_bin = env::current_exe()
        .ok()
        .and_then(|p| {
            let dir = p.parent()?;
            Some(dir.join(binary_name))
        })
        .unwrap_or_else(|| panic!("could not find {binary_name} binary next to wasm_test_runner"));

    let mut child = Command::new(&server_bin)
        .env(port_env_name, port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to start {name} at {}: {e}", server_bin.display()));

    let mut ready = false;
    let max_attempts = startup_timeout_secs.saturating_mul(10).max(1);
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
                "{name} exited before becoming ready (status: {status})\n{name} stdout:\n{stdout}\n{name} stderr:\n{stderr}"
            );
        }

        if http_ready(health_url).await {
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
            "{name} did not become ready within {startup_timeout_secs} seconds\n{name} stdout:\n{stdout}\n{name} stderr:\n{stderr}"
        );
    }

    child
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let test_server_port: u16 = env::var("TEST_SERVER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3444);
    let test_server_startup_timeout_secs: u64 = env::var("TEST_SERVER_STARTUP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let runner_timeout_secs = default_runner_timeout_secs(&args);
    let test_server_health_url = format!("http://127.0.0.1:{test_server_port}/health");
    eprintln!("wasm-bindgen watchdog timeout: {runner_timeout_secs}s");

    let test_server_already_running = http_ready(&test_server_health_url).await;
    let mut test_server_child = None;

    if !test_server_already_running {
        test_server_child = Some(
            spawn_server_binary(
                "test_server",
                "test_server",
                "TEST_SERVER_PORT",
                test_server_port,
                &test_server_health_url,
                test_server_startup_timeout_secs,
            )
            .await,
        );
        eprintln!("Test server started on port {test_server_port}");
    } else {
        eprintln!("Test server already running on port {test_server_port}");
    }

    let status = run_wasm_bindgen_runner(&args, runner_timeout_secs).await;

    if let Some(mut child) = test_server_child {
        let _ = child.kill();
        let _ = child.wait();
    }

    process::exit(status.code().unwrap_or(1));
}
