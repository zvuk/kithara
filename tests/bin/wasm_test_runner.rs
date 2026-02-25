//! Custom WASM test runner that starts fixture server automatically.
//!
//! Cargo invokes this as: `wasm_test_runner <test.wasm> [filter] [--options...]`
//!
//! It starts the fixture server in-process (as a tokio task), waits for it
//! to be ready, then delegates to `wasm-bindgen-test-runner` with all args
//! forwarded. When the process exits, the fixture server dies with it.

// On wasm32, provide a no-op main.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let port: u16 = std::env::var("FIXTURE_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3333);
    let health_url = format!("http://127.0.0.1:{port}/health");

    // Check if server is already running (developer started it manually)
    let already_running = reqwest::get(&health_url).await.is_ok();

    if !already_running {
        // Start fixture server as a background tokio task (in this process).
        // Import the server module from fixture_server binary is not possible
        // directly, so we start it as a subprocess.
        let fixture_bin = std::env::current_exe()
            .ok()
            .and_then(|p| {
                let dir = p.parent()?;
                Some(dir.join("fixture_server"))
            })
            .expect("could not find fixture_server binary next to wasm_test_runner");

        let mut child = std::process::Command::new(&fixture_bin)
            .env("FIXTURE_PORT", port.to_string())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| {
                panic!(
                    "failed to start fixture_server at {}: {e}",
                    fixture_bin.display()
                )
            });

        // Wait for readiness (up to 10 seconds)
        let mut ready = false;
        for _ in 0..100 {
            if reqwest::get(&health_url).await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if !ready {
            let _ = child.kill();
            panic!("fixture server did not become ready within 10 seconds");
        }

        eprintln!("Fixture server started on port {port}");

        // Run wasm-bindgen-test-runner and wait for it
        let status = std::process::Command::new("wasm-bindgen-test-runner")
            .args(&args)
            .env("WASM_BINDGEN_USE_BROWSER", "1")
            .status()
            .expect("wasm-bindgen-test-runner not found in PATH");

        // Kill fixture server
        let _ = child.kill();
        let _ = child.wait();

        std::process::exit(status.code().unwrap_or(1));
    } else {
        eprintln!("Fixture server already running on port {port}");

        // Delegate to wasm-bindgen-test-runner with all args forwarded
        let status = std::process::Command::new("wasm-bindgen-test-runner")
            .args(&args)
            .env("WASM_BINDGEN_USE_BROWSER", "1")
            .status()
            .expect("wasm-bindgen-test-runner not found in PATH");

        std::process::exit(status.code().unwrap_or(1));
    }
}
