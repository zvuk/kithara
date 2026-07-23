use kithara::{
    assets::{AssetLayout, AssetLayoutRegistry, AssetResource, AssetSource},
    platform::sync::Arc,
};
use url::Url;

const RESOURCE_NAMESPACE: &str = "test-resource";

#[derive(Debug)]
pub(super) struct LiteralLayout;

impl AssetLayout for LiteralLayout {
    fn root(&self, source: &AssetSource) -> String {
        let AssetSource::Remote {
            discriminator: Some(root),
            ..
        } = source
        else {
            panic!("literal test layout requires an explicit root")
        };
        root.clone()
    }

    fn path(&self, resource: &AssetResource) -> String {
        let AssetResource::Named { namespace, name } = resource else {
            panic!("literal test layout requires a named resource")
        };
        assert_eq!(namespace, RESOURCE_NAMESPACE);
        name.clone()
    }
}

pub(super) fn literal_layouts() -> AssetLayoutRegistry {
    AssetLayoutRegistry::new(Arc::new(LiteralLayout))
}

pub(super) fn source(asset_root: &str) -> AssetSource {
    AssetSource::Remote {
        url: Url::parse("https://cache.test/source").expect("valid test URL"),
        discriminator: Some(asset_root.to_string()),
    }
}

pub(super) fn resource(path: impl Into<String>) -> AssetResource {
    AssetResource::Named {
        namespace: RESOURCE_NAMESPACE.to_string(),
        name: path.into(),
    }
}
