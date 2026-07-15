use kithara_assets::{AssetResource, AssetScope, AssetSource, AssetStore, ResourceKey};
use url::Url;

pub(crate) struct TestProtocol;

fn source(identity: &str) -> AssetSource {
    AssetSource::Remote {
        url: Url::parse("https://assets.test.invalid/track").expect("valid test source URL"),
        discriminator: Some(identity.to_owned()),
    }
}

pub(crate) fn scope(store: &AssetStore, identity: &str) -> AssetScope {
    store
        .scope::<TestProtocol>(&source(identity))
        .expect("valid test asset scope")
}

pub(crate) fn key(scope: &AssetScope, raw: impl AsRef<str>) -> ResourceKey {
    let raw = raw.as_ref();
    let (namespace, name) = raw.split_once('/').unwrap_or(("resource", raw));
    scope
        .key(&AssetResource::Named {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
        })
        .expect("valid test resource key")
}
