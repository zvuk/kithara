//! A custom [`AssetLayout`] that returns an unsafe `rel_path` must be
//! rejected by store-side validation with a typed error, never silently
//! rewritten. The built-in layouts sanitize; hostile custom ones do not.

use std::sync::Arc;

use kithara_assets::{AssetLayout, AssetStoreBuilder, AssetsError, ResourceInfo};
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use tempfile::tempdir;
use url::Url;

#[derive(Debug)]
struct HostileLayout(&'static str);

impl AssetLayout for HostileLayout {
    fn rel_path(&self, _info: &ResourceInfo<'_>) -> String {
        self.0.to_string()
    }
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
#[case("../escape")]
#[case("/absolute/path")]
#[case("")]
#[case("dir/../escape")]
fn hostile_layout_rel_path_is_rejected(#[case] hostile: &'static str) {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::default()
        .root_dir(dir.path())
        .layout(Arc::new(HostileLayout(hostile)))
        .build();
    let scope = store.scope("root");

    let url = Url::parse("https://example.com/audio.mp3").unwrap();
    let key = scope.key_for(&ResourceInfo::Track {
        url: &url,
        name: None,
        ext_hint: Some("mp3"),
    });

    let err = scope
        .store()
        .acquire_resource(&key, None)
        .expect_err("hostile rel_path must be rejected");
    assert!(
        matches!(err, AssetsError::InvalidKey),
        "expected InvalidKey, got {err:?}"
    );
}
