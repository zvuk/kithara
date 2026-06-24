#![forbid(unsafe_code)]

use kithara_assets::AssetScope;
use kithara_drm::DecryptContext;
use url::Url;

use super::{
    parse::{MediaPlaylist, VariantId, VariantStream},
    playlist_cache::PlaylistCache,
};
use crate::{HlsResult, handle::ResourceHandle};

/// Loadable media playlist for one master variant: a narrow `PlaylistCache`
/// handle plus a per-resource [`ResourceHandle`] for that variant's media
/// `.m3u8`, tagged with its [`VariantId`]. [`load`](Self::load) delegates to
/// [`PlaylistCache::media_playlist`], preserving the per-variant `OnceCell`
/// dedup and disk-cache semantics.
pub(crate) struct VariantPlaylist {
    cache: PlaylistCache,
    resource: ResourceHandle,
    variant_id: VariantId,
}

impl VariantPlaylist {
    /// Build a loadable for `variant`, resolving its media URL against
    /// `master_url` through the cache's base-override-aware
    /// [`PlaylistCache::resolve_url`] and minting the per-resource handle from
    /// `scope`. Preserves `VariantId(variant.id.0)` exactly.
    ///
    /// # Errors
    /// Returns an error when the variant URL fails to resolve.
    pub(crate) fn for_variant(
        cache: &PlaylistCache,
        scope: &AssetScope<DecryptContext>,
        master_url: &Url,
        variant: &VariantStream,
    ) -> HlsResult<Self> {
        let media_url = cache.resolve_url(master_url, &variant.uri)?;
        let resource =
            ResourceHandle::new(scope.clone(), scope.key_from_url(&media_url), media_url);
        Ok(Self {
            cache: cache.clone(),
            resource,
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
            .media_playlist(self.resource.url(), self.variant_id)
            .await
    }
}

/// Load every variant's media playlist for `master`, in master order, by
/// constructing one [`VariantPlaylist`] per variant and loading it. Order,
/// per-variant `VariantId(variant.id.0)`, and `resolve_url` semantics match the
/// inline loop they replace.
///
/// # Errors
/// Returns an error when any variant URL fails to resolve or any media playlist
/// fails to fetch or parse.
pub(crate) async fn load_variant_playlists(
    cache: &PlaylistCache,
    scope: &AssetScope<DecryptContext>,
    master_url: &Url,
    variants: &[VariantStream],
) -> HlsResult<Vec<MediaPlaylist>> {
    let mut playlists = Vec::with_capacity(variants.len());
    for variant in variants {
        let loadable = VariantPlaylist::for_variant(cache, scope, master_url, variant)?;
        playlists.push(loadable.load().await?);
    }
    Ok(playlists)
}
