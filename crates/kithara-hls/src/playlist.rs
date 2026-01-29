//! Playlist parsing helpers and ABR integration.

use kithara_abr::{VariantInfo, VariantSource};
pub use kithara_stream::ContainerFormat;

// Re-export parsing types and functions for external use
pub use crate::parsing::{
    CodecInfo, EncryptionMethod, InitSegment, KeyInfo, MasterPlaylist, MediaPlaylist, MediaSegment,
    SegmentKey, VariantId, VariantStream, parse_master_playlist, parse_media_playlist,
};

/// Extract extended variant metadata from master playlist.
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
                .map(|fmt| format!("{:?}", fmt)),
        })
        .collect()
}

/// Implement `VariantSource` for `MasterPlaylist`.
impl VariantSource for MasterPlaylist {
    fn variant_count(&self) -> usize {
        self.variants.len()
    }

    fn variant_bandwidth(&self, index: usize) -> Option<u64> {
        self.variants
            .iter()
            .find(|v| v.id.0 == index)
            .and_then(|v| v.bandwidth)
    }
}
