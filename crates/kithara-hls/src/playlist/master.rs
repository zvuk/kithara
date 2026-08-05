#![forbid(unsafe_code)]

use kithara_assets::{AssetResource, AssetScope, ResourceKey};
use url::Url;

use super::{parse::ParsedMaster, playlist_cache::PlaylistCache};
use crate::HlsResult;

/// Loadable master playlist: a narrow `PlaylistCache` handle plus the master
/// `.m3u8`'s cache identity. [`load`](Self::load) folds the fetch + parse +
/// dedup by delegating to [`PlaylistCache::master_playlist`], so the
/// `OnceCell` dedup and disk-cache semantics stay byte-identical to the inline
/// call it replaces.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct MasterPlaylist {
    cache: PlaylistCache,
    #[field(get, vis = "pub(crate)")]
    key: ResourceKey,
    url: Url,
}

impl MasterPlaylist {
    /// Build a loadable for the master at `url`, resolving its cache key from
    /// `scope`.
    pub(crate) fn new(cache: PlaylistCache, scope: &AssetScope, url: Url) -> HlsResult<Self> {
        let key = scope.key(&AssetResource::Url(url.clone()))?;
        Ok(Self { cache, key, url })
    }

    /// Fetch + parse the master playlist (deduped + disk-cached via the
    /// `PlaylistCache`).
    ///
    /// # Errors
    /// Returns an error when fetching or parsing fails.
    pub(crate) async fn load(&self) -> HlsResult<ParsedMaster> {
        self.cache.master_playlist(&self.key, &self.url).await
    }
}
