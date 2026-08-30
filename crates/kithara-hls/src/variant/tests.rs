use std::{
    num::NonZeroU64,
    sync::{Barrier, OnceLock, atomic::Ordering},
    thread,
};

use kithara_abr::AbrState;
use kithara_assets::{
    AcquisitionResult, AssetResource, AssetScope, AssetSource, AssetStore, StorageBackend,
    WriteSide,
};
use kithara_drm::DecryptContext;
use kithara_events::{AbrMode, AbrReason, Event, EventBus, HlsEvent, VariantIndex};
use kithara_platform::{
    CancelToken,
    sync::{Arc, ThreadGate},
    time::Duration,
};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    AudioCodec, ContainerFormat, ReadOutcome, ReaderInput, ReaderProfile, ReaderWarmup,
    SeekControl, SeekObserve, SeekState, SourceError, SourcePhase, StreamError, VariantTransition,
    VariantTransitionId,
};
use kithara_test_utils::kithara;
use url::Url;

use super::{HlsVariant, PlanConfig, PlanCtx, SizeDemand, VariantParts, segment_placeholder_size};
use crate::{
    playlist::{PlaylistState, SegmentState, VariantState},
    segment::{
        Downloading, FetchClaim, InitSegment, MediaSegment, PlannedFetch, Segment, SegmentContent,
        SegmentSize, SegmentSlotState,
    },
    signal::SizeSignal,
    stream::HlsSession,
};

fn test_ctx(prefetch_budget: usize) -> PlanCtx {
    let cancel = CancelToken::never();
    let backend = Arc::new(
        AssetStore::builder()
            .backend(StorageBackend::Memory)
            .cancel(cancel)
            .build(),
    );
    PlanCtx {
        bus: EventBus::new(8),
        scope: backend
            .scope::<crate::Hls>(&AssetSource::Remote {
                url: Url::parse("https://example.com/master.m3u8").expect("master url"),
                discriminator: Some("test".to_owned()),
            })
            .expect("test asset scope"),
        seek_epoch: 0,
        headers: None,
        signal: SizeSignal::new(Arc::new(ThreadGate::default()), Arc::new(OnceLock::new())),
        config: PlanConfig::builder()
            .prefetch_budget(prefetch_budget)
            .build(),
    }
}

fn make_init(size: u64, scope: &AssetScope) -> Option<Segment> {
    if size == 0 {
        return None;
    }
    let url: Url = "https://example.com/init.mp4".parse().expect("valid url");
    let resource_id = scope
        .key(&AssetResource::Url(url.clone()))
        .expect("init key");
    Some(Segment::Init(InitSegment {
        url,
        resource_id,
        state: SegmentSlotState::missing(),
        size: SegmentSize::seed(size),
        content: SegmentContent::Plain,
    }))
}

fn make_placeholder_init(size: u64, scope: &AssetScope) -> Segment {
    let url: Url = "https://example.com/init.mp4".parse().expect("valid url");
    let resource_id = scope
        .key(&AssetResource::Url(url.clone()))
        .expect("init key");
    Segment::Init(InitSegment {
        url,
        resource_id,
        state: SegmentSlotState::missing(),
        size: SegmentSize::placeholder(size),
        content: SegmentContent::Plain,
    })
}

fn make_seg(idx: u32, size: u64, scope: &AssetScope) -> Segment {
    let url: Url = format!("https://example.com/seg{idx}.m4s")
        .parse()
        .expect("valid url");
    let resource_id = scope
        .key(&AssetResource::Url(url.clone()))
        .expect("segment key");
    Segment::Media(MediaSegment {
        url,
        resource_id,
        state: SegmentSlotState::missing(),
        size: SegmentSize::seed(size),
        content: SegmentContent::Plain,
        decode_time: Duration::from_millis(u64::from(idx) * 2000),
        duration: Duration::from_secs(2),
    })
}

fn make_placeholder_seg(idx: u32, size: u64, scope: &AssetScope) -> Segment {
    let url: Url = format!("https://example.com/seg{idx}.m4s")
        .parse()
        .expect("valid url");
    let resource_id = scope
        .key(&AssetResource::Url(url.clone()))
        .expect("segment key");
    Segment::Media(MediaSegment {
        url,
        resource_id,
        state: SegmentSlotState::missing(),
        size: SegmentSize::placeholder(size),
        content: SegmentContent::Plain,
        decode_time: Duration::from_millis(u64::from(idx) * 2000),
        duration: Duration::from_secs(2),
    })
}

fn make_var(variant: usize, init_size: u64, media_sizes: &[u64], ctx: &PlanCtx) -> Arc<HlsVariant> {
    make_var_with_seek_obs(
        variant,
        init_size,
        media_sizes,
        ctx,
        Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
    )
}

fn active_session(v: &Arc<HlsVariant>, ctx: &PlanCtx, position: u64) -> HlsSession {
    HlsSession::active(
        CancelToken::never(),
        Arc::new(SeekState::new()),
        ctx.signal.clone(),
        v.variant,
        Arc::clone(v),
        position,
    )
}

fn make_var_with_seek_obs(
    variant: usize,
    init_size: u64,
    media_sizes: &[u64],
    ctx: &PlanCtx,
    seek_obs: Arc<dyn SeekObserve>,
) -> Arc<HlsVariant> {
    let init = make_init(init_size, &ctx.scope);
    let segments: Vec<Segment> = media_sizes
        .iter()
        .enumerate()
        .map(|(i, &size)| {
            make_seg(
                u32::try_from(i).expect("segment index < u32::MAX"),
                size,
                &ctx.scope,
            )
        })
        .collect();
    VariantParts {
        init,
        segments,
        seek_obs,
        codec: None,
        container: None,
    }
    .into_variant(variant, ctx)
}

fn make_playlist_state(
    codec: Option<AudioCodec>,
    container: Option<ContainerFormat>,
    count: usize,
) -> Arc<PlaylistState> {
    let segments = (0..count)
        .map(|idx| {
            let url: Url = format!("https://example.com/media{idx}.m4s")
                .parse()
                .expect("valid url");
            SegmentState {
                url,
                duration: Duration::from_secs(2),
                byte_range_len: None,
            }
        })
        .collect();
    Arc::new(PlaylistState::new(vec![VariantState {
        codec,
        container,
        init_url: None,
        segments,
    }]))
}

#[kithara::test]
fn media_info_carries_playlist_container() {
    let ctx = test_ctx(1);
    let playlist_state =
        make_playlist_state(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), 1);
    let v = VariantParts {
        init: None,
        segments: vec![make_seg(0, 10, &ctx.scope)],
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: playlist_state.variant_codec(0),
        container: playlist_state.variant_container(0),
    }
    .into_variant(0, &ctx);

    let info = v.media_info();
    assert_eq!(info.codec, Some(AudioCodec::AacLc));
    assert_eq!(info.container, Some(ContainerFormat::Fmp4));
}

fn push_planned(v: &HlsVariant, seg: u32) {
    v.flow.queue.lock().push_back(PlannedFetch::Segment(seg));
}

fn queue_seg_indices(v: &HlsVariant) -> Vec<u32> {
    v.flow
        .queue
        .lock()
        .iter()
        .filter_map(|p| match p {
            PlannedFetch::Segment(seg) => Some(*seg),
            PlannedFetch::Init => None,
        })
        .collect()
}

/// Whether a seek is still unresolved — the reader has not yet landed on the
/// target it was aimed at.
fn seek_projection_is_live(v: &HlsVariant) -> bool {
    v.seek.alias.load().is_some() || v.seek.exact_seek.load().is_some()
}

fn queue_has_init(v: &HlsVariant) -> bool {
    v.flow
        .queue
        .lock()
        .iter()
        .any(|p| matches!(p, PlannedFetch::Init))
}

fn collect_events(events: &mut kithara_events::EventReceiver) -> Vec<Event> {
    std::iter::from_fn(|| events.try_recv().ok())
        .map(|envelope| envelope.event)
        .collect()
}

#[kithara::test]
fn placeholder_size_uses_duration_as_route_geometry() {
    let size = segment_placeholder_size(Duration::from_secs(2), Some(64_000));

    assert_eq!(size, 8 * 1024);
    assert!(
        !SegmentSize::placeholder(size).is_exact(),
        "duration-derived route sizes must stay non-exact"
    );
}

#[kithara::test]
fn cache_complete_publishes_once_after_full_commit() {
    let ctx = test_ctx(3);
    let mut events = ctx.bus.subscribe();
    let v = VariantParts {
        init: make_init(8, &ctx.scope),
        segments: (0..2).map(|idx| make_seg(idx, 4, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::AacLc),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);

    if let Some(init) = v.init() {
        let key = init.resource_id().clone();
        let AcquisitionResult::Pending(writer) =
            ctx.scope.store().acquire_resource(&key, None).unwrap()
        else {
            panic!("init must acquire as Pending");
        };
        writer.write_at(0, b"initinit").unwrap();
        writer.commit(Some(8)).unwrap();
        init.state().mark_loaded();
    }
    for segment in v.segments() {
        let key = segment.resource_id().clone();
        let AcquisitionResult::Pending(writer) =
            ctx.scope.store().acquire_resource(&key, None).unwrap()
        else {
            panic!("segment must acquire as Pending");
        };
        writer.write_at(0, b"data").unwrap();
        writer.commit(Some(4)).unwrap();
        segment.state().mark_loaded();
    }

    v.maybe_publish_cache_complete();
    v.maybe_publish_cache_complete();

    let events = collect_events(&mut events);
    let cache_complete_count = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::Hls(HlsEvent::CacheComplete {
                    total_bytes: Some(16),
                })
            )
        })
        .count();
    assert_eq!(cache_complete_count, 1);
}

#[kithara::test]
fn range_ready_clamps_tail_seek_alias_to_eof() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: None,
        segments: (0..3).map(|idx| make_seg(idx, 10, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::AacHeV2),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);
    for segment in v.segments() {
        let key = segment.resource_id().clone();
        let AcquisitionResult::Pending(writer) = ctx
            .scope
            .store()
            .acquire_resource(&key, None)
            .expect("acquire segment")
        else {
            panic!("segment resource must be pending");
        };
        let segment_len =
            usize::try_from(segment.len()).expect("test segment length must fit usize");
        let bytes = vec![0u8; segment_len];
        writer.write_at(0, &bytes).expect("write segment");
        writer
            .commit(Some(bytes.len() as u64))
            .expect("commit segment");
    }

    let anchor = v.segment_byte_offset(1).expect("segment 1 offset");
    v.set_seek_alias(anchor, 1);

    assert!(
        v.range_ready(&(anchor..anchor + 64)),
        "tail range that starts in a seek alias must clamp to ready EOF"
    );
}

#[kithara::test]
fn segment_aware_seek_alias_routes_tail_by_segment_index() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: None,
        segments: (0..4).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Flac),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);
    let anchor = 1_000;

    v.set_seek_alias(anchor, 2);

    assert_eq!(v.find_at_offset(anchor), Some((2, anchor, 100)));
    assert_eq!(v.find_at_offset(anchor + 99), Some((2, anchor, 100)));
    assert_eq!(v.find_at_offset(anchor + 100), Some((3, anchor + 100, 100)));
    assert_eq!(v.find_at_offset(anchor + 200), None);
}

#[kithara::test]
fn position_starts_at_zero() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400], &ctx);
    let session = active_session(&v, &ctx, 0);
    assert_eq!(session.position(), 0);
}

#[kithara::test]
fn descriptor_at_byte_uses_the_find_at_offset_range() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400, 400, 400], &ctx);

    let descriptor = v.descriptor_at_byte(1_050).expect("segment 2 resolves");

    assert_eq!(descriptor.segment_index, 2);
    assert_eq!(
        descriptor.byte_range,
        1_000..1_400,
        "ByteMap descriptors must use the same coordinates as \
         find_at_offset/read_at/phase_at"
    );
}

#[kithara::test]
fn reset_to_full_range_uses_live_init_size() {
    let ctx = test_ctx(3);
    let v = make_var(0, 600, &[400, 400], &ctx);

    v.layout.apply_commit(v.segments(), || {
        v.apply_loaded_size(PlannedFetch::Init, 588);
        v.init_size()
    });

    v.reset_to_full_range();

    assert_eq!(
        v.segment_byte_offset(0),
        Some(588),
        "full-range reset must use the committed live init length"
    );
}

#[kithara::test]
fn advance_increments_position() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400], &ctx);
    let session = active_session(&v, &ctx, 0);
    session.advance(64);
    assert_eq!(session.position(), 64);
    session.advance(36);
    assert_eq!(session.position(), 100);
}

#[kithara::test]
fn set_position_overrides_cursor() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400], &ctx);
    let session = active_session(&v, &ctx, 0);
    session.advance(50);
    session.set_position(1234);
    assert_eq!(session.position(), 1234);
}

#[kithara::test]
fn find_at_offset_inside_init_prefix_is_none() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400], &ctx);
    assert!(v.find_at_offset(0).is_none());
    assert!(v.find_at_offset(199).is_none());
}

#[kithara::test]
fn demand_segment_at_offset_inside_init_prefix_is_segment_zero() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400], &ctx);
    assert_eq!(v.demand_segment_at_offset(0), Some(0));
    assert_eq!(v.demand_segment_at_offset(199), Some(0));
}

#[kithara::test]
fn reader_warmup_starting_in_init_prefix_rebuilds_from_segment_zero() {
    let ctx = test_ctx(3);
    let v = make_var(0, 48, &[64, 64, 64], &ctx);
    let profile = ReaderProfile::new(
        ReaderInput::Incremental,
        ReaderWarmup::ReadBehind {
            max_bytes: NonZeroU64::new(100).expect("non-zero warmup"),
        },
        NonZeroU64::new(64).expect("non-zero read ahead"),
    );

    v.prepare_reader(profile, Duration::from_secs(2))
        .expect("reader preparation");

    let queue: Vec<_> = v.flow.queue.lock().iter().copied().collect();
    assert_eq!(
        queue,
        vec![
            PlannedFetch::Init,
            PlannedFetch::Segment(0),
            PlannedFetch::Segment(1),
            PlannedFetch::Segment(2),
        ]
    );
}

#[kithara::test]
fn find_at_offset_at_init_size_returns_segment_zero() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400], &ctx);
    let (idx, byte_offset, _) = v.find_at_offset(200).expect("hit");
    assert_eq!(idx, 0);
    assert_eq!(byte_offset, 200);
}

#[kithara::test]
fn find_at_offset_mid_segment_binary_search() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[400, 400, 400, 400], &ctx);
    let (idx, _, _) = v.find_at_offset(550).expect("mid-segment");
    assert_eq!(idx, 1);
    let (idx, _, _) = v.find_at_offset(1199).expect("last byte of seg 2");
    assert_eq!(idx, 2);
    let (idx, _, _) = v.find_at_offset(1200).expect("first byte of seg 3");
    assert_eq!(idx, 3);
}

#[kithara::test]
fn find_at_offset_reflects_post_commit_size_shrink() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[400, 400, 400, 400], &ctx);

    let (idx, off, size) = v.find_at_offset(450).expect("seg 1 before shrink");
    assert_eq!((idx, off, size), (1, 400, 400));

    v.layout.apply_commit(v.segments(), || {
        v.segments()[0].size().set_exact(384);
        v.init_size()
    });

    let (idx, off, size) = v.find_at_offset(390).expect("seg 1 after shrink");
    assert_eq!(
        (idx, off, size),
        (1, 384, 400),
        "shrinking seg 0 slides seg 1 down by the stripped delta"
    );
    assert!(
        v.find_at_offset(383).is_some_and(|(i, ..)| i == 0),
        "byte 384 is seg 1's new start, so 383 is the last byte of the shrunk seg 0"
    );
    assert!(
        v.find_at_offset(384).is_some_and(|(i, ..)| i == 1),
        "byte 384 belongs to seg 1 after the shrink"
    );
}

/// `total_bytes()` is a lock-free `AtomicU64` snapshot (RT produce-core read).
/// It must still track every write-lock mutation — a post-commit size shrink
/// republishes the cached total.
#[kithara::test]
fn total_bytes_lock_free_tracks_commit() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400, 400, 400], &ctx);
    assert_eq!(v.total_bytes(), 200 + 400 * 4, "init + 4 media segments");

    v.layout.apply_commit(v.segments(), || {
        v.segments()[0].size().set_exact(384);
        v.init_size()
    });
    assert_eq!(
        v.total_bytes(),
        200 + 384 + 400 * 3,
        "lock-free total reflects the post-commit shrink of seg 0"
    );
}

/// The produce-core lookup takes only a shared read-lock on the Layout
/// frame, so it can never resize the offset table (resize needs the
/// exclusive lock). Verify repeated lookups stay self-consistent across
/// the whole virtual range.
#[kithara::test]
fn find_at_offset_is_stable_over_repeated_lookups() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[400, 400, 400, 400], &ctx);

    for byte in 0..1_600_u64 {
        let (idx, off, size) = v.find_at_offset(byte).expect("every media byte resolves");
        assert!(
            off <= byte && byte < off + size,
            "byte {byte} inside its segment"
        );
        assert_eq!(u64::from(idx), byte / 400, "400-byte segments map linearly");
    }
    assert!(
        v.find_at_offset(1_600).is_none(),
        "one past the last byte is EOF"
    );
}

#[kithara::test]
fn total_bytes_includes_init_and_segments() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400, 400, 400], &ctx);
    assert_eq!(v.total_bytes(), 200 + 400 * 4);
}

#[kithara::test]
fn init_byte_range_present_when_size_positive() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[], &ctx);
    assert_eq!(v.init_byte_range(), 0..200);
}

#[kithara::test]
fn init_byte_range_empty_when_size_zero() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[], &ctx);
    assert!(v.init_byte_range().is_empty());
}

/// `init_size == 0` (no `#EXT-X-MAP`, byte-range-embedded init, or a failed
/// separate init URL) is exactly a `None` init slot (old `VariantInit::NotApplicable`):
/// no separate init resource, no `about:blank` acquire, and `rebuild` never
/// enqueues `PlannedFetch::Init`.
#[kithara::test]
fn variant_init_not_applicable_no_acquire() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[400, 400], &ctx);
    assert!(
        v.init().is_none(),
        "init_size == 0 must construct as a None init slot (old NotApplicable)"
    );
    assert_eq!(v.init_size(), 0);
    assert!(
        v.init_resource().is_none(),
        "a None init slot carries no init resource"
    );
    v.rebuild(&ctx, 0);
    assert!(
        !queue_has_init(&v),
        "a None init slot must never enqueue PlannedFetch::Init"
    );
    let session = active_session(&v, &ctx, 0);
    let cmds = session.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 2, "only the two media segments dispatch");
    let seg0_url = v.segments()[0].url().clone();
    assert_eq!(
        cmds[0].url().clone(),
        seg0_url,
        "first cmd is seg 0, not an init"
    );
}

/// `init_size > 0` (fMP4 `#EXT-X-MAP` with a known size) is a present
/// `Some(Segment::Init)`: a real, separately-fetched init segment that is
/// enqueued first and acquired exactly as before.
#[kithara::test]
fn variant_init_pending_for_fmp4() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400], &ctx);
    let Some(entry @ Segment::Init(_)) = v.init() else {
        panic!("init_size > 0 must construct as Some(Segment::Init)");
    };
    assert_eq!(entry.size().get(), 200);
    assert_eq!(v.init_size(), 200);
    let init_url = entry.url().clone();
    assert!(
        v.init_resource().is_some(),
        "Pending init exposes its resource key"
    );
    v.rebuild(&ctx, 0);
    assert!(
        queue_has_init(&v),
        "Pending init must enqueue PlannedFetch::Init first"
    );
    let session = active_session(&v, &ctx, 0);
    let cmds = session.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 3, "init + two media segments dispatch");
    assert_eq!(cmds[0].url().clone(), init_url, "init dispatched first");
}

/// Frozen-discriminator guard: `init.size` only ever shrinks post-commit
/// (known/probed size -> committed `final_len`); it never crosses 0 -> positive.
/// So a present `Some(Segment::Init)` constructed with `init_size > 0` stays
/// present even after a commit shrink — the `Option<Segment>` discriminant is
/// equivalent to the old dynamic `init_size() > 0` check at every later read.
#[kithara::test]
fn variant_init_pending_stays_pending_after_commit_shrink() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400], &ctx);

    v.layout.apply_commit(v.segments(), || {
        v.apply_loaded_size(PlannedFetch::Init, 160);
        v.init_size()
    });

    assert_eq!(v.init_size(), 160, "init size shrinks on commit");
    assert!(
        matches!(v.init(), Some(Segment::Init(_))),
        "a shrink (still > 0) keeps the init present — size never crosses to 0"
    );
    assert!(v.init_resource().is_some());
}

/// Regression: an `#EXT-X-MAP` init whose size is not yet known is still a real
/// init that must be fetched. Existence follows the EXT-X-MAP URL, not the byte
/// size. Misclassifying it as
/// a `None` init slot drops the init: `read_at(0)` then routes to the media
/// loop and serves segment 0's container where the demuxer expects `ftyp`
/// ("`re_mp4`: ftyp not found"), or the reader wedges ("no progress").
#[kithara::test]
fn init_with_url_but_unknown_size_is_pending() {
    let ctx = test_ctx(3);
    let url: Url = "https://example.com/init.mp4".parse().expect("valid url");
    let playlist = PlaylistState::new(vec![VariantState {
        codec: None,
        container: None,
        init_url: Some(url),
        segments: Vec::new(),
    }]);

    let init = HlsVariant::build_init_entry(&playlist, 0, None, &ctx).expect("init entry");

    assert!(
        matches!(init, Some(Segment::Init(_))),
        "EXT-X-MAP init with an unknown size must stay present \
         (existence follows the URL, not the byte size), got {init:?}"
    );
}

/// Regression for the `read_at` init-prefix guard. While an `#EXT-X-MAP` init
/// is declared but not yet sized (`init_size() == 0` — a failed/absent init
/// HEAD, or the window before the init commits), the offset table seeds
/// segment 0 at offset 0. A read at offset 0 must NOT serve that media — doing
/// so hands the demuxer segment 0's container where the init's `ftyp` belongs
/// ("`re_mp4`: ftyp not found"). The read is held pending until the init sizes
/// the prefix.
#[kithara::test]
fn read_at_zero_holds_pending_while_init_unsized() {
    let ctx = test_ctx(3);
    let init_url: Url = "https://example.com/init.mp4".parse().expect("valid url");
    let init = Some(Segment::Init(InitSegment {
        resource_id: ctx
            .scope
            .key(&AssetResource::Url(init_url.clone()))
            .expect("init key"),
        url: init_url,
        state: SegmentSlotState::missing(),
        size: SegmentSize::seed(0),
        content: SegmentContent::Plain,
    }));
    let v = VariantParts {
        init,
        segments: vec![make_seg(0, 1024, &ctx.scope)],
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: None,
        container: None,
    }
    .into_variant(0, &ctx);

    assert!(
        v.has_init() && v.init_size() == 0,
        "precondition: a declared but unsized init"
    );
    assert!(
        v.find_at_offset(0).is_some(),
        "the trap: segment 0 is addressable at offset 0 while the init is unsized"
    );

    // Commit segment 0's bytes so an *unguarded* read_at(0) would serve them.
    let seg0_key = v.segments()[0].resource_id().clone();
    let AcquisitionResult::Pending(writer) = ctx
        .scope
        .store()
        .acquire_resource(&seg0_key, None)
        .expect("acquire segment 0")
    else {
        panic!("segment 0 resource must be pending");
    };
    // Commit a full 1024-byte segment (matching the size atom) so an unguarded
    // read_at(0) resolves a satisfiable range and returns the bytes — making
    // this a genuine red-without-the-guard regression, not a range-pending
    // artifact. The `RIFF` magic stands in for "segment 0's container, not the
    // init's `ftyp`".
    let mut media = vec![0u8; 1024];
    media[..4].copy_from_slice(b"RIFF");
    writer.write_at(0, &media).expect("write segment 0");
    writer
        .commit(Some(media.len() as u64))
        .expect("commit segment 0");

    let mut buf = [0u8; 64];
    let outcome = v.read_at(0, &mut buf).expect("read_at(0)");
    assert!(
        matches!(outcome, ReadOutcome::Pending(_)),
        "read_at(0) must hold pending while the init is unsized, not serve \
         segment 0's container; got {outcome:?}"
    );
    assert_ne!(
        &buf[..4],
        b"RIFF",
        "segment 0's container must not have been served at offset 0"
    );
}

#[kithara::test]
fn descriptor_at_time_clamps_to_last() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100, 100], &ctx);
    let d = v
        .descriptor_at_time(Duration::from_secs(2))
        .expect("descriptor");
    assert_eq!(d.segment_index, 1);
    let d = v
        .descriptor_at_time(Duration::from_secs(999))
        .expect("descriptor");
    assert_eq!(d.segment_index, 2);
}

#[kithara::test]
fn seek_point_at_time_returns_bounds_and_clamps() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100, 100], &ctx);
    let (idx, start, end) = v
        .seek_point_at_time(Duration::from_secs(3))
        .expect("seek point");
    assert_eq!(idx, 1);
    assert_eq!(start, Duration::from_secs(2));
    assert_eq!(end, Duration::from_secs(4));

    let (idx, start, end) = v
        .seek_point_at_time(Duration::from_secs(999))
        .expect("seek point clamps to last");
    assert_eq!(idx, 2);
    assert_eq!(start, Duration::from_secs(4));
    assert_eq!(end, Duration::from_secs(6));
}

#[kithara::test]
fn descriptor_after_byte_finds_next_segment() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100, 100], &ctx);
    let d = v.descriptor_after_byte(50).expect("descriptor");
    assert_eq!(d.segment_index, 1);
    let d = v.descriptor_after_byte(100).expect("descriptor");
    assert_eq!(d.segment_index, 1);
}

#[kithara::test]
fn rebuild_refills_queue_without_touching_session_cancel() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100; 6], &ctx);
    push_planned(&v, 0);
    let token = CancelToken::root();
    let session = HlsSession::active(
        token.clone(),
        Arc::new(SeekState::new()),
        ctx.signal.clone(),
        0,
        Arc::clone(&v),
        0,
    );
    assert!(!token.is_cancelled());
    v.rebuild(&ctx, 2);
    assert!(
        !token.is_cancelled(),
        "rebuild must not cancel its reader session"
    );
    assert_eq!(session.position(), 0);
    assert_eq!(queue_seg_indices(&v), vec![2, 3, 4, 5]);
}

#[kithara::test]
fn segment_aware_rebuild_at_time_prefetches_seek_preroll_segment() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: None,
        segments: (0..5).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::AacLc),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);

    let target = v
        .rebuild_at_time(&ctx, Duration::from_secs(4))
        .expect("target segment");

    assert_eq!(target, 2, "time seek still lands on the target segment");
    assert_eq!(
        queue_seg_indices(&v),
        vec![1, 2, 3, 4],
        "segment-aware seek must fetch the codec pre-roll segment too"
    );
    assert_eq!(
        v.prefetch_anchor(),
        v.segment_byte_offset(1).expect("pre-roll segment offset")
    );
}

#[kithara::test]
fn segment_aware_seek_time_anchor_leaves_the_fetch_plan_to_the_peer() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: None,
        segments: (0..5).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::AacLc),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);

    v.rebuild(&ctx, 0);
    let plan_before = queue_seg_indices(&v);

    let anchor = v
        .prepare_seek_time_anchor(Duration::from_secs(4))
        .expect("seek anchor")
        .expect("segment-aware anchor");

    assert_eq!(anchor.segment_index, Some(2));
    assert_eq!(
        anchor.byte_offset, 200,
        "decoder anchor remains the target segment boundary"
    );
    assert_eq!(
        queue_seg_indices(&v),
        plan_before,
        "the fetch plan is the peer's; anchor resolution must not touch it"
    );
    assert_eq!(v.prefetch_anchor(), 100);
}

#[kithara::test]
fn segment_aware_rebuild_with_decoder_probe_fetches_recreate_preroll_segment() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: make_init(48, &ctx.scope),
        segments: (0..5).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::AacLc),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);

    v.rebuild_with_decoder_probe(&ctx, 2);

    assert!(queue_has_init(&v));
    assert_eq!(
        queue_seg_indices(&v),
        vec![0, 1, 2, 3, 4],
        "format-boundary decoder seek must fetch the codec pre-roll segment"
    );
}

#[kithara::test]
fn exact_size_rebuild_at_time_starts_at_target_segment() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: None,
        segments: (0..5).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);

    let target = v
        .rebuild_at_time(&ctx, Duration::from_secs(4))
        .expect("target segment");

    assert_eq!(target, 2);
    assert_eq!(queue_seg_indices(&v), vec![2, 3, 4]);
    assert_eq!(
        v.prefetch_anchor(),
        v.segment_byte_offset(2).expect("target segment offset")
    );
}

#[kithara::test]
fn exact_size_rebuild_with_decoder_probe_starts_tail_at_target_segment() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: make_init(48, &ctx.scope),
        segments: (0..5).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);

    v.rebuild_with_decoder_probe(&ctx, 2);

    assert!(queue_has_init(&v));
    assert_eq!(queue_seg_indices(&v), vec![0, 2, 3, 4]);
}

#[kithara::test]
fn incoming_reader_preparation_keeps_the_landing_fetch_anchor() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: make_init(48, &ctx.scope),
        segments: (0..5).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);
    let profile = ReaderProfile::new(
        ReaderInput::Incremental,
        ReaderWarmup::None,
        NonZeroU64::new(64).expect("non-zero read ahead"),
    );

    let preparation = v
        .prepare_reader(profile, Duration::from_secs(4))
        .expect("incoming reader preparation");

    assert_eq!(preparation.anchor().segment_index, Some(2));
    assert_eq!(preparation.anchor().byte_offset, 248);
    assert!(queue_has_init(&v));
    // The plan reaches one segment behind the landing even here, where byte
    // sizes are exact: a demuxer parks at the packet boundary at or before the
    // landing, and the landing is a segment start, so its first packet begins in
    // segment 1. Exactness of byte sizes says nothing about the packet grid.
    // Nothing else precedes the landing — no head-of-stream decoder probe.
    assert_eq!(queue_seg_indices(&v), vec![1, 2, 3, 4]);
    // Only the plan reaches back — the anchor the reader is opened at, and the
    // prefetch it drives, both stay on the landing.
    assert_eq!(v.prefetch_anchor(), 248);
}

#[kithara::test]
fn incoming_session_leads_with_the_landing_and_skips_everything_before_it() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: make_init(48, &ctx.scope),
        segments: (0..6).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Flac),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(1, &ctx);
    let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
    let claim = abr
        .claim_pending_decision(VariantIndex::new(0))
        .expect("incoming transition claim");
    let transition = VariantTransition::new(
        VariantTransitionId::new(claim.ticket(), 0),
        VariantIndex::new(0),
        VariantIndex::new(1),
    );
    let profile = ReaderProfile::new(
        ReaderInput::Incremental,
        ReaderWarmup::None,
        NonZeroU64::new(64).expect("non-zero read ahead"),
    );

    let session = HlsSession::incoming(
        CancelToken::never(),
        profile,
        Arc::new(SeekState::new()),
        ctx.signal,
        transition,
        Arc::clone(&v),
        Duration::from_secs(8),
    )
    .expect("incoming session");

    assert_eq!(session.position(), 0);
    assert!(!seek_projection_is_live(&v));
    assert!(queue_has_init(&v));
    // The landing's backoff, the landing, then forward — and nothing before
    // them. No head-of-stream decoder probe: this reader opens on the landing
    // anchor and seeks straight to it, so it never reads segment 0, and
    // fetching it only delays the segment readiness is waiting for.
    assert_eq!(queue_seg_indices(&v), vec![3, 4, 5]);
    assert!(
        queue_seg_indices(&v)
            .iter()
            .all(|segment| !(0..3).contains(segment))
    );
    assert_eq!(
        v.prefetch_anchor(),
        v.segment_byte_offset(4).expect("landing segment offset")
    );
}

#[kithara::test]
fn incoming_session_dispatches_only_the_decoder_construction_window() {
    let ctx = test_ctx(10);
    let v = VariantParts {
        init: make_init(48, &ctx.scope),
        segments: (0..10).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Flac),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(1, &ctx);
    let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
    let claim = abr
        .claim_pending_decision(VariantIndex::new(0))
        .expect("incoming transition claim");
    let transition = VariantTransition::new(
        VariantTransitionId::new(claim.ticket(), 0),
        VariantIndex::new(0),
        VariantIndex::new(1),
    );
    let profile = ReaderProfile::new(
        ReaderInput::Incremental,
        ReaderWarmup::None,
        NonZeroU64::new(64).expect("non-zero read ahead"),
    );
    let session = HlsSession::incoming(
        CancelToken::never(),
        profile,
        Arc::new(SeekState::new()),
        ctx.signal.clone(),
        transition,
        Arc::clone(&v),
        Duration::from_secs(8),
    )
    .expect("incoming session");

    let commands = session.dispatch_constructing(&ctx, 10);

    assert_eq!(
        commands.len(),
        3,
        "init, the landing backoff, and the landing"
    );
    assert_eq!(queue_seg_indices(&v), vec![5, 6, 7, 8, 9]);
    assert!(session.dispatch_constructing(&ctx, 10).is_empty());

    // The cap belongs to construction, not to the session. Once the reader is
    // transferred the decoder primes through it, and a session still pinned to
    // the window it was *built* from stops serving those reads: its staged span
    // freezes and the outgoing frontier it is chasing walks away for good.
    let after_transfer = session.dispatch(&ctx, 10);
    assert_eq!(after_transfer.len(), 5, "the rest of the variant");
    assert!(queue_seg_indices(&v).is_empty());
}

/// The construction pack is not just *emitted* — landing it is what makes the
/// session readable. An init-only reader cannot be built until its header is
/// on disk, so the pack and the readiness window have to agree on which bytes
/// that is: a pack that never covers what readiness waits on leaves the
/// transition preparing forever with no failure to point at.
#[kithara::test]
fn incoming_session_is_ready_once_its_construction_fetches_land() {
    let ctx = test_ctx(10);
    let v = VariantParts {
        init: make_init(48, &ctx.scope),
        segments: (0..10).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Flac),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(1, &ctx);
    let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
    let claim = abr
        .claim_pending_decision(VariantIndex::new(0))
        .expect("incoming transition claim");
    let transition = VariantTransition::new(
        VariantTransitionId::new(claim.ticket(), 0),
        VariantIndex::new(0),
        VariantIndex::new(1),
    );
    let profile = ReaderProfile::new(
        ReaderInput::InitOnly,
        ReaderWarmup::None,
        NonZeroU64::new(64).expect("non-zero read ahead"),
    );
    let session = HlsSession::incoming(
        CancelToken::never(),
        profile,
        Arc::new(SeekState::new()),
        ctx.signal.clone(),
        transition,
        Arc::clone(&v),
        Duration::from_secs(8),
    )
    .expect("incoming session");

    assert!(
        !session.is_ready().expect("readiness before any bytes"),
        "a cold session cannot be readable"
    );

    let mut commands = session.dispatch_constructing(&ctx, 10);
    for cmd in &mut commands {
        let Some(mut writer) = cmd.take_writer() else {
            continue;
        };
        writer(&[7; 100]).expect("construction bytes");
    }

    assert!(
        session
            .is_ready()
            .expect("readiness after construction bytes"),
        "the construction pack must cover everything readiness waits on"
    );
}

#[kithara::test]
fn dispatch_emits_init_first_then_segments_under_budget() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400, 400], &ctx);
    let init_url = v.init().expect("init is present").url().clone();
    let seg0_url = v.segments()[0].url().clone();
    let seg1_url = v.segments()[1].url().clone();
    let seg2_url = v.segments()[2].url().clone();
    v.rebuild(&ctx, 0);
    let session = active_session(&v, &ctx, 0);
    let cmds = session.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 4);
    assert_eq!(cmds[0].url().clone(), init_url, "init dispatched first");
    assert_eq!(cmds[1].url().clone(), seg0_url);
    assert_eq!(cmds[2].url().clone(), seg1_url);
    assert_eq!(cmds[3].url().clone(), seg2_url);
    for cmd in &cmds {
        assert!(cmd.cancel().is_some(), "every cmd carries a cancel token");
    }
}

#[kithara::test]
fn dispatch_respects_budget() {
    let ctx = test_ctx(5);
    let v = make_var(0, 0, &[100; 10], &ctx);
    v.rebuild(&ctx, 0);
    let session = active_session(&v, &ctx, 0);
    let cmds = session.dispatch(&ctx, 3);
    assert_eq!(cmds.len(), 3);
    assert_eq!(queue_seg_indices(&v), vec![3, 4, 5, 6, 7, 8, 9]);
}

#[kithara::test]
fn dispatch_respects_segment_lookahead_cap() {
    let mut ctx = test_ctx(10);
    ctx.config.look_ahead_segments = Some(2);
    let v = make_var(0, 0, &[100; 6], &ctx);
    v.rebuild(&ctx, 0);
    let session = active_session(&v, &ctx, 0);

    let cmds = session.dispatch(&ctx, 10);

    assert_eq!(cmds.len(), 2);
    assert_eq!(queue_seg_indices(&v), vec![2, 3, 4, 5]);
}

#[kithara::test]
fn dispatch_skips_non_missing_segments() {
    let ctx = test_ctx(5);
    let v = make_var(0, 0, &[100, 100, 100], &ctx);
    v.segments()[1].state().mark_loaded();
    v.flow.queue.lock().clear();
    for seg in 0..3_u32 {
        push_planned(&v, seg);
    }
    let session = active_session(&v, &ctx, 0);
    let cmds = session.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 2);
    assert!(v.segments()[1].state().is_loaded());
}

#[kithara::test]
fn dispatch_requeues_orphaned_downloading_segment() {
    // Root C seek-rebuild race: a seek re-queues the target segment while an
    // old (now-orphaned) prefetch still holds it `Downloading`. `dispatch`
    // pops the segment then fails `try_claim` (slot is Downloading) — it must
    // NOT silently drop the popped-but-unclaimed segment. Once the orphaned
    // fetch settles back to `Missing`, a later dispatch must re-fetch it, else
    // the reader blocked on that segment hangs forever (`recv_outcome_blocking`
    // 5s watchdog). Pinned by `player_worker_hls_then_unavailable_mp3_then_mp3_recovery`.
    let ctx = test_ctx(5);
    let v = make_var(0, 0, &[100, 100, 100], &ctx);

    // seg 1 is mid-flight under an orphaned claim (Missing -> Downloading).
    let orphan = v.segments()[1]
        .state()
        .try_claim(
            PlannedFetch::Segment(1),
            v.flow.queue.revision(),
            Arc::downgrade(&v),
            ctx.signal.clone(),
        )
        .expect("seg 1 must be claimable");

    v.flow.queue.lock().clear();
    for seg in 0..3_u32 {
        push_planned(&v, seg);
    }
    let session = active_session(&v, &ctx, 0);

    // First dispatch: seg 0 + seg 2 emit; seg 1 is Downloading -> claim fails.
    let cmds = session.dispatch(&ctx, 10);
    assert_eq!(
        cmds.len(),
        2,
        "seg 0 and seg 2 dispatch; seg 1 is in-flight"
    );

    // The orphaned fetch settles back to Missing (cancel before commit) — the
    // unsettled `DownloadClaim` Drop reverts the slot to Missing.
    drop(orphan);
    assert!(
        !v.segments()[1].state().is_loaded(),
        "orphaned claim drop reverts seg 1 to Missing"
    );

    // A later dispatch (reader re-aim / next poll) MUST re-fetch seg 1. Before
    // the fix the segment was popped+dropped from the queue and lost, so this
    // returned 0 and the reader hung.
    let cmds2 = session.dispatch(&ctx, 10);
    assert_eq!(
        cmds2.len(),
        1,
        "seg 1 (orphaned -> Missing) must be re-dispatched, not lost from the queue"
    );
}

#[kithara::test]
fn a_dropped_claim_returns_its_fetch_to_the_plan() {
    // The Drop safety net reverts the slot to Missing, but dispatch popped
    // the plan entry when it sent the fetch: without a requeue the segment
    // is never asked for again and the reader hangs on a gap nobody owns.
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100], &ctx);
    let claim = v.segments()[0]
        .state()
        .try_claim(
            PlannedFetch::Segment(0),
            v.flow.queue.revision(),
            Arc::downgrade(&v),
            ctx.signal.clone(),
        )
        .expect("segment claim");
    assert!(!v.flow.queue.planned(PlannedFetch::Segment(0)));

    drop(claim);

    assert!(v.flow.queue.planned(PlannedFetch::Segment(0)));
}

#[kithara::test]
fn a_seek_supersedes_claims_from_the_previous_plan() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: None,
        segments: (0..5).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::AacLc),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);
    let claim = v.segments()[0]
        .state()
        .try_claim(
            PlannedFetch::Segment(0),
            v.flow.queue.revision(),
            Arc::downgrade(&v),
            ctx.signal.clone(),
        )
        .expect("prefix segment claim");
    for segment in &v.segments()[1..] {
        segment.state().mark_loaded();
    }

    let target = v
        .rebuild_at_time(&ctx, Duration::from_secs(4))
        .expect("target segment");
    drop(claim);

    assert_eq!(target, 2);
    assert!(
        queue_seg_indices(&v).is_empty(),
        "a fully cached seek must neither rebuild the plan nor resurrect its old prefix"
    );
}

#[kithara::test]
fn a_noop_rebuild_disowns_a_cancelled_fetch() {
    // A rebuild that changes nothing (the queue already matches the plan)
    // still claims plan ownership: the fetch the triggering rearm cancelled
    // settles into a superseded plan and stays off it, instead of
    // re-entering as the new queue head.
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100], &ctx);
    v.segments()[1].state().mark_loaded();
    let claim = v.segments()[0]
        .state()
        .try_claim(
            PlannedFetch::Segment(0),
            v.flow.queue.revision(),
            Arc::downgrade(&v),
            ctx.signal.clone(),
        )
        .expect("segment claim");
    v.rebuild(&ctx, 1);

    drop(claim);

    assert!(!v.flow.queue.planned(PlannedFetch::Segment(0)));
}

#[kithara::test]
fn phase_at_reports_waiting_demand_for_claimed_segment() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100], &ctx);
    let claim = v.segments()[0]
        .state()
        .try_claim(
            PlannedFetch::Segment(0),
            v.flow.queue.revision(),
            Arc::downgrade(&v),
            ctx.signal.clone(),
        )
        .expect("segment claim");

    assert_eq!(v.phase_at(0..16), SourcePhase::WaitingDemand);

    claim.into_missing();
    assert_eq!(v.phase_at(0..16), SourcePhase::Waiting);
}

#[kithara::test]
fn phase_at_reports_waiting_demand_for_queued_segment() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100], &ctx);

    push_planned(&v, 0);

    assert_eq!(v.phase_at(0..16), SourcePhase::WaitingDemand);
}

fn claim_segment_zero(v: &Arc<HlsVariant>, ctx: &PlanCtx) -> FetchClaim<Downloading> {
    v.segments()[0]
        .state()
        .try_claim(
            PlannedFetch::Segment(0),
            v.flow.queue.revision(),
            Arc::downgrade(v),
            ctx.signal.clone(),
        )
        .expect("segment claim")
}

#[kithara::test]
fn a_parked_wait_files_demand_on_the_claimed_segment() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100], &ctx);
    let _claim = claim_segment_zero(&v, &ctx);
    assert!(
        !v.segments()[0].state().is_reader_demanded(),
        "an in-flight fetch nobody waits on must not be escalated"
    );

    assert!(v.wait_range(0..16, None).is_err(), "the range is not ready");

    assert!(
        v.segments()[0].state().is_reader_demanded(),
        "the wait parking on the claimed segment must escalate its fetch"
    );
}

#[kithara::test]
fn a_phase_query_leaves_demand_unfiled() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100], &ctx);
    let _claim = claim_segment_zero(&v, &ctx);

    assert_eq!(v.phase_at(0..16), SourcePhase::WaitingDemand);

    assert!(
        !v.segments()[0].state().is_reader_demanded(),
        "a phase query observes the slot; only a parked wait escalates it"
    );
}

#[kithara::test]
fn a_settled_slot_answers_no_demand() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100], &ctx);
    let claim = claim_segment_zero(&v, &ctx);
    assert!(v.wait_range(0..16, None).is_err(), "the range is not ready");

    claim.into_missing();

    assert!(
        !v.segments()[0].state().is_reader_demanded(),
        "a settled slot has no in-flight fetch left to escalate"
    );

    let _claim = claim_segment_zero(&v, &ctx);

    assert!(
        !v.segments()[0].state().is_reader_demanded(),
        "the settle must clear the filing: a fresh claim starts unescalated"
    );
}

#[kithara::test]
fn a_readiness_poll_leaves_demand_unfiled() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100], &ctx);
    let profile = ReaderProfile::new(
        ReaderInput::Incremental,
        ReaderWarmup::None,
        NonZeroU64::new(16).expect("non-zero read ahead"),
    );
    let preparation = v
        .prepare_reader(profile, Duration::ZERO)
        .expect("reader preparation");
    let _claim = claim_segment_zero(&v, &ctx);

    assert!(
        !v.reader_is_ready(&preparation).expect("readiness poll"),
        "the claimed segment is not loaded"
    );

    assert!(
        !v.segments()[0].state().is_reader_demanded(),
        "a readiness poll is not a read; only a parked wait escalates the fetch"
    );
}

#[kithara::test]
fn a_planned_segment_is_owed_not_escalated() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100], &ctx);
    push_planned(&v, 0);
    assert!(v.wait_range(0..16, None).is_err(), "the range is not ready");

    let _claim = claim_segment_zero(&v, &ctx);

    assert!(
        !v.segments()[0].state().is_reader_demanded(),
        "a wait parked before the claim must not carry into the fresh fetch: the owed dispatch stamps it High at emit"
    );
}

fn exact_seek_session() -> (Arc<HlsVariant>, HlsSession, u64) {
    let ctx = test_ctx(3);
    let seek = Arc::new(SeekState::new());
    let segments: Vec<Segment> = (0..3)
        .map(|idx| make_placeholder_seg(idx, 256, &ctx.scope))
        .collect();
    let v = VariantParts {
        segments,
        init: None,
        seek_obs: Arc::clone(&seek) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);
    let stale_anchor = 512;
    v.set_prefetch_anchor(stale_anchor);
    v.set_seek_alias(stale_anchor, 2);
    v.rebuild(&ctx, 2);
    v.set_exact_seek_demand(stale_anchor, 2);
    let session = HlsSession::active(
        CancelToken::never(),
        seek,
        ctx.signal,
        0,
        Arc::clone(&v),
        stale_anchor,
    );
    (v, session, stale_anchor)
}

fn publish_exact_size_while_paused(
    variant: Arc<HlsVariant>,
    demand: SizeDemand,
    stored: Arc<Barrier>,
    resume: Arc<Barrier>,
) -> thread::JoinHandle<bool> {
    thread::spawn(move || {
        variant.apply_resolved_size_before_layout_publication(demand, 100, || {
            stored.wait();
            resume.wait();
        })
    })
}

#[kithara::test]
fn exact_seek_completion_corrects_the_session_cursor_before_first_read() {
    let (v, session, _stale_anchor) = exact_seek_session();

    v.apply_resolved_size(SizeDemand::Segment(0), 100);
    v.apply_resolved_size(SizeDemand::Segment(1), 100);
    v.apply_resolved_size(SizeDemand::Segment(2), 100);

    assert_eq!(session.position(), 200);
}

#[kithara::test]
fn exact_seek_completion_invalidates_a_stale_session_read_snapshot() {
    let (v, session, stale_anchor) = exact_seek_session();
    let observed = session.position();
    assert_eq!(observed, stale_anchor);

    v.apply_resolved_size(SizeDemand::Segment(0), 100);
    v.apply_resolved_size(SizeDemand::Segment(1), 100);
    v.apply_resolved_size(SizeDemand::Segment(2), 100);

    let wait = v.wait_range(observed..observed + 1, Some(Duration::ZERO));
    assert!(!matches!(wait, Ok(WaitOutcome::Eof)));

    let mut sample = [0_u8; 1];
    let outcome = v.read_at(observed, &mut sample).expect("stale alias read");
    assert!(!matches!(outcome, ReadOutcome::Eof));

    session.advance(1);
    assert_eq!(session.position(), 201);
}

#[kithara::test]
fn exact_seek_metadata_gate_publishes_the_resolved_alias_before_opening() {
    let (v, session, stale_anchor) = exact_seek_session();

    v.apply_resolved_size(SizeDemand::Segment(0), 100);
    v.apply_resolved_size(SizeDemand::Segment(1), 100);
    assert!(v.apply_resolved_size_without_seek_completion(SizeDemand::Segment(2), 100));
    assert_eq!(session.position(), stale_anchor);

    assert_eq!(v.exact_seek_metadata_phase(), None);

    assert_eq!(session.position(), 200);
}

#[kithara::test]
fn exact_seek_projection_tracks_size_revisions_until_reader_moves() {
    let (v, session, _stale_anchor) = exact_seek_session();
    v.apply_resolved_size(SizeDemand::Segment(2), 100);
    v.apply_resolved_size(SizeDemand::Segment(0), 100);
    v.apply_resolved_size(SizeDemand::Segment(1), 100);
    assert_eq!(session.position(), 200);

    v.layout.apply_commit(v.segments(), || {
        v.apply_loaded_size(PlannedFetch::Segment(0), 150);
        v.init_route_size()
    });
    v.complete_exact_seek_if_ready();

    assert_eq!(v.exact_seek_metadata_phase(), None);
    assert_eq!(session.position(), 250);
}

#[kithara::test]
fn exact_seek_projection_retires_after_the_first_consumed_byte() {
    let (v, session, _stale_anchor) = exact_seek_session();
    v.apply_resolved_size(SizeDemand::Segment(2), 100);
    v.apply_resolved_size(SizeDemand::Segment(0), 100);
    v.apply_resolved_size(SizeDemand::Segment(1), 100);
    assert_eq!(session.position(), 200);

    v.layout.apply_commit(v.segments(), || {
        v.apply_loaded_size(PlannedFetch::Segment(0), 101);
        v.init_route_size()
    });
    v.complete_exact_seek_if_ready();
    assert_eq!(session.position(), 201);

    session.advance(1);

    assert_eq!(session.position(), 202);
    assert!(!seek_projection_is_live(&v));
}

#[kithara::test]
fn late_rebuild_at_time_does_not_reopen_consumed_exact_seek_projection() {
    let ctx = test_ctx(3);
    let seek = Arc::new(SeekState::new());
    let v = VariantParts {
        segments: (0..4)
            .map(|idx| make_placeholder_seg(idx, 256, &ctx.scope))
            .collect(),
        init: None,
        seek_obs: Arc::clone(&seek) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);
    let stale_anchor = 512;
    v.set_prefetch_anchor(stale_anchor);
    v.set_seek_alias(stale_anchor, 2);
    v.set_exact_seek_demand(stale_anchor, 2);
    let session = HlsSession::active(
        CancelToken::never(),
        seek,
        ctx.signal.clone(),
        0,
        Arc::clone(&v),
        stale_anchor,
    );
    for segment in 0..=2 {
        v.apply_resolved_size(SizeDemand::Segment(segment), 100);
    }
    assert_eq!(session.position(), 200);

    session.advance(1);
    assert_eq!(session.position(), 201);
    assert!(!seek_projection_is_live(&v));

    assert_eq!(v.rebuild_at_time(&ctx, Duration::from_secs(4)), Some(2));
    assert_eq!(session.position(), 201);
    assert_eq!(v.exact_seek_metadata_phase(), None);
    assert!(!seek_projection_is_live(&v));
}

#[kithara::test]
fn exact_seek_gate_stays_closed_until_exact_layout_is_published() {
    let (v, session, stale_anchor) = exact_seek_session();
    v.apply_resolved_size(SizeDemand::Segment(2), 100);
    v.apply_resolved_size(SizeDemand::Segment(0), 100);

    let stored = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let publisher = publish_exact_size_while_paused(
        Arc::clone(&v),
        SizeDemand::Segment(1),
        Arc::clone(&stored),
        Arc::clone(&resume),
    );
    stored.wait();

    let phase_during_publication = v.exact_seek_metadata_phase();
    let position_during_publication = session.position();

    resume.wait();
    assert!(publisher.join().expect("size publisher"));

    assert_eq!(phase_during_publication, Some(SourcePhase::WaitingDemand));
    assert_eq!(position_during_publication, stale_anchor);
    assert_eq!(v.exact_seek_metadata_phase(), None);
    assert_eq!(session.position(), 200);
}

#[kithara::test]
fn exact_byte_gate_stays_closed_until_exact_layout_is_published() {
    let (v, _session, _stale_anchor) = exact_seek_session();
    let observed_byte = 400;
    v.clear_exact_seek();
    v.set_exact_byte_seek_demand(observed_byte);
    v.apply_resolved_size(SizeDemand::Segment(2), 100);
    v.apply_resolved_size(SizeDemand::Segment(0), 100);

    let stored = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let publisher = publish_exact_size_while_paused(
        Arc::clone(&v),
        SizeDemand::Segment(1),
        Arc::clone(&stored),
        Arc::clone(&resume),
    );
    stored.wait();

    let phase_during_publication = v.exact_byte_metadata_phase();

    resume.wait();
    assert!(publisher.join().expect("size publisher"));

    assert_eq!(phase_during_publication, Some(SourcePhase::WaitingDemand));
    assert_eq!(v.exact_byte_metadata_phase(), None);
}

#[kithara::test]
fn exact_init_gate_stays_closed_until_layout_is_published() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: Some(make_placeholder_init(256, &ctx.scope)),
        segments: vec![make_seg(0, 100, &ctx.scope)],
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: None,
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);
    v.apply_resolved_size(SizeDemand::Segment(0), 100);
    v.set_exact_byte_seek_demand(300);
    assert_eq!(
        v.exact_byte_metadata_phase(),
        Some(SourcePhase::WaitingDemand),
        "a non-exact init keeps the exact-byte gate closed"
    );

    let stored = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let publisher = publish_exact_size_while_paused(
        Arc::clone(&v),
        SizeDemand::Init,
        Arc::clone(&stored),
        Arc::clone(&resume),
    );
    stored.wait();

    let phase_during_publication = v.exact_byte_metadata_phase();

    resume.wait();
    assert!(publisher.join().expect("init size publisher"));

    assert_eq!(phase_during_publication, Some(SourcePhase::WaitingDemand));
    assert_eq!(v.exact_byte_metadata_phase(), None);
}

#[kithara::test]
fn eof_gate_rejects_a_torn_layout_publication() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: None,
        segments: (0..2)
            .map(|idx| make_placeholder_seg(idx, 100, &ctx.scope))
            .collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);
    let writer_started = Arc::new(Barrier::new(2));
    let writer_finished = Arc::new(Barrier::new(2));
    let publisher = {
        let v = Arc::clone(&v);
        let writer_started = Arc::clone(&writer_started);
        let writer_finished = Arc::clone(&writer_finished);
        thread::spawn(move || {
            writer_started.wait();
            v.layout.apply_commit(v.segments(), || {
                for segment in v.segments() {
                    segment.set_loaded_size(256);
                }
                v.init_route_size()
            });
            writer_finished.wait();
        })
    };

    let torn_eof = v.eof_at_before_ready_check(250, || {
        writer_started.wait();
        writer_finished.wait();
    });
    publisher.join().expect("layout publisher");

    assert!(!torn_eof);
    assert_ne!(v.phase_at(250..251), SourcePhase::Eof);
}

#[kithara::test]
fn ready_gate_rejects_a_torn_layout_publication() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: None,
        segments: (0..2)
            .map(|idx| make_placeholder_seg(idx, 100, &ctx.scope))
            .collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);
    let writer_started = Arc::new(Barrier::new(2));
    let writer_finished = Arc::new(Barrier::new(2));
    let publisher = {
        let v = Arc::clone(&v);
        let writer_started = Arc::clone(&writer_started);
        let writer_finished = Arc::clone(&writer_finished);
        thread::spawn(move || {
            writer_started.wait();
            v.layout.apply_commit(v.segments(), || {
                for segment in v.segments() {
                    segment.set_loaded_size(256);
                }
                v.init_route_size()
            });
            writer_finished.wait();
        })
    };
    let range = 250..251;

    let torn_ready = v.range_ready_after_total(&range, || {
        writer_started.wait();
        writer_finished.wait();
    });
    publisher.join().expect("layout publisher");

    assert!(!torn_ready);
    assert_ne!(v.phase_at(range), SourcePhase::Ready);
}

#[kithara::test]
fn range_gate_rejects_eof_ready_cross_publication() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: None,
        segments: (0..2)
            .map(|idx| make_placeholder_seg(idx, 256, &ctx.scope))
            .collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);
    let writer_started = Arc::new(Barrier::new(2));
    let writer_finished = Arc::new(Barrier::new(2));
    let publisher = {
        let v = Arc::clone(&v);
        let writer_started = Arc::clone(&writer_started);
        let writer_finished = Arc::clone(&writer_finished);
        thread::spawn(move || {
            writer_started.wait();
            v.layout.apply_commit(v.segments(), || {
                for segment in v.segments() {
                    segment.set_loaded_size(100);
                }
                v.init_route_size()
            });
            writer_finished.wait();
        })
    };

    let phase = v.phase_at_after_eof(250..251, || {
        writer_started.wait();
        writer_finished.wait();
    });
    publisher.join().expect("layout publisher");

    assert_eq!(phase, SourcePhase::WaitingDemand);
}

#[kithara::test]
fn exact_session_seek_uses_the_session_cursor_to_detect_movement() {
    let ctx = test_ctx(3);
    let segments: Vec<Segment> = (0..10)
        .map(|idx| make_placeholder_seg(idx, 64, &ctx.scope))
        .collect();
    let seek = Arc::new(SeekState::new());
    let v = VariantParts {
        segments,
        init: None,
        seek_obs: Arc::clone(&seek) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);
    let stale_projection = 512;
    v.clear_exact_byte_seek();
    let session = HlsSession::active(
        CancelToken::never(),
        seek,
        ctx.signal,
        0,
        Arc::clone(&v),
        stale_projection,
    );
    session.advance(1);

    session.seek_to_byte(stale_projection);

    assert_eq!(session.position(), stale_projection);
    assert_eq!(
        v.phase_at(stale_projection..stale_projection + 1),
        SourcePhase::WaitingDemand
    );
}

#[kithara::test]
fn raw_byte_seek_registers_lazy_exact_demand_only_after_cursor_moves() {
    let ctx = test_ctx(3);
    let segments: Vec<Segment> = (0..10)
        .map(|idx| make_placeholder_seg(idx, 64, &ctx.scope))
        .collect();
    let v = VariantParts {
        segments,
        init: None,
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);
    let session = active_session(&v, &ctx, 0);

    session.seek_to_byte(0);
    assert!(
        session.dispatch(&ctx, 3).is_empty(),
        "a no-op seek to the current cursor must not issue startup size probes"
    );

    session.seek_to_byte(512);
    assert_eq!(v.phase_at(512..513), SourcePhase::WaitingDemand);
    let cmds = session.dispatch(&ctx, 3);
    assert_eq!(
        cmds.len(),
        3,
        "raw byte seek size probes must respect the dispatch budget"
    );
    assert_eq!(cmds[0].url().clone(), v.segments()[0].url().clone());
    assert_eq!(cmds[1].url().clone(), v.segments()[1].url().clone());
    assert_eq!(cmds[2].url().clone(), v.segments()[2].url().clone());
    let cmds = session.dispatch(&ctx, 3);
    assert_eq!(cmds.len(), 3);
    assert_eq!(cmds[0].url().clone(), v.segments()[3].url().clone());
    assert_eq!(cmds[1].url().clone(), v.segments()[4].url().clone());
    assert_eq!(cmds[2].url().clone(), v.segments()[5].url().clone());
}

#[kithara::test]
fn on_evict_returns_minus_one_for_init() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[100, 100, 100], &ctx);
    let init = v.init().expect("init is present");
    init.state().mark_loaded();
    v.segments()[1].state().mark_loaded();
    let key = init.resource_id().clone();
    let res = v.on_evict(&key);
    assert_eq!(res, Some(-1));
    assert!(!v.init().expect("init is present").state().is_loaded());
    assert!(
        v.segments()[1].state().is_loaded(),
        "init eviction must not touch segment states"
    );
}

#[kithara::test]
fn on_evict_returns_seg_idx_for_segment() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100], &ctx);
    v.segments()[1].state().mark_loaded();
    let key = v.segments()[1].resource_id().clone();
    let res = v.on_evict(&key);
    assert_eq!(res, Some(1));
    assert!(!v.segments()[1].state().is_loaded());
}

#[kithara::test]
fn on_evict_returns_none_for_foreign_asset() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100], &ctx);
    let foreign: Url = "https://other.example.com/x.m4s".parse().expect("url");
    let foreign_key = ctx
        .scope
        .key(&AssetResource::Url(foreign))
        .expect("foreign key");
    let res = v.on_evict(&foreign_key);
    assert_eq!(res, None);
}

#[kithara::test]
fn rebuild_fills_forward_window_from_seg() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100; 10], &ctx);
    v.rebuild(&ctx, 2);
    assert_eq!(queue_seg_indices(&v), vec![2, 3, 4, 5, 6, 7, 8, 9]);
}

/// A rebuild whose forward window is already cached must still drop the
/// fetches a previous epoch left *behind* the new target.
///
/// The cheap early return reasons from `fetch_plan_satisfied`, which only
/// inspects `[from_seg, num_segments)` — it never sees the prefix entries and
/// its "dispatch skips them" argument does not cover them, because nothing
/// says a segment behind the target is loaded. Returning early there keeps
/// dispatching prefix fetches after a seek has moved past them.
#[kithara::test]
fn rebuild_drops_prefix_fetches_when_forward_window_is_cached() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100; 10], &ctx);
    for segment in v.segments() {
        segment.state().mark_loaded();
    }
    push_planned(&v, 2);
    push_planned(&v, 3);

    v.rebuild(&ctx, 8);

    assert_eq!(queue_seg_indices(&v), vec![8, 9]);
}

#[kithara::test]
fn skeleton_types_instantiate() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[], &ctx);
    assert_eq!(v.num_segments(), 0);
}

#[kithara::test]
fn dispatch_drm_segment_routes_through_with_ctx() {
    let ctx = test_ctx(3);
    let init = make_init(0, &ctx.scope);
    let url: Url = "https://example.com/seg0.m4s".parse().expect("valid url");
    let resource_id = ctx
        .scope
        .key(&AssetResource::Url(url.clone()))
        .expect("segment key");
    let key = *b"0123456789abcdef";
    let seg = Segment::Media(MediaSegment {
        url,
        resource_id,
        state: SegmentSlotState::missing(),
        size: SegmentSize::seed(100),
        content: SegmentContent::Encrypted(DecryptContext::new(key, [0u8; 16])),
        decode_time: Duration::ZERO,
        duration: Duration::from_secs(2),
    });
    let v = VariantParts {
        init,
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: None,
        container: None,
        segments: vec![seg],
    }
    .into_variant(0, &ctx);
    push_planned(&v, 0);
    let session = active_session(&v, &ctx, 0);
    let cmds = session.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 1);
    assert!(cmds[0].cancel().is_some());
    push_planned(&v, 0);
    assert!(
        session.dispatch(&ctx, 10).is_empty(),
        "claimed (in-flight) segment must not be re-dispatched"
    );
}

#[kithara::test]
fn dropped_fetch_cmd_reverts_segment_to_missing() {
    let ctx = test_ctx(5);
    let v = make_var(0, 0, &[100, 100], &ctx);
    push_planned(&v, 0);
    let session = active_session(&v, &ctx, 0);
    let cmds = session.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 1, "first dispatch claims and emits seg 0");
    // Drop the command without running its `on_complete`: the owned
    // download handle is dropped without a settle, so the Drop safety
    // net must revert the slot to Missing rather than strand it.
    drop(cmds);
    push_planned(&v, 0);
    assert_eq!(
        session.dispatch(&ctx, 10).len(),
        1,
        "dropped claim must revert the slot to Missing so it re-dispatches"
    );
}

#[kithara::test]
fn positions_of_two_sessions_are_independent_after_flip() {
    let ctx = test_ctx(3);
    let v_old = make_var(0, 0, &[400; 20], &ctx);
    let v_new = make_var(1, 0, &[800; 20], &ctx);
    let v_new_seg10_offset = v_new.segment_byte_offset(10).expect("seg 10");
    let old_session = active_session(&v_old, &ctx, 5000);
    let new_session = active_session(&v_new, &ctx, v_new_seg10_offset);
    assert_eq!(old_session.position(), 5000);
    assert_eq!(new_session.position(), v_new_seg10_offset);

    new_session.advance(123);
    assert_eq!(
        old_session.position(),
        5000,
        "advancing the new session must not touch the old session"
    );
    assert_eq!(new_session.position(), v_new_seg10_offset + 123);
}

#[kithara::test]
fn position_advances_are_strictly_monotonic() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100], &ctx);
    let session = active_session(&v, &ctx, 0);
    let mut expected = 0_u64;
    let mut observed = Vec::new();
    for n in [10_u64, 25, 7, 64, 1, 100] {
        session.advance(n);
        expected += n;
        observed.push(session.position());
        assert_eq!(session.position(), expected);
    }
    let mut sorted = observed.clone();
    sorted.sort_unstable();
    assert_eq!(observed, sorted);
}

#[kithara::test]
fn dispatch_cmd_cancel_shares_cancellation_with_session() {
    let ctx = test_ctx(5);
    let v = make_var(0, 0, &[100, 100], &ctx);
    let root = CancelToken::root();
    let session = HlsSession::active(
        root,
        Arc::new(SeekState::new()),
        ctx.signal.clone(),
        0,
        Arc::clone(&v),
        0,
    );
    for seg in 0..2_u32 {
        push_planned(&v, seg);
    }
    let cmds = session.dispatch(&ctx, 10);
    for cmd in &cmds {
        let token = cmd.cancel().expect("cmd carries cancel");
        assert!(!token.is_cancelled());
    }
    session.abort();
    for cmd in &cmds {
        let token = cmd.cancel().expect("cmd carries cancel");
        assert!(
            token.is_cancelled(),
            "command cancellation must follow its reader session"
        );
    }
}

#[kithara::test]
fn session_flip_cancels_old_and_keeps_new_live() {
    let ctx = test_ctx(3);
    let v_old = make_var(0, 0, &[100; 20], &ctx);
    let v_new = make_var(1, 0, &[200; 20], &ctx);
    let old_token = CancelToken::root();
    let new_token = CancelToken::root();

    let from_seg = 7_u32;
    let v_new_seg7_offset = v_new.segment_byte_offset(from_seg).expect("seg 7");
    let old_session = HlsSession::active(
        old_token.clone(),
        Arc::new(SeekState::new()),
        ctx.signal.clone(),
        0,
        v_old,
        0,
    );
    let new_session = HlsSession::active(
        new_token.clone(),
        Arc::new(SeekState::new()),
        ctx.signal.clone(),
        1,
        Arc::clone(&v_new),
        v_new_seg7_offset,
    );
    old_session.abort();
    v_new.rebuild(&ctx, from_seg);

    assert!(
        old_token.is_cancelled(),
        "aborting the old session cancels its token"
    );
    assert!(
        !new_token.is_cancelled(),
        "rebuilding the new variant must not touch its session token"
    );
    assert_eq!(new_session.position(), v_new_seg7_offset);
}

#[kithara::test]
fn dispatch_skips_loaded_segments_in_queue_without_burning_budget() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100; 20], &ctx);
    v.segments()[10].state().mark_loaded();

    v.rebuild(&ctx, 10);
    let session = active_session(&v, &ctx, 0);
    let cmds = session.dispatch(&ctx, 3);
    assert_eq!(cmds.len(), 3);
    let seg10_url = v.segments()[10].url().clone();
    assert!(
        cmds.iter().all(|c| c.url() != &seg10_url),
        "Loaded seg 10 must not be re-emitted"
    );
}

/// Non-blocking-pull contract: a not-ready range must make `wait_range`
/// return `WaitBudgetExceeded` *immediately* (no internal sleep). The backoff
/// between probes is the caller's responsibility (the worker scheduler park),
/// so the read path never blocks on a syscall. Waking the peer downloader is
/// the reader driver's job (`Stream::probe_read` / `read` / `prime_seek_range`,
/// per its on-core/off-core context), not this method's. The old
/// implementation slept 2ms per spin and looped until the 10ms budget elapsed;
/// the probe must now return in well under that.
#[kithara::test]
fn wait_range_probes_without_sleeping() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400], &ctx);

    let started = Instant::now();
    let outcome = v.wait_range(0..1, Some(Duration::from_millis(10)));
    let elapsed = Instant::now().saturating_duration_since(started);

    assert!(
        matches!(
            outcome,
            Err(StreamError::Source(SourceError::WaitBudgetExceeded))
        ),
        "not-ready range must signal WaitBudgetExceeded immediately, got {outcome:?}"
    );
    assert!(
        elapsed < Duration::from_millis(2),
        "probe must not sleep (old impl slept 2ms/spin up to the 10ms budget); took {elapsed:?}"
    );
}

/// The flush short-circuit remains reachable and immediate after the
/// non-blocking-pull conversion: a flushing seek state yields `Interrupted`
/// without spinning on the budget signal.
#[kithara::test]
fn wait_range_flush_short_circuits_without_sleeping() {
    let ctx = test_ctx(3);
    let seek = Arc::new(SeekState::new());
    let v = make_var_with_seek_obs(
        0,
        200,
        &[400],
        &ctx,
        Arc::clone(&seek) as Arc<dyn SeekObserve>,
    );

    let _ = SeekControl::begin(&*seek, Duration::from_millis(10));
    let started = Instant::now();
    let interrupted = v.wait_range(0..1, Some(Duration::from_millis(10)));
    let elapsed = Instant::now().saturating_duration_since(started);
    assert!(
        matches!(interrupted, Ok(WaitOutcome::Interrupted)),
        "flushing seek state must Interrupt the probe, got {interrupted:?}"
    );
    assert!(
        elapsed < Duration::from_millis(2),
        "flush short-circuit must not sleep; took {elapsed:?}"
    );
}

fn seg_idx_by_url(v: &HlsVariant, url: &Url) -> u32 {
    let idx = v
        .segments()
        .iter()
        .position(|seg| seg.url() == url)
        .expect("cmd url belongs to the variant");
    u32::try_from(idx).expect("segment index fits u32")
}

/// While a variant transition is building, the audible session may hold only
/// its owed window — the playing segment and the next. The look-ahead it
/// would otherwise queue is what starves the incoming construction. A latch
/// inside the playing segment adds nothing to that window.
#[kithara::test]
fn dispatch_owed_stops_after_the_next_segment() {
    let ctx = test_ctx(8);
    let v = make_var(0, 0, &[100, 100, 100, 100, 100, 100], &ctx);
    v.rebuild(&ctx, 0);
    let session = active_session(&v, &ctx, 0);

    let cmds = session.dispatch_owed(&ctx, 10, Duration::ZERO);

    let got: Vec<Url> = cmds.iter().map(|cmd| cmd.url().clone()).collect();
    assert_eq!(
        got,
        vec![v.segments()[0].url().clone(), v.segments()[1].url().clone(),],
        "position 0 owes segment 0 and its successor, nothing further"
    );
}

/// The look-ahead caps are an option, the owed window is a debt: while a
/// transition builds, the outgoing must still fetch every segment up to the
/// latch even when the prefetch segment window is narrower — the splice
/// cannot land without those bytes.
#[kithara::test]
fn dispatch_owed_overrides_the_segment_lookahead_cap() {
    let mut ctx = test_ctx(10);
    ctx.config.look_ahead_segments = Some(2);
    let v = make_var(0, 0, &[100, 100, 100, 100, 100, 100], &ctx);
    v.rebuild(&ctx, 0);
    let session = active_session(&v, &ctx, 0);

    let cmds = session.dispatch_owed(&ctx, 10, Duration::from_secs(5));

    let got: Vec<Url> = cmds.iter().map(|cmd| cmd.url().clone()).collect();
    assert_eq!(
        got,
        vec![
            v.segments()[0].url().clone(),
            v.segments()[1].url().clone(),
            v.segments()[2].url().clone(),
            v.segments()[3].url().clone(),
        ],
        "the owed debt runs to the latch through a narrower segment window"
    );
}

/// Same debt against the byte-sized prefetch window: bytes the splice still
/// needs dispatch even when they lie past `look_ahead_bytes`.
#[kithara::test]
fn dispatch_owed_overrides_the_byte_lookahead_cap() {
    let mut ctx = test_ctx(10);
    ctx.config.look_ahead_bytes = Some(150);
    let v = make_var(0, 0, &[100, 100, 100, 100, 100, 100], &ctx);
    v.rebuild(&ctx, 0);
    let session = active_session(&v, &ctx, 0);

    let cmds = session.dispatch_owed(&ctx, 10, Duration::from_secs(5));

    let got: Vec<Url> = cmds.iter().map(|cmd| cmd.url().clone()).collect();
    assert_eq!(
        got,
        vec![
            v.segments()[0].url().clone(),
            v.segments()[1].url().clone(),
            v.segments()[2].url().clone(),
            v.segments()[3].url().clone(),
        ],
        "the owed debt runs to the latch through a narrower byte window"
    );
}

/// The owed window follows the transition latch when the cut lies ahead of
/// the reader: the outgoing must decode every byte up to the cut, and a seek
/// can park its reads segments ahead of the byte cursor. Starving those
/// fetches parks the outgoing decoder forever, and the incoming prime waits
/// on its frontier just as long.
#[kithara::test]
fn dispatch_owed_reaches_the_transition_latch() {
    let ctx = test_ctx(8);
    let v = make_var(0, 0, &[100, 100, 100, 100, 100, 100], &ctx);
    v.rebuild(&ctx, 0);
    let session = active_session(&v, &ctx, 0);

    // Reader at segment 0; the latch sits at 5s, inside segment 2.
    let cmds = session.dispatch_owed(&ctx, 10, Duration::from_secs(5));

    let got: Vec<Url> = cmds.iter().map(|cmd| cmd.url().clone()).collect();
    assert_eq!(
        got,
        vec![
            v.segments()[0].url().clone(),
            v.segments()[1].url().clone(),
            v.segments()[2].url().clone(),
            v.segments()[3].url().clone(),
        ],
        "the owed window runs through the latch segment and its successor"
    );
}

/// A parked read is owed no matter where the projection points: the outgoing
/// demuxer consumes past the projected byte cursor and waits on segments the
/// stale owed window refuses to dispatch, leaving the queue head one past the
/// cap forever (the phase_continuity livelock: `queue_head=27 cap=26`). The
/// range the reader waits on is what the splice still consumes, so the owed
/// window must run through it.
#[kithara::test]
fn dispatch_owed_covers_the_range_the_reader_waits_on() {
    let ctx = test_ctx(8);
    let v = make_var(0, 0, &[100, 100, 100, 100, 100, 100], &ctx);
    v.rebuild(&ctx, 0);
    let session = active_session(&v, &ctx, 0);
    // The demuxer parked on bytes 300..500 (segments 3 and 4), past the owed
    // window of a reader projected at byte 0 with the latch inside segment 0.
    let parked = v.wait_range(300..500, Some(Duration::ZERO));
    assert!(
        matches!(
            parked,
            Err(StreamError::Source(SourceError::WaitBudgetExceeded))
        ),
        "the unloaded range must park the reader first: {parked:?}"
    );

    let cmds = session.dispatch_owed(&ctx, 10, Duration::ZERO);

    let got: Vec<Url> = cmds.iter().map(|cmd| cmd.url().clone()).collect();
    assert_eq!(
        got,
        vec![
            v.segments()[0].url().clone(),
            v.segments()[1].url().clone(),
            v.segments()[2].url().clone(),
            v.segments()[3].url().clone(),
            v.segments()[4].url().clone(),
        ],
        "the owed window runs through the segments the parked read waits on"
    );
}

/// A session seek retires the parked-read debt: the wait belonged to reads
/// the seek abandoned, and dragging it forward would hold look-ahead capacity
/// against the new position.
#[kithara::test]
fn a_session_seek_clears_the_parked_read_debt() {
    let ctx = test_ctx(8);
    let v = make_var(0, 0, &[100, 100, 100, 100, 100, 100], &ctx);
    v.rebuild(&ctx, 0);
    let session = active_session(&v, &ctx, 0);
    let _ = v.wait_range(300..500, Some(Duration::ZERO));

    session.seek_to_byte(100);
    let cmds = session.dispatch_owed(&ctx, 10, Duration::ZERO);

    // The plan itself is the peer rebuild's job; this session-level pass
    // still drains the queue from its head. The property is the cap: with
    // the wait retired it ends at the new position's owed window (segment 2),
    // not at the abandoned wait's segment 4.
    let got: Vec<Url> = cmds.iter().map(|cmd| cmd.url().clone()).collect();
    assert_eq!(
        got,
        vec![
            v.segments()[0].url().clone(),
            v.segments()[1].url().clone(),
            v.segments()[2].url().clone(),
        ],
        "a seek retires the old wait; the owed cap follows the new position"
    );
}

/// Retiring the look-ahead burns the tokens of fetches past the owed window,
/// so the downloader frees their slots instead of finishing bytes the latched
/// cut already declared dead.
#[kithara::test]
fn retire_lookahead_burns_fetches_beyond_the_owed_window() {
    let ctx = test_ctx(8);
    let v = make_var(0, 0, &[100, 100, 100, 100, 100, 100], &ctx);
    v.rebuild(&ctx, 0);
    let session = active_session(&v, &ctx, 0);
    let cmds = session.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 6, "precondition: every segment dispatches");

    session.retire_lookahead();

    for cmd in &cmds {
        let seg = seg_idx_by_url(&v, cmd.url());
        if seg <= 1 {
            continue;
        }
        assert!(
            cmd.cancel()
                .expect("segment cmd carries a cancel token")
                .is_cancelled(),
            "look-ahead fetch for segment {seg} must be retired"
        );
    }
}

/// The owed window survives a look-ahead retire: the reader is still
/// consuming those bytes and cancelling them would underrun the splice.
#[kithara::test]
fn retire_lookahead_spares_the_owed_window() {
    let ctx = test_ctx(8);
    let v = make_var(0, 0, &[100, 100, 100, 100, 100, 100], &ctx);
    v.rebuild(&ctx, 0);
    let session = active_session(&v, &ctx, 0);
    let cmds = session.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 6, "precondition: every segment dispatches");

    session.retire_lookahead();

    for cmd in &cmds {
        let seg = seg_idx_by_url(&v, cmd.url());
        if seg > 1 {
            continue;
        }
        assert!(
            !cmd.cancel()
                .expect("segment cmd carries a cancel token")
                .is_cancelled(),
            "owed fetch for segment {seg} must stay live"
        );
    }
}

/// A recoverably failed fetch re-enters the plan in plan order, not at the
/// front: dispatch caps read the queue head, and a far look-ahead entry
/// parked there would wall off every nearer segment behind it.
#[kithara::test]
fn requeue_planned_reenters_in_plan_order() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100, 100, 100], &ctx);
    v.rebuild(&ctx, 0);
    let revision = {
        let mut queue = v.flow.queue.lock();
        let (_, revision) = queue.pop_front().expect("planned fetch");
        for _ in 0..2 {
            queue.pop_front();
        }
        revision
    };

    assert!(v.requeue_planned(PlannedFetch::Segment(0), revision));
    assert!(v.requeue_planned(PlannedFetch::Segment(2), revision));

    let queue: Vec<_> = v.flow.queue.lock().iter().copied().collect();
    assert_eq!(
        queue,
        vec![
            PlannedFetch::Segment(0),
            PlannedFetch::Segment(2),
            PlannedFetch::Segment(3),
        ],
        "requeued fetches must land between their plan-order neighbours"
    );
}

/// Near-end time seek on a segment-aware (fMP4) variant whose segments are
/// still placeholder-sized: the anchor byte is minted against the placeholder
/// frame, so a late prefix size commit would re-key every raw offset under it.
fn drifting_seek_session() -> (PlanCtx, Arc<HlsVariant>, HlsSession, u64) {
    let ctx = test_ctx(3);
    let seek = Arc::new(SeekState::new());
    let segments: Vec<Segment> = (0..12)
        .map(|idx| make_placeholder_seg(idx, 100, &ctx.scope))
        .collect();
    let v = VariantParts {
        segments,
        init: None,
        seek_obs: Arc::clone(&seek) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::AacLc),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);
    let anchor = v
        .prepare_seek_time_anchor(Duration::from_secs(21))
        .expect("seek point")
        .expect("anchor")
        .byte_offset;
    assert_eq!(anchor, 1000, "segment 10 anchor on the placeholder frame");
    let session = HlsSession::active(
        CancelToken::never(),
        seek,
        ctx.signal.clone(),
        0,
        Arc::clone(&v),
        anchor,
    );
    (ctx, v, session, anchor)
}

/// Settle a segment through the production path (`into_loaded` →
/// `HlsVariant::apply_commit`), exactly like a fetch completing.
fn settle_seg(v: &Arc<HlsVariant>, ctx: &PlanCtx, idx: u32, len: u64) {
    let claim = v.segments()[idx as usize]
        .state()
        .try_claim(
            PlannedFetch::Segment(idx),
            v.flow.queue.revision(),
            Arc::downgrade(v),
            ctx.signal.clone(),
        )
        .expect("segment claim");
    claim.into_loaded(len);
}

#[kithara::test]
fn post_seek_prefix_settle_does_not_move_the_frame() {
    let (ctx, v, session, anchor) = drifting_seek_session();

    // A stale in-flight prefix fetch (planned before the seek — a rebuild
    // never cancels in-flight work) settles after the anchor was minted.
    settle_seg(&v, &ctx, 1, 600);
    session.advance(64);

    assert_eq!(
        v.segment_byte_offset(10),
        Some(anchor),
        "a settle behind the seek tail must not re-key the byte space under \
         the anchor the reader lives in"
    );
    let resolved = v.find_at_offset(anchor + 64).map(|(idx, ..)| idx);
    assert_eq!(
        resolved,
        Some(10),
        "the post-seek cursor keeps naming the seek target, not a prefix \
         segment slid under it"
    );
}

#[kithara::test]
fn eof_mints_at_the_anchored_frame_end_despite_a_late_prefix_settle() {
    let (ctx, v, session, anchor) = drifting_seek_session();

    // The planned tail (readahead 9, target 10, last 11) settles exact...
    for idx in 9..12 {
        settle_seg(&v, &ctx, idx, 100);
    }
    // ...while a stale prefix settle lands behind the seek tail.
    settle_seg(&v, &ctx, 1, 600);
    session.advance(200);

    let end = anchor + 200;
    let mut buf = [0_u8; 1];
    assert!(
        matches!(
            v.read_at(end, &mut buf).expect("end read"),
            ReadOutcome::Eof
        ),
        "the byte after the last tail segment is the stream end on the \
         anchored frame; the parked prefix size must not push it out"
    );
    assert!(
        matches!(
            v.wait_range(end..end + 1, Some(Duration::ZERO)),
            Ok(WaitOutcome::Eof)
        ),
        "wait at the anchored frame end must report EOF, not park on a \
         phantom gap opened by the parked prefix size"
    );
}

#[kithara::test]
fn the_next_space_re_mint_applies_the_deferred_prefix_settle() {
    let (ctx, v, _session, _anchor) = drifting_seek_session();

    settle_seg(&v, &ctx, 1, 600);
    assert_eq!(
        v.segment_byte_offset(2),
        Some(200),
        "the real size stays parked while the seek tail is active"
    );

    v.reset_for_seek();

    assert_eq!(
        v.segment_byte_offset(2),
        Some(700),
        "the space re-mint publishes the parked real size with the fresh frame"
    );
}

/// Settle the init through the production path, like `settle_seg`.
fn settle_init(v: &Arc<HlsVariant>, ctx: &PlanCtx, len: u64) {
    let claim = v
        .segments
        .init
        .as_ref()
        .expect("init slot")
        .state()
        .try_claim(
            PlannedFetch::Init,
            v.flow.queue.revision(),
            Arc::downgrade(v),
            ctx.signal.clone(),
        )
        .expect("init claim");
    claim.into_loaded(len);
}

#[kithara::test]
fn an_init_settle_lands_immediately_while_a_seek_tail_is_live() {
    // Init reads gate on the size being exact, so the freeze must never
    // park an init settle: a parked init starves the demuxer probe of an
    // ABR pending variant, which deadlocks before activation could drain
    // it. The byte-space shift it causes is the accepted cost.
    let ctx = test_ctx(3);
    let seek = Arc::new(SeekState::new());
    let segments: Vec<Segment> = (0..12).map(|idx| make_seg(idx, 100, &ctx.scope)).collect();
    let v = VariantParts {
        segments,
        init: Some(make_placeholder_init(256, &ctx.scope)),
        seek_obs: Arc::clone(&seek) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::AacLc),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);
    v.prepare_seek_time_anchor(Duration::from_secs(21))
        .expect("seek point")
        .expect("anchor");

    settle_init(&v, &ctx, 700);

    assert_eq!(
        v.segment_byte_offset(0),
        Some(700),
        "the init settle keys the frame immediately despite the live tail"
    );
    assert!(
        v.segments
            .init
            .as_ref()
            .expect("init slot")
            .size()
            .is_exact(),
        "the init size atom reads exact right after the settle"
    );
}

#[kithara::test]
fn an_exact_size_revision_parks_and_fails_the_skip_formula() {
    // Fully exact fMP4 track (canonical-complete layout) with a live seek
    // tail: a revision settle of an already-exact prefix size (DRM-style
    // plaintext length over a byterange seed) parks while the size atom
    // stays exact, so only the emptiness check in the skip formula can
    // force the re-mint that lands it.
    let ctx = test_ctx(3);
    let seek = Arc::new(SeekState::new());
    let segments: Vec<Segment> = (0..12).map(|idx| make_seg(idx, 100, &ctx.scope)).collect();
    let v = VariantParts {
        segments,
        init: None,
        seek_obs: Arc::clone(&seek) as Arc<dyn SeekObserve>,
        codec: Some(AudioCodec::AacLc),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);
    v.prepare_seek_time_anchor(Duration::from_secs(21))
        .expect("seek point")
        .expect("anchor");

    // Canonical table, nothing parked: the re-mint is skipped and the
    // stale tail stays live (and inert) behind it.
    v.reset_to_full_range();

    settle_seg(&v, &ctx, 1, 600);
    assert_eq!(
        v.segment_byte_offset(2),
        Some(200),
        "the revision parks behind the still-live tail"
    );
    assert!(
        !v.layout_seek_invariant(),
        "a parked size fails the skip formula even though the table still \
         reads canonical"
    );

    v.reset_for_seek();

    assert_eq!(
        v.segment_byte_offset(2),
        Some(700),
        "the forced re-mint lands the parked revision"
    );
}

#[kithara::test]
fn a_settle_after_the_space_re_mint_applies_immediately() {
    let (ctx, v, _session, _anchor) = drifting_seek_session();

    // The re-mint retires the seek tail together with the frozen space...
    v.reset_for_seek();
    // ...so a settle landing right after applies to the fresh frame instead
    // of parking behind a tail whose space is already gone.
    settle_seg(&v, &ctx, 1, 600);

    assert_eq!(
        v.segment_byte_offset(2),
        Some(700),
        "a settle after the re-mint keys the fresh frame immediately"
    );
}

fn write_seg_bytes(v: &Arc<HlsVariant>, ctx: &PlanCtx, idx: u32, len: u64) {
    let key = v.segments()[idx as usize].resource_id().clone();
    let AcquisitionResult::Pending(writer) = ctx
        .scope
        .store()
        .acquire_resource(&key, None)
        .expect("acquire segment")
    else {
        panic!("segment resource must be pending");
    };
    let bytes: Vec<u8> = (0..len).map(|b| b.to_le_bytes()[0]).collect();
    writer.write_at(0, &bytes).expect("write segment");
    writer.commit(Some(len)).expect("commit segment");
}

/// A chunked run over one slot opens its resource once.
#[kithara::test]
fn reading_a_segment_in_chunks_opens_its_resource_once() {
    let ctx = test_ctx(1);
    let v = make_var(0, 0, &[64], &ctx);
    write_seg_bytes(&v, &ctx, 0, 64);
    settle_seg(&v, &ctx, 0, 64);

    let mut buf = [0_u8; 8];
    for offset in (0..64_u64).step_by(8) {
        let outcome = v.read_at(offset, &mut buf).expect("chunked segment read");
        assert!(
            matches!(outcome, ReadOutcome::Bytes(n) if n.get() == 8),
            "chunk at {offset} must serve 8 bytes, got {outcome:?}"
        );
        assert_eq!(
            u64::from(buf[0]),
            offset,
            "chunk at {offset} must serve that offset's bytes"
        );
    }

    assert_eq!(
        v.segments.opens.load(Ordering::Relaxed),
        1,
        "eight chunks over one segment must open its resource once"
    );
}

/// A `NotFound` while the fetch is in flight must not be held.
#[kithara::test]
fn a_read_before_the_bytes_land_does_not_stick() {
    let ctx = test_ctx(1);
    let v = make_var(0, 0, &[64], &ctx);
    settle_seg(&v, &ctx, 0, 64);

    let mut buf = [0_u8; 8];
    let outcome = v.read_at(0, &mut buf).expect("read before the bytes land");
    assert!(
        matches!(outcome, ReadOutcome::Pending(_)),
        "a slot with no bytes yet is pending, got {outcome:?}"
    );

    write_seg_bytes(&v, &ctx, 0, 64);

    let outcome = v.read_at(0, &mut buf).expect("read after the bytes land");
    assert!(
        matches!(outcome, ReadOutcome::Bytes(n) if n.get() == 8),
        "the same slot must serve once its bytes land, got {outcome:?}"
    );
}

/// Eviction takes the bytes away under the reader.
#[kithara::test]
fn an_evicted_slot_is_opened_again() {
    let ctx = test_ctx(1);
    let v = make_var(0, 0, &[64], &ctx);
    write_seg_bytes(&v, &ctx, 0, 64);
    settle_seg(&v, &ctx, 0, 64);

    let mut buf = [0_u8; 8];
    let outcome = v.read_at(0, &mut buf).expect("first read");
    assert!(
        matches!(outcome, ReadOutcome::Bytes(_)),
        "the slot serves before eviction, got {outcome:?}"
    );
    let before = v.segments.opens.load(Ordering::Relaxed);

    let key = v.segments()[0].resource_id().clone();
    assert_eq!(v.on_evict(&key), Some(0), "seg 0 belongs to this variant");
    let _outcome = v.read_at(0, &mut buf).expect("read after eviction");

    assert!(
        v.segments.opens.load(Ordering::Relaxed) > before,
        "a read after eviction must open the resource again"
    );
}

fn disk_ctx(root: &std::path::Path) -> PlanCtx {
    let cancel = CancelToken::never();
    let backend = Arc::new(
        AssetStore::builder()
            .backend(StorageBackend::Disk { root: root.into() })
            .cancel(cancel)
            .build(),
    );
    PlanCtx {
        bus: EventBus::new(8),
        scope: backend
            .scope::<crate::Hls>(&AssetSource::Remote {
                url: Url::parse("https://example.com/master.m3u8").expect("master url"),
                discriminator: Some("disk".to_owned()),
            })
            .expect("test asset scope"),
        seek_epoch: 0,
        headers: None,
        signal: SizeSignal::new(Arc::new(ThreadGate::default()), Arc::new(OnceLock::new())),
        config: PlanConfig::builder().prefetch_budget(1).build(),
    }
}

/// A ready gate over a pending read is a loop with no exit.
#[kithara::test]
fn a_ready_gate_never_outruns_the_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");

    let path = {
        let ctx = disk_ctx(dir.path());
        let v = make_var(0, 0, &[64], &ctx);
        write_seg_bytes(&v, &ctx, 0, 64);
        let key = v.segments()[0].resource_id().clone();
        ctx.scope.store().checkpoint().expect("persist the index");
        dir.path()
            .join(ctx.scope.asset_root())
            .join(key.rel_path().expect("relative key"))
    };
    std::fs::remove_file(&path).expect("prune the cached bytes");

    let ctx = disk_ctx(dir.path());
    let v = make_var(0, 0, &[64], &ctx);
    settle_seg(&v, &ctx, 0, 64);

    let mut buf = [0_u8; 8];
    let read = v.read_at(0, &mut buf).expect("read");
    let gate = v.wait_range(0..8, Some(Duration::ZERO));

    assert!(
        !(matches!(gate, Ok(WaitOutcome::Ready)) && matches!(read, ReadOutcome::Pending(_))),
        "gate and read disagree, so the reader spins with no exit: \
         gate={gate:?} read={read:?}"
    );
}
