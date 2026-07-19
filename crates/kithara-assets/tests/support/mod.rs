use kithara_assets::{AssetResource, AssetSource};
use url::Url;

pub(crate) struct Test;

pub(crate) fn source(identity: &str) -> AssetSource {
    AssetSource::Remote {
        url: Url::parse("https://assets.test.invalid/track").expect("valid test source URL"),
        discriminator: Some(identity.to_owned()),
    }
}

pub(crate) fn resource(raw: impl AsRef<str>) -> AssetResource {
    let raw = raw.as_ref();
    let (namespace, name) = raw.split_once('/').unwrap_or(("resource", raw));
    AssetResource::Named {
        namespace: namespace.to_owned(),
        name: name.to_owned(),
    }
}
