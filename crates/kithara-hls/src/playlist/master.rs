#![forbid(unsafe_code)]

use kithara_assets::{AssetResource, AssetScope, ResourceKey};
use url::Url;

use super::{parse::ParsedMaster, playlist_cache::PlaylistCache};
use crate::{HlsResult, handle::ResourceHandle};

/// Loadable master playlist: a narrow `PlaylistCache` handle plus a
/// per-resource [`ResourceHandle`] for the master `.m3u8`. [`load`](Self::load)
/// folds the fetch + parse + dedup by delegating to
/// [`PlaylistCache::master_playlist`], so the `OnceCell` dedup and disk-cache
/// semantics stay byte-identical to the inline call it replaces.
pub(crate) struct MasterPlaylist {
    cache: PlaylistCache,
    resource: ResourceHandle,
}

impl MasterPlaylist {
    /// Build a loadable for the master at `url`, resolving the per-resource
    /// [`ResourceHandle`] from `scope`.
    pub(crate) fn new(cache: PlaylistCache, scope: &AssetScope, url: Url) -> HlsResult<Self> {
        let key = scope.key(&AssetResource::Url(url.clone()))?;
        let resource = ResourceHandle::new(scope.clone(), key, url);
        Ok(Self { cache, resource })
    }

    pub(crate) fn key(&self) -> &ResourceKey {
        self.resource.key()
    }

    /// Fetch + parse the master playlist (deduped + disk-cached via the
    /// `PlaylistCache`).
    ///
    /// # Errors
    /// Returns an error when fetching or parsing fails.
    pub(crate) async fn load(&self) -> HlsResult<ParsedMaster> {
        self.cache
            .master_playlist(self.resource.key(), self.resource.url())
            .await
    }
}
