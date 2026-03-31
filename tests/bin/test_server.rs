//! Entry point for spec-driven test server.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() {
    use kithara_test_utils::run_test_server;

    run_test_server().await;
}
