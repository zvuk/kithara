//! HLS playlist parsing and data types.

use std::time::Duration;

use hls_m3u8::{
    Decryptable, MasterPlaylist as HlsMasterPlaylist, MediaPlaylist as HlsMediaPlaylist,
    tags::VariantStream as HlsVariantStreamTag, types::DecryptionKey as HlsDecryptionKey,
};
use kithara_stream::AudioCodec;

use crate::HlsResult;

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
    /// Parsed audio codec from the CODECS string.
    pub audio_codec: Option<AudioCodec>,
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
    /// Container format detected from segment/init URIs.
    pub detected_container: Option<ContainerFormat>,
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

/// Detect container format from URI extension.
fn detect_container_from_uri(uri: &str) -> Option<ContainerFormat> {
    let path = uri.split('?').next().unwrap_or(uri);
    let ext = path.rsplit('.').next()?.to_lowercase();

    match ext.as_str() {
        "ts" | "m2ts" => Some(ContainerFormat::Ts),
        "mp4" | "m4s" | "m4a" | "m4v" => Some(ContainerFormat::Fmp4),
        _ => None,
    }
}

/// Parses a master playlist (M3U8) into [`MasterPlaylist`].
pub fn parse_master_playlist(data: &[u8]) -> HlsResult<MasterPlaylist> {
    let input =
        std::str::from_utf8(data).map_err(|e| crate::HlsError::PlaylistParse(e.to_string()))?;
    let hls_master = HlsMasterPlaylist::try_from(input)
        .map_err(|e| crate::HlsError::PlaylistParse(e.to_string()))?
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

            let codec = codecs_str.map(|c| {
                // Parse audio codec from CODECS string
                // CODECS can contain multiple codecs separated by comma (e.g., "avc1.42c00d,mp4a.40.2")
                let audio_codec = c
                    .split(',')
                    .map(|s| s.trim())
                    .find_map(AudioCodec::from_hls_codec);

                // Determine container format from URI extension
                let container = detect_container_from_uri(&uri);

                CodecInfo {
                    codecs: Some(c),
                    audio_codec,
                    container,
                }
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
    let input =
        std::str::from_utf8(data).map_err(|e| crate::HlsError::PlaylistParse(e.to_string()))?;
    let hls_media = HlsMediaPlaylist::try_from(input)
        .map_err(|e| crate::HlsError::PlaylistParse(e.to_string()))?
        .into_owned();

    // Treat `#EXT-X-ENDLIST` as the only reliable end-of-stream marker.
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
        .find_map(|seg| seg.keys().first().copied())
        .and_then(keyinfo_from_decryption_key);

    let segments: Vec<MediaSegment> = hls_media
        .segments
        .iter()
        .enumerate()
        .map(|(index, (_idx, seg))| {
            let seg_key: Option<SegmentKey> = seg
                .keys()
                .first()
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
                .first()
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

    // Detect container from init segment or first segment URI
    let detected_container = init_segment
        .as_ref()
        .and_then(|init| detect_container_from_uri(&init.uri))
        .or_else(|| {
            segments
                .first()
                .and_then(|seg| detect_container_from_uri(&seg.uri))
        });

    Ok(MediaPlaylist {
        segments,
        target_duration,
        init_segment,
        media_sequence,
        end_list,
        current_key,
        detected_container,
    })
}
