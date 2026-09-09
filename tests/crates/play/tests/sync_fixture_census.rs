#![cfg(not(target_arch = "wasm32"))]

use std::{panic::AssertUnwindSafe, path::Path};

use futures::FutureExt;
use kithara::platform::time::Duration;
use kithara_integration_tests::{TestServerHelper, kithara};

use super::sync_product_matrix::{Provider, sources};

type Census = (
    TestServerHelper,
    Vec<(Provider, Result<Vec<String>, String>)>,
);

#[kithara::fixture]
async fn provider_sources() -> Census {
    let server = TestServerHelper::new().await;
    let mut entries = Vec::new();
    for provider in Provider::ALL {
        let paths = AssertUnwindSafe(sources(*provider, 2, &server))
            .catch_unwind()
            .await
            .map_err(|payload| {
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_default()
            });
        entries.push((*provider, paths));
    }
    (server, entries)
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn every_provider_materialises_two_sources(#[future(awt)] provider_sources: Census) {
    let (_server, entries) = provider_sources;
    let mut blocked = Vec::new();
    for (provider, paths) in entries {
        let paths = match paths {
            Ok(paths) => paths,
            Err(message) => {
                assert!(
                    message.starts_with("BLOCKED_FIXTURE"),
                    "{provider:?}: {message}"
                );
                blocked.push(format!("{provider:?}: {message}"));
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
    if std::env::var_os("KITHARA_REMOTE_FIXTURES").is_some_and(|value| !value.is_empty()) {
        assert!(
            blocked.is_empty(),
            "requested remote fixtures are unavailable: {blocked:?}"
        );
    }
    eprintln!("blocked providers: {blocked:?}");
}
