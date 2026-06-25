use std::{ops::Range, sync::Arc};

use kithara_assets::{AssetScope, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_platform::time::Duration;
use kithara_stream::StreamResult;
use url::Url;

use crate::{
    handle::ResourceHandle,
    segment::{SegmentSize, SegmentSlotState},
};

/// Decryption disposition for a segment / init resource. Replaces
/// `decrypt_ctx: Option<DecryptContext>`, whose `.is_some()` /
/// `.map_or_else` discrimination on the acquire path conflated "no key"
/// with "cleartext". `Plain` is the explicit cleartext case; `Encrypted`
/// carries the AES-128 [`DecryptContext`].
#[derive(Debug, Clone)]
pub(crate) enum SegmentContent {
    Plain,
    Encrypted(DecryptContext),
}

impl From<Option<DecryptContext>> for SegmentContent {
    fn from(ctx: Option<DecryptContext>) -> Self {
        ctx.map_or(Self::Plain, Self::Encrypted)
    }
}

/// One cache slot in the variant's content domain. The shared-but-distinct
/// kinds — a separately fetched `#EXT-X-MAP` init prefix vs a media segment —
/// are folded under one enum (req 6): callers treat any slot uniformly through
/// the cascade methods (`state` / `size` / `resource` / `read_at` / `contains`
/// / `len` / `url`), and the few media-only queries (`decode_time` / `duration`)
/// live on [`MediaSegment`].
///
/// Each cascade method dispatches DOWN into the arm, reproducing exactly the
/// per-kind code path the variant's `read_at` / `range_ready` /
/// `media_descriptor` / `init_descriptor_at` ran before the fold. `read_at` /
/// `contains` route through the segment's [`ResourceHandle`] — the same handle
/// `segment_handle` / `init_handle` vended — built from the passed scope plus
/// the arm's `resource_id` + `url`.
#[derive(Debug)]
pub(crate) enum Segment {
    Init(InitSegment),
    Media(MediaSegment),
}

impl Segment {
    /// Cache state. Shared with the slot's `FetchSlot` handle (see
    /// [`MediaSegment::state`]).
    pub(crate) fn state(&self) -> &Arc<SegmentSlotState> {
        match self {
            Self::Init(s) => &s.state,
            Self::Media(s) => &s.state,
        }
    }

    /// The segment's byte-length atom (seed or committed). `len()` reads it.
    pub(crate) fn size(&self) -> &SegmentSize {
        match self {
            Self::Init(s) => &s.size,
            Self::Media(s) => &s.size,
        }
    }

    /// Decryption disposition — the acquire path branches on it.
    pub(crate) fn content(&self) -> &SegmentContent {
        match self {
            Self::Init(s) => &s.content,
            Self::Media(s) => &s.content,
        }
    }

    /// The segment's resource key.
    pub(crate) fn resource_id(&self) -> &ResourceKey {
        match self {
            Self::Init(s) => &s.resource_id,
            Self::Media(s) => &s.resource_id,
        }
    }

    /// The segment's fetch URL.
    pub(crate) fn url(&self) -> &Url {
        match self {
            Self::Init(s) => &s.url,
            Self::Media(s) => &s.url,
        }
    }

    /// Route byte length: `size().get()`. The route length is stable virtual
    /// geometry; the completeness gate is `size().is_exact()`.
    pub(crate) fn len(&self) -> u64 {
        self.size().get()
    }

    /// Read byte length: exact committed/probed length when known, otherwise
    /// the route length so descriptors remain addressable before commit.
    pub(crate) fn read_len(&self) -> u64 {
        self.size().read_len()
    }

    /// Narrow disk handle for this slot, built from the variant's shared scope
    /// plus the slot's key and url — the same cheap clone `segment_handle` /
    /// `init_handle` produced.
    pub(crate) fn resource(&self, scope: &AssetScope<DecryptContext>) -> ResourceHandle {
        ResourceHandle::new(
            scope.clone(),
            self.resource_id().clone(),
            self.url().clone(),
        )
    }

    /// Open the slot's resource and copy `range` into `dst`. Routes through the
    /// slot's [`ResourceHandle`] — `Ok(None)` means the bytes are not on disk
    /// yet.
    pub(crate) fn read_at(
        &self,
        scope: &AssetScope<DecryptContext>,
        range: Range<u64>,
        dst: &mut [u8],
    ) -> StreamResult<Option<usize>> {
        self.resource(scope).read_at(range, dst)
    }

    /// Whether every byte in `range` is already present on disk for this slot.
    pub(crate) fn contains(&self, scope: &AssetScope<DecryptContext>, range: Range<u64>) -> bool {
        self.resource(scope).contains(range)
    }

    /// The media arm, or `None` for the init slot — the seam for the
    /// media-only `decode_time` / `duration` accessors. `segments()` only ever
    /// holds `Media` arms, so callers iterating it always get `Some`.
    pub(crate) fn as_media(&self) -> Option<&MediaSegment> {
        match self {
            Self::Init(_) => None,
            Self::Media(s) => Some(s),
        }
    }
}

#[derive(Debug)]
pub(crate) struct MediaSegment {
    /// Cache state. The owning [`FetchClaim<Downloading>`](crate::segment::FetchClaim) handle (held by the
    /// segment's `FetchSlot`) shares this `Arc` and flips it on settle:
    /// `Loaded` on success, `Missing` on recoverable failure. Stale
    /// settles (cancelled before completion) are gated by
    /// `FetchSlot.cancel` and leave the slot untouched.
    pub(crate) state: Arc<SegmentSlotState>,
    /// Media size state. Non-exact placeholders are routeable before commit;
    /// committed lengths update the contiguous HLS byte map.
    pub(crate) size: SegmentSize,
    pub(crate) decode_time: Duration,
    pub(crate) duration: Duration,
    pub(crate) resource_id: ResourceKey,
    pub(crate) content: SegmentContent,
    pub(crate) url: Url,
}

impl MediaSegment {
    /// Decode-time window start of this media segment.
    pub(crate) fn decode_time(&self) -> Duration {
        self.decode_time
    }

    /// Decode-time window length of this media segment.
    pub(crate) fn duration(&self) -> Duration {
        self.duration
    }
}

#[derive(Debug)]
pub(crate) struct InitSegment {
    /// Shared with the init segment's `FetchSlot` handle; see
    /// [`MediaSegment::state`].
    pub(crate) state: Arc<SegmentSlotState>,
    /// Init size state — see [`MediaSegment::size`].
    pub(crate) size: SegmentSize,
    pub(crate) resource_id: ResourceKey,
    /// Decryption disposition for the init segment. HLS init segments
    /// don't carry their own `#EXT-X-KEY`; an encrypted variant mirrors
    /// the first media segment's key — the standard packaging convention.
    pub(crate) content: SegmentContent,
    pub(crate) url: Url,
}
