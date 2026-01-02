use url::Url;

use crate::error::HlsError;
use crate::parser::VariantId;

/// Represents different types of HLS resources with their URLs.
#[derive(Debug, Clone)]
pub enum Resource {
    /// Master playlist URL
    Master(Url),
    /// Media playlist (variant playlist) URL
    MediaPlaylist(Url, VariantId),
    /// Encryption key URL with optional variant ID
    Key(Url, VariantId),
    /// Init segment URL
    InitSegment(Url, VariantId),
    /// Media segment URL
    MediaSegment(Url, VariantId),
}

impl Resource {
    /// Get URL from Resource.
    pub fn url(&self) -> &Url {
        match self {
            Resource::Master(url)
            | Resource::MediaPlaylist(url, _)
            | Resource::Key(url, _)
            | Resource::InitSegment(url, _)
            | Resource::MediaSegment(url, _) => url,
        }
    }

    /// Create a Master resource from a string that can be parsed as a URL.
    pub fn master(url: impl AsRef<str>) -> Result<Self, HlsError> {
        Ok(Resource::Master(
            Url::parse(url.as_ref()).map_err(HlsError::url_parse)?,
        ))
    }

    /// Create a MediaPlaylist resource from a string that can be parsed as a URL.
    pub fn media_playlist(url: impl AsRef<str>, variant_id: VariantId) -> Result<Self, HlsError> {
        Ok(Resource::MediaPlaylist(
            Url::parse(url.as_ref()).map_err(HlsError::url_parse)?,
            variant_id,
        ))
    }

    /// Create a Key resource from a string that can be parsed as a URL.
    pub fn key(url: impl AsRef<str>, variant_id: VariantId) -> Result<Self, HlsError> {
        Ok(Resource::Key(
            Url::parse(url.as_ref()).map_err(HlsError::url_parse)?,
            variant_id,
        ))
    }

    /// Create an InitSegment resource from a string that can be parsed as a URL.
    pub fn init_segment(url: impl AsRef<str>, variant_id: VariantId) -> Result<Self, HlsError> {
        Ok(Resource::InitSegment(
            Url::parse(url.as_ref()).map_err(HlsError::url_parse)?,
            variant_id,
        ))
    }

    /// Create a MediaSegment resource from a string that can be parsed as a URL.
    pub fn media_segment(url: impl AsRef<str>, variant_id: VariantId) -> Result<Self, HlsError> {
        Ok(Resource::MediaSegment(
            Url::parse(url.as_ref()).map_err(HlsError::url_parse)?,
            variant_id,
        ))
    }
}
