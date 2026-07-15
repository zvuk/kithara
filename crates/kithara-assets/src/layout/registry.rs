#![forbid(unsafe_code)]

use std::{any::TypeId, collections::HashMap};

use kithara_platform::sync::Arc;

use super::{AssetLayout, DefaultLayout};

/// Immutable store-time selection of layout policies by protocol marker type.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AssetLayoutRegistry {
    default: Arc<dyn AssetLayout>,
    overrides: HashMap<TypeId, Arc<dyn AssetLayout>>,
}

impl AssetLayoutRegistry {
    /// Create a registry with a caller-provided default layout.
    #[must_use]
    pub fn new(default: Arc<dyn AssetLayout>) -> Self {
        Self {
            default,
            overrides: HashMap::new(),
        }
    }

    /// Register or replace the layout selected for marker `T`.
    pub fn register<T: 'static>(&mut self, layout: Arc<dyn AssetLayout>) {
        self.overrides.insert(TypeId::of::<T>(), layout);
    }

    /// Register a layout for marker `T` and return the registry.
    #[must_use]
    pub fn with<T: 'static>(mut self, layout: Arc<dyn AssetLayout>) -> Self {
        self.register::<T>(layout);
        self
    }

    pub(crate) fn layout<T: 'static>(&self) -> &Arc<dyn AssetLayout> {
        self.overrides
            .get(&TypeId::of::<T>())
            .unwrap_or(&self.default)
    }
}

impl Default for AssetLayoutRegistry {
    fn default() -> Self {
        Self::new(Arc::new(DefaultLayout))
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{AssetResource, AssetSource};

    #[derive(Debug)]
    struct FixedLayout(&'static str);

    impl AssetLayout for FixedLayout {
        fn root(&self, _source: &AssetSource) -> String {
            self.0.to_string()
        }

        fn path(&self, _resource: &AssetResource) -> String {
            format!("{}/resource", self.0)
        }
    }

    struct File;
    struct Hls;

    fn source() -> AssetSource {
        AssetSource::Local {
            path: env::temp_dir().join("track.mp3"),
        }
    }

    #[kithara::test]
    fn override_is_exact_to_marker_type() {
        let registry = AssetLayoutRegistry::new(Arc::new(FixedLayout("default")))
            .with::<File>(Arc::new(FixedLayout("file")));

        assert_eq!(registry.layout::<File>().root(&source()), "file");
        assert_eq!(registry.layout::<Hls>().root(&source()), "default");
    }

    #[kithara::test]
    fn repeated_registration_replaces_marker_layout() {
        let mut registry = AssetLayoutRegistry::default();
        registry.register::<File>(Arc::new(FixedLayout("first")));
        registry.register::<File>(Arc::new(FixedLayout("second")));

        assert_eq!(registry.layout::<File>().root(&source()), "second");
    }
}
