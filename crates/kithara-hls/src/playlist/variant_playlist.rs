#![forbid(unsafe_code)]

use kithara_assets::{AssetResource, AssetScope, ResourceKey};
use url::Url;

use super::{
    parse::{MediaPlaylist, VariantId, VariantStream},
    playlist_cache::PlaylistCache,
};
use crate::HlsResult;

pub(crate) struct VariantPlaylist {
    cache: PlaylistCache,
    key: ResourceKey,
    url: Url,
    variant_id: VariantId,
}

impl VariantPlaylist {
    pub(crate) fn for_variant(
        cache: &PlaylistCache,
        scope: &AssetScope,
        master_url: &Url,
        master_key: &ResourceKey,
        variant: &VariantStream,
    ) -> HlsResult<Self> {
        let media_url = cache.resolve_url(master_url, &variant.uri)?;
        // A single-rendition master can double as the media playlist: Reuse the master key so
        // both point at one cache entry instead of minting a second.
        let key = if &media_url == master_url {
            master_key.clone()
        } else {
            scope.key(&AssetResource::Url(media_url.clone()))?
        };
        Ok(Self {
            key,
            cache: cache.clone(),
            url: media_url,
            variant_id: VariantId(variant.id.0),
        })
    }

    /// Fetch + parse this variant's media playlist (deduped + disk-cached via
    /// the `PlaylistCache`).
    ///
    /// # Errors
    /// Returns an error when fetching or parsing fails.
    pub(crate) async fn load(&self) -> HlsResult<MediaPlaylist> {
        self.cache
            .media_playlist(&self.key, &self.url, self.variant_id)
            .await
    }
}

/// Load every variant's media playlist for `master`, in master order, by
/// constructing one [`VariantPlaylist`] per variant and loading variants
/// concurrently via `try_join_all`. `try_join_all` preserves input order, so
/// the returned playlists stay in master order.
///
/// # Errors
/// Returns an error when any variant URL fails to resolve or any media playlist
/// fails to fetch or parse.
pub(crate) async fn load_variant_playlists(
    cache: &PlaylistCache,
    scope: &AssetScope,
    master_url: &Url,
    master_key: &ResourceKey,
    variants: &[VariantStream],
) -> HlsResult<Vec<MediaPlaylist>> {
    let loadables = variants
        .iter()
        .map(|variant| VariantPlaylist::for_variant(cache, scope, master_url, master_key, variant))
        .collect::<HlsResult<Vec<_>>>()?;
    futures::future::try_join_all(loadables.iter().map(VariantPlaylist::load)).await
}
