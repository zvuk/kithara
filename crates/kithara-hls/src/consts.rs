use kithara_platform::time::Duration;

/// Enough rounds for an obstruction another task is already clearing to
/// disappear, few enough that a standing one reaches the reader instead of
/// parking it.
pub(crate) const DEFAULT_ACQUIRE_ATTEMPT_BUDGET: u8 = 3;

pub(crate) const DEFAULT_EPHEMERAL_CACHE_MAX_MEDIA_WINDOW: usize = 60;
pub(crate) const DEFAULT_EPHEMERAL_CACHE_MIN_MEDIA_WINDOW: usize = 3;
pub(crate) const DEFAULT_EPHEMERAL_CACHE_NON_MEDIA_RESERVE: usize = 4;
pub(crate) const DEFAULT_DOWNLOAD_BATCH_SIZE: usize = 3;

/// Production HLS streams need a downloader backpressure cap so an idle reader
/// does not drain the whole playlist into cache.
pub(crate) const DEFAULT_LOOK_AHEAD_BYTES: u64 = 2 * 1024 * 1024;

pub(crate) const AES_KEY_LEN: usize = 16;
pub(crate) const IV_SEQUENCE_OFFSET: usize = 8;

#[cfg(test)]
pub(crate) const VALID_KEY: &[u8] = b"0123456789abcdef";

/// AES initialization vector length in bytes.
pub(crate) const IV_LEN: usize = 16;

#[cfg(test)]
pub(crate) const SIMPLE_MASTER_PLAYLIST: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"mp4a.40.2\"
audio.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2000000,CODECS=\"mp4a.40.2\"
audio_high.m3u8";

#[cfg(test)]
pub(crate) const SIMPLE_MEDIA_PLAYLIST: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXTINF:4.0,
segment0.ts
#EXTINF:4.0,
segment1.ts
#EXT-X-ENDLIST";

#[cfg(test)]
pub(crate) const MEDIA_PLAYLIST_WITH_INIT: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-MAP:URI=\"init.mp4\"
#EXTINF:4.0,
segment0.m4s
#EXT-X-ENDLIST";

#[cfg(test)]
pub(crate) const MEDIA_PLAYLIST_WITH_LATER_INIT_MAP: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:4.0,
segment0.m4s
#EXT-X-MAP:URI=\"init.mp4\"
#EXTINF:4.0,
segment1.m4s
#EXT-X-ENDLIST";

#[cfg(test)]
pub(crate) const LIVE_MEDIA_PLAYLIST_SEQUENCE_100: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:100
#EXTINF:4.0,
segment100.ts
#EXTINF:4.0,
segment101.ts";

#[cfg(test)]
pub(crate) const INVALID_PLAYLIST: &[u8] = b"NOT A VALID PLAYLIST";

#[cfg(test)]
pub(crate) const EMPTY_MASTER_PLAYLIST: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6";

#[cfg(test)]
pub(crate) const MASTER_PLAYLIST_WITH_CODEC: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"mp4a.40.2,avc1.64001f\",RESOLUTION=1280x720
video.m3u8";

#[cfg(test)]
pub(crate) const MASTER_PLAYLIST_WITH_MIXED_CASE_FLAC_CODEC: &[u8] = b"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"fLaC\"
audio_flac.m3u8";

#[cfg(test)]
pub(crate) const MASTER_PLAYLIST_WITH_UNCLOSED_QUOTE: &[u8] = b"#EXTM3U
#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"";

#[cfg(test)]
pub(crate) const MEDIA_PLAYLIST_WITH_UNCLOSED_QUOTE: &[u8] = b"#EXTM3U
#EXT-X-TARGETDURATION:4
#EXT-X-KEY:METHOD=AES-128,URI=\"
#EXTINF:4,
seg0.ts";

/// Watchdog timeout for the off-RT blocking `wait_range(_, None)`: must exceed
/// the `kithara-net` per-fetch total timeout so a stalled upstream is failed by
/// the network layer (the wait then returns a terminal `Err`) before this
/// deadlock watchdog fires. Mirrors `kithara-storage` `WAIT_HANG_TIMEOUT`. Only
/// a wait that never wakes after every signal site fired is a real deadlock.
pub(crate) const WAIT_HANG_TIMEOUT: Duration = Duration::from_secs(180);

pub(crate) const INIT_PLACEHOLDER_BYTES: u64 = 16 * 1024;

/// Reserved [`VariantFlow::prefetch_resume_at`] marker for "nothing is
/// deferred" (a `2^64 - 1` byte cursor is unreachable).
pub(crate) const NO_PREFETCH_DEFERRAL: u64 = u64::MAX;

#[cfg(test)]
pub(crate) const BUDGET: u8 = DEFAULT_ACQUIRE_ATTEMPT_BUDGET;

pub(crate) const NO_WAIT: u64 = 0;

#[cfg(test)]
pub(crate) const EXACT_SEEK_LANDING: u32 = 2;
