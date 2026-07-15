use kithara::{
    assets::{
        AssetLayout, AssetLayoutRegistry, AssetResource, AssetScope, AssetSource, AssetStore,
        ResourceKey,
    },
    platform::sync::Arc,
};
use url::Url;

const RESOURCE_NAMESPACE: &str = "test-resource";

#[derive(Debug)]
struct LiteralLayout;

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

pub(super) trait AssetStoreTestScopeExt {
    fn test_scope(&self, asset_root: &str) -> AssetScope;
}

impl AssetStoreTestScopeExt for AssetStore {
    fn test_scope(&self, asset_root: &str) -> AssetScope {
        let source = AssetSource::Remote {
            url: Url::parse("https://cache.test/source").expect("valid test URL"),
            discriminator: Some(asset_root.to_string()),
        };
        self.scope::<LiteralLayout>(&source)
            .expect("valid test asset root")
    }
}

pub(super) trait AssetScopeTestKeyExt {
    fn test_key<P>(&self, path: P) -> ResourceKey
    where
        P: Into<String>;
}

impl AssetScopeTestKeyExt for AssetScope {
    fn test_key<P>(&self, path: P) -> ResourceKey
    where
        P: Into<String>,
    {
        self.key(&AssetResource::Named {
            namespace: RESOURCE_NAMESPACE.to_string(),
            name: path.into(),
        })
        .expect("valid test resource path")
    }
}
