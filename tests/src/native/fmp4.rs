use std::borrow::Cow;

use kithara::platform::sync::Arc;

/// One muxed fMP4 variant plus the `CODECS` attribute its playlist entry needs.
///
/// `kithara-test-fixtures` builds the boxes; the RFC 6381 string is a manifest
/// concern and stays here with the harness that writes playlists. The segments
/// are shared because every request for the same variant hands out the same
/// bytes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PackagedVariantData {
    pub(crate) init_segment: Arc<Vec<u8>>,
    pub(crate) rfc6381_codec: Cow<'static, str>,
    pub(crate) media_segments: Vec<Arc<Vec<u8>>>,
    pub(crate) segment_durations_secs: Vec<f64>,
}
