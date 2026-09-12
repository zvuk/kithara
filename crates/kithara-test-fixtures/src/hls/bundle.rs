use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use thiserror::Error;

use crate::{
    asset::{Asset, AssetError},
    hls_manifest::Manifest,
};

/// One locally cached HLS resource addressed by its rewritten playlist route.
#[derive(Debug)]
pub struct HlsResource {
    path: PathBuf,
    content_type: String,
}

impl HlsResource {
    /// MIME type to return when serving this resource.
    #[must_use]
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// Cached resource body.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A complete cached VOD graph: rewritten playlists, media, init sections and keys.
#[derive(Debug)]
pub struct HlsBundle {
    resources: BTreeMap<String, HlsResource>,
    master: String,
}

/// Invalid or unavailable HLS bundle manifest.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HlsBundleError {
    /// The fixture itself was not materialized or disappeared.
    #[error(transparent)]
    Asset(#[from] AssetError),
    /// The cached manifest is not UTF-8.
    #[error("HLS bundle manifest is not UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    /// The cached manifest is not valid TOML.
    #[error("invalid HLS bundle manifest: {0}")]
    Manifest(#[from] toml::de::Error),
    /// The bundle manifest has no filesystem parent.
    #[error("HLS bundle fixture must be stored on disk")]
    NoStore,
    /// A generated route or filename violates the bundle contract.
    #[error("invalid HLS bundle {field}: {value}")]
    Invalid {
        /// Manifest field.
        field: &'static str,
        /// Rejected value.
        value: String,
    },
}

impl TryFrom<&Asset> for HlsBundle {
    type Error = HlsBundleError;

    fn try_from(asset: &Asset) -> Result<Self, Self::Error> {
        let relative_root = Path::new(asset.entry().path)
            .parent()
            .ok_or(HlsBundleError::NoStore)?;
        let manifest: Manifest = toml::from_str(std::str::from_utf8(asset.try_bytes()?)?)?;
        let mut resources = BTreeMap::new();
        for resource in manifest.resources {
            if !resource.route.starts_with('/') {
                return Err(HlsBundleError::Invalid {
                    field: "route",
                    value: resource.route,
                });
            }
            let file = Path::new(&resource.file);
            if file.components().count() != 1
                || !matches!(file.components().next(), Some(Component::Normal(_)))
            {
                return Err(HlsBundleError::Invalid {
                    field: "file",
                    value: resource.file,
                });
            }
            let route = resource.route;
            let previous = resources.insert(
                route.clone(),
                HlsResource {
                    content_type: resource.content_type,
                    path: crate::store::file(&relative_root.join(file))
                        .map_err(AssetError::Store)?,
                },
            );
            if previous.is_some() {
                return Err(HlsBundleError::Invalid {
                    field: "duplicate route",
                    value: route,
                });
            }
        }
        if !resources.contains_key(&manifest.master) {
            return Err(HlsBundleError::Invalid {
                field: "master route",
                value: manifest.master,
            });
        }
        Ok(Self {
            resources,
            master: manifest.master,
        })
    }
}

impl HlsBundle {
    /// Resource for one rewritten route.
    #[must_use]
    pub fn get(&self, route: &str) -> Option<&HlsResource> {
        self.resources.get(route)
    }

    /// Every route in the bundle.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &HlsResource)> {
        self.resources
            .iter()
            .map(|(route, resource)| (route.as_str(), resource))
    }

    /// Rewritten master-playlist route.
    #[must_use]
    pub fn master_route(&self) -> &str {
        &self.master
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use kithara_test_utils::kithara;
    use tempfile::TempDir;

    use super::HlsBundle;
    use crate::{
        asset::{Asset, AssetEntry},
        hls_manifest::{Manifest, Resource},
    };

    #[kithara::test(native, flash(false))]
    fn resolves_manifest_files_under_the_asset_store() {
        static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
        let root = crate::store::runtime_root().expect("fixture store root");
        let temp = TempDir::new_in(root).expect("temporary store");
        let body = temp.path().join("master.m3u8");
        std::fs::write(&body, b"#EXTM3U\n").expect("write master");
        let manifest_path = temp.path().join("bundle.toml");
        std::fs::write(
            &manifest_path,
            toml::to_string(&Manifest {
                master: "/hls/master.m3u8".to_owned(),
                resources: vec![Resource {
                    content_type: "application/vnd.apple.mpegurl".to_owned(),
                    file: "master.m3u8".to_owned(),
                    route: "/hls/master.m3u8".to_owned(),
                }],
            })
            .expect("serialize manifest"),
        )
        .expect("write manifest");
        let path = Box::leak(
            manifest_path
                .strip_prefix(root)
                .expect("manifest is inside the store")
                .to_string_lossy()
                .into_owned()
                .into_boxed_str(),
        );
        let entry = Box::leak(Box::new(AssetEntry {
            path,
            name: "bundle",
            id: "bundle",
            content_type: "application/x-kithara-hls-bundle",
            unavailable: None,
        }));
        let asset = Asset::on_disk(entry, &BYTES);

        let bundle = HlsBundle::try_from(&asset).expect("load bundle");
        let master = bundle.get(bundle.master_route()).expect("master resource");

        assert_eq!(master.path(), body);
        assert_eq!(master.content_type(), "application/vnd.apple.mpegurl");
        assert_eq!(bundle.iter().count(), 1);
    }
}
