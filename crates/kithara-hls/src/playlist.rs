use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};

use hls_m3u8::{
    Decryptable, MasterPlaylist as HlsMasterPlaylist, MediaPlaylist as HlsMediaPlaylist,
    tags::VariantStream as HlsVariantStreamTag, types::DecryptionKey as HlsDecryptionKey,
};
use thiserror::Error;
use tokio::sync::OnceCell;
use url::Url;

use crate::{
    HlsError, HlsResult,
    abr::{Variant, variants_from_master},
    fetch::FetchManager,
};

fn uri_basename_no_query(uri: &str) -> Option<&str> {
    let no_query = uri.split('?').next().unwrap_or(uri);
    let base = no_query.rsplit('/').next().unwrap_or(no_query);
    if base.is_empty() { None } else { Some(base) }
}

#[derive(Debug, Error)]
pub enum PlaylistError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Assets error: {0}")]
    Assets(#[from] kithara_assets::AssetsError),

    #[error("Playlist parsing error: {0}")]
    Parse(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
}

/// Identifies a variant within a parsed master playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VariantId(pub usize);

/// Container format information (best-effort).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerFormat {
    /// MPEG-2 Transport Stream.
    Ts,
    /// Fragmented MP4.
    Fmp4,
    /// Any other format we don't explicitly handle yet.
    Other,
}

/// Codec/container information extracted from playlist attributes (best-effort).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecInfo {
    /// The raw `CODECS="..."` string from the playlist.
    pub codecs: Option<String>,
    /// A best-effort guess at the audio codec.
    pub audio_codec: Option<String>,
    /// A best-effort guess at the container format.
    pub container: Option<ContainerFormat>,
}

/// Supported HLS encryption methods (as parsed from playlists).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionMethod {
    /// No encryption.
    None,
    /// AES-128 CBC encryption of the whole segment.
    Aes128,
    /// Sample-based AES encryption.
    SampleAes,
    /// Any other method, stored as a raw string.
    Other(String),
}

/// A parsed `#EXT-X-KEY` (playlist-level metadata).
#[derive(Debug, Clone)]
pub struct KeyInfo {
    /// The encryption method to be used.
    pub method: EncryptionMethod,
    /// The URI of the encryption key. Can be relative to the playlist.
    pub uri: Option<String>,
    /// The initialization vector (IV), if specified.
    pub iv: Option<[u8; 16]>,
    /// The key format, e.g., "identity".
    pub key_format: Option<String>,
    /// The key format version(s).
    pub key_format_versions: Option<String>,
}

/// The effective encryption key for a specific segment.
#[derive(Debug, Clone)]
pub struct SegmentKey {
    /// The encryption method that applies to this segment.
    pub method: EncryptionMethod,
    /// Full key information (if available).
    pub key_info: Option<KeyInfo>,
}

/// Parsed master playlist.
#[derive(Debug, Clone)]
pub struct MasterPlaylist {
    /// List of available variants (renditions).
    pub variants: Vec<VariantStream>,
}

/// One variant stream entry from a master playlist.
#[derive(Debug, Clone)]
pub struct VariantStream {
    /// Variant identifier (stable for this parsed master playlist).
    pub id: VariantId,
    /// Absolute or relative URL of the media playlist for this variant.
    pub uri: String,
    /// Optional advertised bandwidth in bits per second.
    pub bandwidth: Option<u64>,
    /// Optional human-readable name.
    pub name: Option<String>,
    /// Codec and format information for this variant.
    pub codec: Option<CodecInfo>,
}

/// Parsed init segment information (for fMP4 streams).
#[derive(Debug, Clone)]
pub struct InitSegment {
    /// URL of the initialization segment (absolute or relative to playlist URI).
    pub uri: String,
    /// Optional encryption information effective for this init segment.
    pub key: Option<SegmentKey>,
}

/// Parsed media playlist.
#[derive(Debug, Clone)]
pub struct MediaPlaylist {
    /// List of segments in the order they appear.
    pub segments: Vec<MediaSegment>,
    /// Target segment duration if present.
    pub target_duration: Option<Duration>,
    /// Optional initialization segment (for fMP4 streams).
    pub init_segment: Option<InitSegment>,
    /// Media sequence number of the first segment.
    pub media_sequence: u64,
    /// Whether the playlist is finished (VOD or live that ended).
    pub end_list: bool,
    /// Informational: first key found in the playlist (if any).
    pub current_key: Option<KeyInfo>,
}

/// One media segment entry.
#[derive(Debug, Clone)]
pub struct MediaSegment {
    /// Sequence number of the segment (media-sequence + index in playlist).
    pub sequence: u64,
    /// The variant this segment belongs to.
    pub variant_id: VariantId,
    /// URL of the segment (absolute or relative to playlist URI).
    pub uri: String,
    /// Duration of the segment if known.
    pub duration: Duration,
    /// Optional encryption information effective for this segment.
    pub key: Option<SegmentKey>,
}

/// Thin wrapper: fetches + parses playlists with caching via `kithara-assets`.
#[derive(Clone)]
pub struct PlaylistManager {
    fetch: Arc<FetchManager>,
    base_url: Option<Url>,
    master: Arc<OnceCell<MasterPlaylist>>,
    variants: Arc<OnceCell<Vec<Variant>>>,
    media: Arc<RwLock<HashMap<VariantId, Arc<OnceCell<MediaPlaylist>>>>>,
}

impl PlaylistManager {
    pub fn new(fetch: Arc<FetchManager>, base_url: Option<Url>) -> Self {
        Self {
            fetch,
            base_url,
            master: Arc::new(OnceCell::new()),
            variants: Arc::new(OnceCell::new()),
            media: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn master_playlist(&self, url: &Url) -> HlsResult<MasterPlaylist> {
        let master = self
            .master
            .get_or_try_init(|| async {
                self.fetch_and_parse(url, "master_playlist", parse_master_playlist)
                    .await
            })
            .await?;

        Ok(master.clone())
    }

    pub async fn variants(&self, url: &Url) -> HlsResult<Vec<Variant>> {
        if let Some(cached) = self.variants.get() {
            return Ok(cached.clone());
        }

        let computed = {
            let master = self.master_playlist(url).await?;
            variants_from_master(&master)
        };

        let _ = self.variants.set(computed.clone());
        Ok(computed)
    }

    pub fn master_variants(&self) -> Option<Vec<VariantStream>> {
        self.master.get().map(|m| m.variants.clone())
    }

    pub async fn media_playlist(
        &self,
        url: &Url,
        variant_id: VariantId,
    ) -> HlsResult<MediaPlaylist> {
        let cell = {
            let mut guard = self
                .media
                .write()
                .map_err(|_| HlsError::PlaylistParse("playlist cache poisoned".to_string()))?;
            guard
                .entry(variant_id)
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        let playlist = cell
            .get_or_try_init(|| async {
                self.fetch_and_parse(url, "media_playlist", |bytes| {
                    parse_media_playlist(bytes, variant_id)
                })
                .await
            })
            .await?;

        Ok(playlist.clone())
    }

    pub fn resolve_url(&self, base: &Url, target: &str) -> HlsResult<Url> {
        let resolved = if let Some(ref base_url) = self.base_url {
            base_url.join(target).map_err(|e| {
                HlsError::InvalidUrl(format!("Failed to resolve URL with base override: {e}"))
            })?
        } else {
            base.join(target)
                .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve URL: {e}")))?
        };

        Ok(resolved)
    }

    async fn fetch_and_parse<T, F>(&self, url: &Url, label: &str, parse: F) -> HlsResult<T>
    where
        F: Fn(&[u8]) -> HlsResult<T>,
    {
        let basename = uri_basename_no_query(url.as_str())
            .ok_or_else(|| HlsError::InvalidUrl(format!("Failed to derive {label} basename")))?;
        let bytes = Arc::clone(&self.fetch)
            .fetch_playlist_atomic(url, basename)
            .await?;

        parse(&bytes)
    }
}

/// Parses a master playlist (M3U8) into [`MasterPlaylist`].
pub fn parse_master_playlist(data: &[u8]) -> HlsResult<MasterPlaylist> {
    let input = std::str::from_utf8(data).map_err(|e| HlsError::PlaylistParse(e.to_string()))?;
    let hls_master = HlsMasterPlaylist::try_from(input)
        .map_err(|e| HlsError::PlaylistParse(e.to_string()))?
        .into_owned();

    let variants = hls_master
        .variant_streams
        .iter()
        .enumerate()
        .map(|(index, vs)| {
            let (uri, bandwidth, codecs_str) = match vs {
                HlsVariantStreamTag::ExtXStreamInf {
                    uri, stream_data, ..
                } => {
                    let bw = stream_data.bandwidth();
                    let codecs = stream_data.codecs().map(|c| c.to_string());
                    (uri.to_string(), Some(bw), codecs)
                }
                HlsVariantStreamTag::ExtXIFrame { uri, stream_data } => {
                    let bw = stream_data.bandwidth();
                    let codecs = stream_data.codecs().map(|c| c.to_string());
                    (uri.to_string(), Some(bw), codecs)
                }
            };

            let codec = codecs_str.map(|c| CodecInfo {
                codecs: Some(c),
                audio_codec: None,
                container: None,
            });

            VariantStream {
                id: VariantId(index),
                uri,
                bandwidth,
                name: None,
                codec,
            }
        })
        .collect();

    Ok(MasterPlaylist { variants })
}

/// Parses a media playlist (M3U8) into [`MediaPlaylist`].
pub fn parse_media_playlist(data: &[u8], variant_id: VariantId) -> HlsResult<MediaPlaylist> {
    let input = std::str::from_utf8(data).map_err(|e| HlsError::PlaylistParse(e.to_string()))?;
    let hls_media = HlsMediaPlaylist::try_from(input)
        .map_err(|e| HlsError::PlaylistParse(e.to_string()))?
        .into_owned();

    // Treat `#EXT-X-ENDLIST` as the only reliable end-of-stream marker.
    // Some servers set Playlist-Type=VOD or EVENT without a terminal ENDLIST.
    let end_list = input.contains("#EXT-X-ENDLIST");
    let target_duration = Some(hls_media.target_duration);
    let media_sequence = hls_media.media_sequence as u64;

    fn map_encryption_method(m: &hls_m3u8::types::EncryptionMethod) -> EncryptionMethod {
        match m {
            hls_m3u8::types::EncryptionMethod::Aes128 => EncryptionMethod::Aes128,
            hls_m3u8::types::EncryptionMethod::SampleAes => EncryptionMethod::SampleAes,
            other => EncryptionMethod::Other(other.to_string()),
        }
    }

    fn keyinfo_from_decryption_key(k: &HlsDecryptionKey<'_>) -> Option<KeyInfo> {
        let method = map_encryption_method(&k.method);

        let uri = k.uri().trim();
        if uri.is_empty() {
            return None;
        }

        Some(KeyInfo {
            method,
            uri: Some(uri.to_string()),
            iv: k.iv.to_slice(),
            key_format: k.format.as_ref().map(|s| s.to_string()),
            key_format_versions: k.versions.as_ref().map(|s| s.to_string()),
        })
    }

    let current_key: Option<KeyInfo> = hls_media
        .segments
        .values()
        .find_map(|seg| seg.keys().get(0).copied())
        .and_then(keyinfo_from_decryption_key);

    let segments = hls_media
        .segments
        .iter()
        .enumerate()
        .map(|(index, (_idx, seg))| {
            let seg_key: Option<SegmentKey> = seg
                .keys()
                .get(0)
                .copied()
                .and_then(keyinfo_from_decryption_key)
                .map(|ki| SegmentKey {
                    method: ki.method.clone(),
                    key_info: Some(ki),
                });

            MediaSegment {
                sequence: media_sequence + index as u64,
                variant_id,
                uri: seg.uri().to_string(),
                duration: seg.duration.duration(),
                key: seg_key,
            }
        })
        .collect();

    let init_segment = hls_media.segments.iter().next().and_then(|(_, seg)| {
        seg.map.as_ref().map(|m| {
            let map_key: Option<SegmentKey> = m
                .keys()
                .get(0)
                .copied()
                .and_then(keyinfo_from_decryption_key)
                .map(|ki| SegmentKey {
                    method: ki.method.clone(),
                    key_info: Some(ki),
                });

            InitSegment {
                uri: m.uri().to_string(),
                key: map_key,
            }
        })
    });

    Ok(MediaPlaylist {
        segments,
        target_duration,
        init_segment,
        media_sequence,
        end_list,
        current_key,
    })
}
