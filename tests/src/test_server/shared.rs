use std::{
    sync::{Arc, OnceLock},
    thread,
};

use kithara_platform::tokio::runtime::Builder as RuntimeBuilder;
use url::Url;

use crate::{native::http_server::router_base_url_on_runtime, test_server_state::TestServerState};

/// Process-global test server. Lives on a dedicated runtime thread so it
/// outlives any individual `#[tokio::test]` runtime within the process.
pub(crate) struct SharedTestServer {
    pub(crate) base_url: Url,
    pub(crate) state: Arc<TestServerState>,
}

static SHARED: OnceLock<SharedTestServer> = OnceLock::new();

/// Get the process-global server, starting it on first use.
pub(crate) fn shared() -> &'static SharedTestServer {
    SHARED.get_or_init(|| {
        let state = TestServerState::new();
        let state_for_thread = Arc::clone(&state);
        let (tx, rx) = std::sync::mpsc::channel::<Url>();
        thread::Builder::new()
            .name("kithara-shared-test-server".into())
            .spawn(move || {
                let rt = RuntimeBuilder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build shared-server runtime");
                rt.block_on(async move {
                    // Server STARTUP (socket bind + readiness delay) is real
                    // infrastructure, not sim-clock-driven test logic: run it on
                    // REAL time so its readiness sleep does not wait on the virtual
                    // clock — which the caller has frozen by blocking on `rx.recv()`
                    // below (a deadlock otherwise). The scope drops before serving
                    // begins, so per-request THROTTLE sleeps stay on the sim clock
                    // and still collapse under `flash`.
                    let base_url = {
                        let _real = kithara_platform::flash::flash_real();
                        router_base_url_on_runtime(state_for_thread).await
                    };
                    tx.send(base_url).expect("send shared server base url");
                    std::future::pending::<()>().await;
                });
            })
            .expect("spawn shared test server thread");
        let base_url = rx.recv().expect("shared server failed to start");
        SharedTestServer { base_url, state }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kithara;

    #[kithara::test(tokio)]
    async fn shared_server_is_one_instance_and_serves_health() {
        let a = shared().base_url.clone();
        let b = shared().base_url.clone();
        assert_eq!(a, b, "shared() must return the same server across calls");

        let resp = reqwest::get(a.join("/health").unwrap()).await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "ok");
    }
}
