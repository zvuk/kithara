//! HLS playlist parsing and data types.

use std::time::Duration;

use hls_m3u8::{
    Decryptable, MasterPlaylist as HlsMasterPlaylist, MediaPlaylist as HlsMediaPlaylist,
    tags::VariantStream as HlsVariantStreamTag, types::DecryptionKey as HlsDecryptionKey,
};
use kithara_abr::VariantInfo;
use kithara_stream::{AudioCodec, ContainerFormat};

use crate::HlsResult;

/// Identifies a variant within a parsed master playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VariantId(pub usize);

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
    /// Whether the playlist allows caching.
    ///
    /// `false` when the playlist contains `#EXT-X-ALLOW-CACHE:NO` (HLS v3, deprecated in v7).
    pub allow_cache: bool,
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
        "ts" | "m2ts" => Some(ContainerFormat::MpegTs),
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
                }
                | HlsVariantStreamTag::ExtXIFrame { uri, stream_data } => {
                    let bw = stream_data.bandwidth();
                    let codecs = stream_data.codecs().map(ToString::to_string);
                    (uri.to_string(), Some(bw), codecs)
                }
            };

            let codec = codecs_str.map(|c| {
                // Parse audio codec from CODECS string
                // CODECS can contain multiple codecs separated by comma (e.g., "avc1.42c00d,mp4a.40.2")
                let audio_codec = c
                    .split(',')
                    .map(str::trim)
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
    fn map_encryption_method(m: hls_m3u8::types::EncryptionMethod) -> EncryptionMethod {
        match m {
            hls_m3u8::types::EncryptionMethod::Aes128 => EncryptionMethod::Aes128,
            hls_m3u8::types::EncryptionMethod::SampleAes => EncryptionMethod::SampleAes,
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
            key_format: k.format.as_ref().map(ToString::to_string),
            key_format_versions: k.versions.as_ref().map(ToString::to_string),
        })
    }

    let input =
        std::str::from_utf8(data).map_err(|e| crate::HlsError::PlaylistParse(e.to_string()))?;
    let hls_media = HlsMediaPlaylist::try_from(input)
        .map_err(|e| crate::HlsError::PlaylistParse(e.to_string()))?
        .into_owned();

    // Treat `#EXT-X-ENDLIST` as the only reliable end-of-stream marker.
    let end_list = input.contains("#EXT-X-ENDLIST");

    // HLS v3 `#EXT-X-ALLOW-CACHE:NO` (deprecated in v7, but still used by servers).
    let allow_cache = !input.contains("#EXT-X-ALLOW-CACHE:NO");
    let target_duration = Some(hls_media.target_duration);
    let media_sequence = hls_media.media_sequence as u64;

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
            // hls_m3u8 may not associate #EXT-X-KEY with #EXT-X-MAP tags.
            // Fall back to the playlist's current_key (the effective key at
            // the point where the MAP tag appears).
            let map_key: Option<SegmentKey> = m
                .keys()
                .first()
                .copied()
                .and_then(keyinfo_from_decryption_key)
                .or_else(|| current_key.clone())
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
        allow_cache,
    })
}

/// Extract extended variant metadata from master playlist.
#[must_use]
pub fn variant_info_from_master(master: &MasterPlaylist) -> Vec<VariantInfo> {
    master
        .variants
        .iter()
        .map(|v| VariantInfo {
            index: v.id.0,
            bandwidth_bps: v.bandwidth,
            name: v.name.clone(),
            codecs: v.codec.as_ref().and_then(|c| c.codecs.clone()),
            container: v
                .codec
                .as_ref()
                .and_then(|c| c.container)
                .map(|fmt| format!("{fmt:?}")),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rstest::{fixture, rstest};

    use super::*;
    use crate::HlsError;

    #[fixture]
    fn variant_id_42() -> VariantId {
        VariantId(42)
    }

    #[fixture]
    fn variant_id_5() -> VariantId {
        VariantId(5)
    }

    #[fixture]
    fn variant_id_0() -> VariantId {
        VariantId(0)
    }

    #[fixture]
    fn simple_master_playlist_data() -> &'static [u8] {
        b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"mp4a.40.2\"
audio.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2000000,CODECS=\"mp4a.40.2\"
audio_high.m3u8"
    }

    #[fixture]
    fn simple_media_playlist_data() -> &'static [u8] {
        b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXTINF:4.0,
segment0.ts
#EXTINF:4.0,
segment1.ts
#EXT-X-ENDLIST"
    }

    #[fixture]
    fn media_playlist_with_init_data() -> &'static [u8] {
        b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-MAP:URI=\"init.mp4\"
#EXTINF:4.0,
segment0.m4s
#EXT-X-ENDLIST"
    }

    #[fixture]
    fn invalid_playlist_data() -> &'static [u8] {
        b"NOT A VALID PLAYLIST"
    }

    #[fixture]
    fn empty_master_playlist_data() -> &'static [u8] {
        b"#EXTM3U
#EXT-X-VERSION:6"
    }

    #[fixture]
    fn master_playlist_with_codec_data() -> &'static [u8] {
        b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"mp4a.40.2,avc1.64001f\",RESOLUTION=1280x720
video.m3u8"
    }

    // Test Cases

    #[rstest]
    fn test_variant_id_creation(variant_id_42: VariantId) {
        assert_eq!(variant_id_42.0, 42);
    }

    #[rstest]
    fn test_parse_simple_master_playlist(simple_master_playlist_data: &[u8]) {
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

    #[rstest]
    fn test_parse_simple_media_playlist(
        simple_media_playlist_data: &[u8],
        variant_id_0: VariantId,
    ) {
        let result = parse_media_playlist(simple_media_playlist_data, variant_id_0);
        assert!(
            result.is_ok(),
            "Failed to parse media playlist: {:?}",
            result.err()
        );

        let media = result.unwrap();
        assert_eq!(media.segments.len(), 2);
        assert_eq!(
            media.target_duration,
            Some(std::time::Duration::from_secs_f64(4.0))
        );
        assert_eq!(media.media_sequence, 0);
        assert!(media.end_list);

        assert_eq!(media.segments[0].variant_id.0, 0);
        assert_eq!(media.segments[0].uri, "segment0.ts");
        assert_eq!(media.segments[0].sequence, 0);

        assert_eq!(media.segments[1].variant_id.0, 0);
        assert_eq!(media.segments[1].uri, "segment1.ts");
        assert_eq!(media.segments[1].sequence, 1);
    }

    #[rstest]
    fn test_parse_media_playlist_with_init_segment(
        media_playlist_with_init_data: &[u8],
        variant_id_0: VariantId,
    ) {
        let result = parse_media_playlist(media_playlist_with_init_data, variant_id_0);
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

    #[rstest]
    fn test_parse_invalid_playlist(invalid_playlist_data: &[u8]) {
        let result = parse_master_playlist(invalid_playlist_data);
        assert!(result.is_err(), "Should fail to parse invalid playlist");

        if let Err(HlsError::PlaylistParse(_)) = result {
            // Expected error type
        } else {
            panic!("Expected PlaylistParse error, got: {:?}", result);
        }
    }

    #[rstest]
    fn test_empty_master_playlist(empty_master_playlist_data: &[u8]) {
        let result = parse_master_playlist(empty_master_playlist_data);
        assert!(result.is_ok(), "Empty master playlist should parse");

        let master = result.unwrap();
        assert_eq!(master.variants.len(), 0);
    }

    #[rstest]
    fn test_media_playlist_variant_id_preserved(
        simple_media_playlist_data: &[u8],
        variant_id_5: VariantId,
    ) {
        let result = parse_media_playlist(simple_media_playlist_data, variant_id_5);
        assert!(result.is_ok());

        let media = result.unwrap();
        assert_eq!(media.segments.len(), 2);
        assert_eq!(media.segments[0].variant_id.0, 5);
        assert_eq!(media.segments[1].variant_id.0, 5);
    }

    #[rstest]
    fn test_master_playlist_with_codec_info(master_playlist_with_codec_data: &[u8]) {
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
    }

    #[rstest]
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

    #[rstest]
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
            id: VariantId(id as usize),
            uri: uri.to_string(),
            bandwidth,
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

    #[rstest]
    #[case(0u32, 0u64, "segment.ts", 4.0)]
    #[case(5u32, 1u64, "segment1.m4s", 6.0)]
    #[case(10u32, 2u64, "chunk.ts", 2.5)]
    fn test_playlist_struct_debug_segment(
        #[case] variant_id: u32,
        #[case] sequence: u64,
        #[case] uri: &str,
        #[case] duration_secs: f64,
    ) {
        let segment = MediaSegment {
            sequence,
            variant_id: VariantId(variant_id as usize),
            uri: uri.to_string(),
            duration: std::time::Duration::from_secs_f64(duration_secs),
            key: None,
        };

        let debug_output = format!("{:?}", segment);
        assert!(debug_output.contains("MediaSegment"));
        assert!(debug_output.contains(uri));
    }

    #[rstest]
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

    #[rstest]
    #[case(
        b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg1.ts\n#EXT-X-ENDLIST",
        1,
        true
    )]
    #[case(b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg1.ts\n#EXTINF:4.0,\nseg2.ts", 2, false)]
    #[case(b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg1.ts\n#EXTINF:4.0,\nseg2.ts\n#EXTINF:4.0,\nseg3.ts\n#EXT-X-ENDLIST", 3, true)]
    fn test_media_playlist_segment_count_and_endlist(
        #[case] data: &[u8],
        #[case] expected_segments: usize,
        #[case] expected_endlist: bool,
        variant_id_0: VariantId,
    ) {
        let result = parse_media_playlist(data, variant_id_0);
        assert!(
            result.is_ok(),
            "Failed to parse playlist: {:?}",
            result.err()
        );

        let media = result.unwrap();
        assert_eq!(media.segments.len(), expected_segments);
        assert_eq!(media.end_list, expected_endlist);
    }

    #[rstest]
    fn test_allow_cache_no(variant_id_0: VariantId) {
        let data = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-ALLOW-CACHE:NO\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg1.ts\n#EXT-X-ENDLIST";
        let media = parse_media_playlist(data, variant_id_0).unwrap();
        assert!(!media.allow_cache, "allow_cache should be false");
    }

    #[rstest]
    fn test_allow_cache_default(simple_media_playlist_data: &[u8], variant_id_0: VariantId) {
        let media = parse_media_playlist(simple_media_playlist_data, variant_id_0).unwrap();
        assert!(media.allow_cache, "allow_cache should be true by default");
    }

    #[rstest]
    fn test_allow_cache_yes(variant_id_0: VariantId) {
        let data = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-ALLOW-CACHE:YES\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\nseg1.ts\n#EXT-X-ENDLIST";
        let media = parse_media_playlist(data, variant_id_0).unwrap();
        assert!(media.allow_cache, "allow_cache should be true for YES");
    }
}
