#![cfg(not(target_arch = "wasm32"))]

use std::{panic::AssertUnwindSafe, path::Path};

use futures::FutureExt;
use kithara_integration_tests::{TestServerHelper, kithara};
use kithara_test_fixtures::assets::MANIFEST;
use kithara_test_utils::kithara_platform::time::Duration;

use super::providers::{Provider, sources};

type Census = (
    TestServerHelper,
    Vec<(Provider, Result<Vec<String>, String>)>,
);

#[kithara::fixture]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn provider_sources() -> Census {
    let _io = kithara::platform::flash::real_io();
    hang_tick!("provider_sources: starting the fixture server".to_owned());
    let server =
        kithara::platform::time::timeout(__hang_detector.remaining(), TestServerHelper::new())
            .await;
    hang_tick!("provider_sources: starting the fixture server".to_owned());
    let server = server.expect("fixture server starts within its liveness budget");
    hang_reset!();
    let mut entries = Vec::new();
    for provider in Provider::ALL {
        hang_tick!(format!("provider_sources: materialising {provider:?}"));
        let paths = AssertUnwindSafe(kithara::platform::time::timeout(
            __hang_detector.remaining(),
            sources(*provider, 2, &server),
        ))
        .catch_unwind()
        .await;
        hang_tick!(format!("provider_sources: materialising {provider:?}"));
        let paths = paths
            .map(|result| result.expect("fixture sources arrive within the watchdog budget"))
            .map_err(|payload| {
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_default()
            });
        entries.push((*provider, paths));
        hang_reset!();
    }
    (server, entries)
}

/// Whether the build itself reported one of this provider's fixtures as absent.
///
/// A provider may only fail to produce sources for this reason. Every other
/// blockage is a defect the census has to report.
fn build_reported_absent(provider: &Provider) -> bool {
    let Provider::Library(names) = provider else {
        return false;
    };
    names.iter().any(|name| {
        MANIFEST
            .iter()
            .find(|entry| entry.name == *name)
            .is_some_and(|entry| entry.unavailable.is_some())
    })
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn every_provider_materialises_two_sources(#[future(awt)] provider_sources: Census) {
    let (_server, entries) = provider_sources;
    for (provider, paths) in entries {
        let paths = match paths {
            Ok(paths) => paths,
            Err(message) => {
                assert!(
                    message.starts_with("BLOCKED_FIXTURE"),
                    "{provider:?}: {message}"
                );
                assert!(
                    build_reported_absent(&provider),
                    "{provider:?} is blocked while its fixtures are present: {message}"
                );
                continue;
            }
        };
        assert_eq!(paths.len(), 2, "{provider:?}");
        for path in &paths {
            let exists = path.starts_with("http") || Path::new(path).is_file();
            assert!(
                exists,
                "{provider:?}: {path} is neither a served URL nor a file"
            );
        }
    }
}
