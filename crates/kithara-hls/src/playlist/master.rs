#![forbid(unsafe_code)]

use kithara_assets::{AssetScope, ResourceInfo};
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
    pub(crate) fn new(cache: PlaylistCache, scope: &AssetScope, url: Url) -> Self {
        let key = scope.key_for(&ResourceInfo::Manifest {
            url: &url,
            rendition: None,
        });
        let resource = ResourceHandle::new(scope.clone(), key, url);
        Self { cache, resource }
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
