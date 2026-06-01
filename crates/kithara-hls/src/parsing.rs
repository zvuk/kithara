use std::str;

use hls_m3u8::{
    Decryptable, MasterPlaylist as HlsMasterPlaylist, MediaPlaylist as HlsMediaPlaylist,
    tags::VariantStream as HlsVariantStreamTag,
    types::{DecryptionKey as HlsDecryptionKey, EncryptionMethod as HlsEncryptionMethod},
};
use kithara_abr::VariantInfo;
use kithara_platform::time::Duration;
use kithara_stream::{AudioCodec, ContainerFormat};
use url::Url;

use crate::HlsResult;

/// Cap error messages to avoid dumping binary data into logs.
fn truncate_error(msg: &str) -> String {
    const MAX_LEN: usize = 200;
    if msg.len() <= MAX_LEN {
        return msg.to_string();
    }
    let truncated = &msg[..msg.floor_char_boundary(MAX_LEN)];
    format!("{truncated}… ({} bytes total)", msg.len())
}

/// AES initialization vector length in bytes.
const IV_LEN: usize = 16;

/// Identifies a variant within a parsed master playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VariantId(pub usize);

/// Codec/container information extracted from playlist attributes (best-effort).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecInfo {
    /// Parsed audio codec from the CODECS string.
    pub audio_codec: Option<AudioCodec>,
    /// The raw `CODECS="..."` string from the playlist.
    pub codecs: Option<String>,
    /// A best-effort guess at the container format.
    pub container: Option<ContainerFormat>,
}

/// Supported HLS encryption methods (as parsed from playlists).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionMethod {
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
    /// The initialization vector (IV), if specified.
    pub iv: Option<[u8; IV_LEN]>,
    /// The URI of the encryption key. Can be relative to the playlist.
    pub uri: Option<String>,
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
    /// Optional advertised bandwidth in bits per second.
    pub bandwidth: Option<u64>,
    /// Codec and format information for this variant.
    pub codec: Option<CodecInfo>,
    /// Optional human-readable name.
    pub name: Option<String>,
    /// Absolute or relative URL of the media playlist for this variant.
    pub uri: String,
    /// Variant identifier (stable for this parsed master playlist).
    pub id: VariantId,
}

/// Parsed init segment information (for fMP4 streams).
#[derive(Debug, Clone)]
pub struct InitSegment {
    /// Optional encryption information effective for this init segment.
    pub key: Option<SegmentKey>,
    /// URL of the initialization segment (absolute or relative to playlist URI).
    pub uri: String,
}

/// Parsed media playlist.
#[derive(Debug, Clone)]
pub struct MediaPlaylist {
    /// Container format detected from segment/init URIs.
    pub detected_container: Option<ContainerFormat>,
    /// Optional initialization segment (for fMP4 streams).
    pub init_segment: Option<InitSegment>,
    /// Absolute URL of the media playlist itself (the `.m3u8`).
    pub url: Url,
    /// List of segments in the order they appear.
    pub segments: Vec<MediaSegment>,
}

/// One media segment entry.
#[derive(Debug, Clone)]
pub struct MediaSegment {
    /// Duration of the segment if known.
    pub duration: Duration,
    /// Byte length from `#EXT-X-BYTERANGE` if present in the playlist.
    pub byte_range_len: Option<u64>,
    /// Optional encryption information effective for this segment.
    pub key: Option<SegmentKey>,
    /// URL of the segment (absolute or relative to playlist URI).
    pub uri: String,
    /// Sequence number of the segment (media-sequence + index in playlist).
    pub sequence: u64,
}

/// Detect container format from URI extension.
fn detect_container_from_uri(uri: &str) -> Option<ContainerFormat> {
    let path = uri.split('?').next().unwrap_or(uri);
    let ext = path.rsplit('.').next()?.to_lowercase();

    match ext.as_str() {
        "ts" | "m2ts" => Some(ContainerFormat::MpegTs),
        "mp4" | "m4s" | "m4a" | "m4v" => Some(ContainerFormat::Fmp4),
        "wav" => Some(ContainerFormat::Wav),
        _ => None,
    }
}

/// Container inferred from a CODECS attribute string. HLS / RFC 6381
/// have no standard fourcc for raw RIFF WAV, so the recognised tokens
/// here are deliberately conservative — `wav` / `pcm` / `lpcm` cover
/// internal byte-continuity fixtures that ship PCM payloads under
/// arbitrary URI extensions.
fn detect_container_from_codecs(codecs: &str) -> Option<ContainerFormat> {
    let normalised = codecs.to_lowercase();
    normalised
        .split(',')
        .map(|c| c.trim().trim_matches('"'))
        .find_map(|token| match token {
            "wav" | "pcm" | "lpcm" => Some(ContainerFormat::Wav),
            _ => None,
        })
}

/// Parses a master playlist (M3U8) into [`MasterPlaylist`].
///
/// # Errors
/// Returns an error when UTF-8 decoding or playlist parsing fails.
pub fn parse_master_playlist(data: &[u8]) -> HlsResult<MasterPlaylist> {
    let input = str::from_utf8(data).map_err(|e| crate::HlsError::PlaylistParse(e.to_string()))?;
    let hls_master = HlsMasterPlaylist::try_from(input)
        .map_err(|e| crate::HlsError::PlaylistParse(truncate_error(&e.to_string())))?
        .into_owned();

    let variants = hls_master
        .variant_streams
        .iter()
        .enumerate()
        .map(|(index, vs)| {
            let (uri, bandwidth, codecs_str) = match vs {
                HlsVariantStreamTag::ExtXStreamInf {
                    uri, stream_data, ..
                }
                | HlsVariantStreamTag::ExtXIFrame { uri, stream_data } => {
                    let bw = stream_data.bandwidth();
                    let codecs = stream_data.codecs().map(ToString::to_string);
                    (uri.to_string(), Some(bw), codecs)
                }
            };

            let codec = codecs_str.map(|c| {
                let audio_codec = c
                    .split(',')
                    .map(str::trim)
                    .map(|codec| codec.trim_matches('"'))
                    .find_map(AudioCodec::parse_hls_codec);

                let container =
                    detect_container_from_codecs(&c).or_else(|| detect_container_from_uri(&uri));

                CodecInfo {
                    audio_codec,
                    container,
                    codecs: Some(c),
                }
            });

            VariantStream {
                uri,
                bandwidth,
                codec,
                id: VariantId(index),
                name: None,
            }
        })
        .collect();

    Ok(MasterPlaylist { variants })
}

/// Parses a media playlist (M3U8) into [`MediaPlaylist`]. `url` is the
/// absolute URL the playlist was fetched from; it is stored on the
/// returned [`MediaPlaylist`] so downstream consumers do not have to
/// keep it in a side channel.
///
/// # Errors
/// Returns an error when UTF-8 decoding or playlist parsing fails.
pub fn parse_media_playlist(url: Url, data: &[u8]) -> HlsResult<MediaPlaylist> {
    fn map_encryption_method(m: HlsEncryptionMethod) -> EncryptionMethod {
        match m {
            HlsEncryptionMethod::Aes128 => EncryptionMethod::Aes128,
            HlsEncryptionMethod::SampleAes => EncryptionMethod::SampleAes,
            other => EncryptionMethod::Other(other.to_string()),
        }
    }

    fn keyinfo_from_decryption_key(k: &HlsDecryptionKey<'_>) -> Option<KeyInfo> {
        let method = map_encryption_method(k.method);

        let uri = k.uri().trim();
        if uri.is_empty() {
            return None;
        }

        Some(KeyInfo {
            method,
            uri: Some(uri.to_string()),
            iv: k.iv.to_slice(),
        })
    }

    let input = str::from_utf8(data).map_err(|e| crate::HlsError::PlaylistParse(e.to_string()))?;
    let hls_media = HlsMediaPlaylist::try_from(input)
        .map_err(|e| crate::HlsError::PlaylistParse(truncate_error(&e.to_string())))?
        .into_owned();

    let media_sequence = hls_media.media_sequence as u64;

    let init_key_fallback: Option<KeyInfo> = hls_media
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
                uri: seg.uri().to_string(),
                duration: seg.duration.duration(),
                key: seg_key,
                byte_range_len: seg.byte_range.as_ref().map(|br| br.len() as u64),
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
                .or_else(|| init_key_fallback.clone())
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

    let detected_container = init_segment
        .as_ref()
        .and_then(|init| detect_container_from_uri(&init.uri))
        .or_else(|| {
            segments
                .first()
                .and_then(|seg| detect_container_from_uri(&seg.uri))
        });

    Ok(MediaPlaylist {
        detected_container,
        init_segment,
        url,
        segments,
    })
}

/// Extract variant metadata from master playlist + parsed media playlists.
///
/// `media_playlists` must align with `master.variants` by index. Each
/// produced [`VariantInfo`] carries duration shape derived from the
/// matching media playlist's segments — `Segmented(per-segment)` when
/// segments are present, otherwise `Unknown`.
#[must_use]
pub fn variant_info_from_master(
    master: &MasterPlaylist,
    media_playlists: &[MediaPlaylist],
) -> Vec<VariantInfo> {
    master
        .variants
        .iter()
        .enumerate()
        .map(|(idx, v)| {
            let duration = media_playlists.get(idx).map_or(
                kithara_events::VariantDuration::Unknown,
                |playlist| {
                    if playlist.segments.is_empty() {
                        kithara_events::VariantDuration::Unknown
                    } else {
                        kithara_events::VariantDuration::Segmented(
                            playlist.segments.iter().map(|s| s.duration).collect(),
                        )
                    }
                },
            );
            VariantInfo {
                duration,
                variant_index: kithara_events::VariantIndex::new(v.id.0),
                bandwidth_bps: v.bandwidth,
                name: v.name.clone(),
                codecs: v.codec.as_ref().and_then(|c| c.codecs.clone()),
                container: v
                    .codec
                    .as_ref()
                    .and_then(|c| c.container)
                    .map(|fmt| format!("{fmt:?}")),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::HlsError;

    /// Static playlist fixtures used by the test cases below. Grouped in a
    /// nested module to avoid `style.multiple-private-module-consts`
    /// (>2 free private consts) and `style.no-impl-only-consts` (consts-
    /// only inherent impl) rules.
    mod fixtures {
        pub(super) const SIMPLE_MASTER_PLAYLIST: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"mp4a.40.2\"
audio.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2000000,CODECS=\"mp4a.40.2\"
audio_high.m3u8";

        pub(super) const SIMPLE_MEDIA_PLAYLIST: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXTINF:4.0,
segment0.ts
#EXTINF:4.0,
segment1.ts
#EXT-X-ENDLIST";

        pub(super) const MEDIA_PLAYLIST_WITH_INIT: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-MAP:URI=\"init.mp4\"
#EXTINF:4.0,
segment0.m4s
#EXT-X-ENDLIST";

        pub(super) const INVALID_PLAYLIST: &[u8] = b"NOT A VALID PLAYLIST";

        pub(super) const EMPTY_MASTER_PLAYLIST: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6";

        pub(super) const MASTER_PLAYLIST_WITH_CODEC: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"mp4a.40.2,avc1.64001f\",RESOLUTION=1280x720
video.m3u8";

        pub(super) const MASTER_PLAYLIST_WITH_MIXED_CASE_FLAC_CODEC: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"fLaC\"
audio_flac.m3u8";
    }

    #[kithara::test]
    fn test_variant_id_creation() {
        assert_eq!(VariantId(42).0, 42);
    }

    #[kithara::test]
    fn test_parse_simple_master_playlist() {
        let simple_master_playlist_data = fixtures::SIMPLE_MASTER_PLAYLIST;
        let result = parse_master_playlist(simple_master_playlist_data);
        assert!(
            result.is_ok(),
            "Failed to parse master playlist: {:?}",
            result.err()
        );

        let master = result.unwrap();
        assert_eq!(master.variants.len(), 2);

        assert_eq!(master.variants[0].id.0, 0);
        assert_eq!(master.variants[0].uri, "audio.m3u8");
        assert_eq!(master.variants[0].bandwidth, Some(1000000));

        assert_eq!(master.variants[1].id.0, 1);
        assert_eq!(master.variants[1].uri, "audio_high.m3u8");
        assert_eq!(master.variants[1].bandwidth, Some(2000000));
    }

    #[kithara::test]
    fn test_parse_simple_media_playlist() {
        let simple_media_playlist_data = fixtures::SIMPLE_MEDIA_PLAYLIST;
        let url: Url = "http://example.com/v0.m3u8".parse().expect("test url");
        let result = parse_media_playlist(url, simple_media_playlist_data);
        assert!(
            result.is_ok(),
            "Failed to parse media playlist: {:?}",
            result.err()
        );

        let media = result.unwrap();
        assert_eq!(media.segments.len(), 2);

        assert_eq!(media.segments[0].uri, "segment0.ts");
        assert_eq!(media.segments[0].sequence, 0);

        assert_eq!(media.segments[1].uri, "segment1.ts");
        assert_eq!(media.segments[1].sequence, 1);
    }

    #[kithara::test]
    fn test_parse_media_playlist_with_init_segment() {
        let media_playlist_with_init_data = fixtures::MEDIA_PLAYLIST_WITH_INIT;
        let url: Url = "http://example.com/v0.m3u8".parse().expect("test url");
        let result = parse_media_playlist(url, media_playlist_with_init_data);
        assert!(
            result.is_ok(),
            "Failed to parse media playlist with init: {:?}",
            result.err()
        );

        let media = result.unwrap();
        assert_eq!(media.segments.len(), 1);
        assert!(media.init_segment.is_some());

        let init = media.init_segment.unwrap();
        assert_eq!(init.uri, "init.mp4");
    }

    #[kithara::test]
    fn test_parse_invalid_playlist() {
        let invalid_playlist_data = fixtures::INVALID_PLAYLIST;
        let result = parse_master_playlist(invalid_playlist_data);
        assert!(result.is_err(), "Should fail to parse invalid playlist");

        if let Err(HlsError::PlaylistParse(_)) = result {
        } else {
            panic!("Expected PlaylistParse error, got: {:?}", result);
        }
    }

    #[kithara::test]
    fn test_empty_master_playlist() {
        let empty_master_playlist_data = fixtures::EMPTY_MASTER_PLAYLIST;
        let result = parse_master_playlist(empty_master_playlist_data);
        assert!(result.is_ok(), "Empty master playlist should parse");

        let master = result.unwrap();
        assert_eq!(master.variants.len(), 0);
    }

    #[kithara::test]
    fn test_master_playlist_with_codec_info() {
        let master_playlist_with_codec_data = fixtures::MASTER_PLAYLIST_WITH_CODEC;
        let result = parse_master_playlist(master_playlist_with_codec_data);
        assert!(result.is_ok());

        let master = result.unwrap();
        assert_eq!(master.variants.len(), 1);

        let variant = &master.variants[0];
        assert!(variant.codec.is_some());

        let codec = variant.codec.as_ref().unwrap();
        let codecs_str = codec.codecs.as_deref().unwrap_or("");
        assert!(codecs_str.contains("mp4a.40.2"));
        assert!(codecs_str.contains("avc1.64001f"));
        assert_eq!(codec.audio_codec, Some(AudioCodec::AacLc));
    }

    #[kithara::test]
    fn test_master_playlist_with_mixed_case_flac_codec() {
        let master_playlist_with_mixed_case_flac_codec_data =
            fixtures::MASTER_PLAYLIST_WITH_MIXED_CASE_FLAC_CODEC;
        let result = parse_master_playlist(master_playlist_with_mixed_case_flac_codec_data);
        assert!(result.is_ok());

        let master = result.unwrap();
        assert_eq!(master.variants.len(), 1);

        let variant = &master.variants[0];
        let codec = variant.codec.as_ref().expect("codec info");
        assert_eq!(codec.audio_codec, Some(AudioCodec::Flac));
    }

    #[kithara::test]
    #[case(HlsError::PlaylistParse("test error".to_string()), "test error")]
    #[case(HlsError::VariantNotFound("variant 0".to_string()), "variant 0")]
    #[case(HlsError::InvalidUrl("bad url".to_string()), "bad url")]
    fn test_error_types(#[case] error: HlsError, #[case] expected_substring: &str) {
        let error_str = error.to_string();
        assert!(
            error_str.contains(expected_substring),
            "Error string '{}' should contain '{}'",
            error_str,
            expected_substring
        );
    }

    #[kithara::test]
    #[case(0u32, "test.m3u8", Some(1000000), Some("Test Variant".to_string()))]
    #[case(1u32, "audio.m3u8", Some(2000000), None)]
    #[case(2u32, "video.m3u8", None, Some("Video Only".to_string()))]
    fn test_playlist_struct_debug_variant(
        #[case] id: u32,
        #[case] uri: &str,
        #[case] bandwidth: Option<u64>,
        #[case] name: Option<String>,
    ) {
        let variant = VariantStream {
            bandwidth,
            id: VariantId(id as usize),
            uri: uri.to_string(),
            name: name.clone(),
            codec: None,
        };

        let debug_output = format!("{:?}", variant);
        assert!(debug_output.contains("VariantStream"));
        assert!(debug_output.contains(uri));

        if let Some(name_str) = name {
            assert!(debug_output.contains(&name_str));
        }
    }

    #[kithara::test]
    #[case(0u64, "segment.ts", 4.0)]
    #[case(1u64, "segment1.m4s", 6.0)]
    #[case(2u64, "chunk.ts", 2.5)]
    fn test_playlist_struct_debug_segment(
        #[case] sequence: u64,
        #[case] uri: &str,
        #[case] duration_secs: f64,
    ) {
        let segment = MediaSegment {
            sequence,
            uri: uri.to_string(),
            duration: Duration::from_secs_f64(duration_secs),
            byte_range_len: None,
            key: None,
        };

        let debug_output = format!("{:?}", segment);
        assert!(debug_output.contains("MediaSegment"));
        assert!(debug_output.contains(uri));
    }

    #[kithara::test]
    #[case(
        b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-STREAM-INF:BANDWIDTH=500000\nlow.m3u8",
        1
    )]
    #[case(b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nmid.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000000\nhigh.m3u8", 2)]
    #[case(b"#EXTM3U\n#EXT-X-VERSION:6", 0)]
    fn test_master_playlist_variant_count(#[case] data: &[u8], #[case] expected_count: usize) {
        let result = parse_master_playlist(data);
        assert!(
            result.is_ok(),
            "Failed to parse playlist: {:?}",
            result.err()
        );

        let master = result.unwrap();
        assert_eq!(master.variants.len(), expected_count);
    }

    #[kithara::test]
    #[case(
        b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg1.ts\n#EXT-X-ENDLIST",
        1
    )]
    #[case(b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg1.ts\n#EXTINF:4.0,\nseg2.ts", 2)]
    #[case(b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg1.ts\n#EXTINF:4.0,\nseg2.ts\n#EXTINF:4.0,\nseg3.ts\n#EXT-X-ENDLIST", 3)]
    fn test_media_playlist_segment_count(#[case] data: &[u8], #[case] expected_segments: usize) {
        let url: Url = "http://example.com/v0.m3u8".parse().expect("test url");
        let result = parse_media_playlist(url, data);
        assert!(
            result.is_ok(),
            "Failed to parse playlist: {:?}",
            result.err()
        );

        let media = result.unwrap();
        assert_eq!(media.segments.len(), expected_segments);
    }
}
