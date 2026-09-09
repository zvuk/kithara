#![cfg(not(target_arch = "wasm32"))]

use kithara_integration_tests::{PrivateTestServer, TestServerHelper};

/// Proves the lane builds and reaches the test server. This is the only
/// green test in `suite_integration_regressions`, distinguishing a broken lane
/// from a red regression test.
#[kithara::test(tokio)]
async fn lane_reaches_test_server() {
    let helper = TestServerHelper::new().await;
    assert!(
        helper.base_url().as_str().starts_with("http://127.0.0.1:"),
        "test server must bind loopback, got {}",
        helper.base_url()
    );
}

/// The network switch is reachable over HTTP and cannot lock itself out:
/// `/control/*` must keep responding while data routes are offline.
///
/// Runs against a private server because it leaves data routes dead for the
/// span of the test, which on the shared server every parallel sibling sees.
#[kithara::test(tokio)]
async fn network_switch_is_reachable_over_http() {
    let server = PrivateTestServer::start().await;
    let helper = server.helper();
    let control = helper.url("/control/network");
    let health = helper.url("/health");
    let client = reqwest::Client::new();

    let offline = client
        .post(control.clone())
        .json(&serde_json::json!({ "online": false }))
        .send()
        .await
        .expect("control endpoint must answer");
    assert_eq!(offline.status(), 204);

    let blocked = client
        .get(helper.url("/behavior/missing"))
        .send()
        .await
        .expect("server must still accept connections while offline");
    assert_eq!(
        blocked.status(),
        503,
        "data routes must be dead while the switch is off"
    );

    let alive = client
        .get(health)
        .send()
        .await
        .expect("health must survive");
    assert_eq!(alive.status(), 200);

    let online = client
        .post(control)
        .json(&serde_json::json!({ "online": true }))
        .send()
        .await
        .expect("control endpoint must stay reachable while offline");
    assert_eq!(online.status(), 204);
}
