use std::sync::{
    Arc, Barrier, OnceLock,
    atomic::{AtomicUsize, Ordering},
};

use kithara_assets::{AcquisitionResult, AssetScope, AssetStoreBuilder, ProcessChunkFn, WriteSide};
use kithara_drm::DecryptContext;
use kithara_platform::{CancelToken, sync::CondvarGate, time::Duration};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    AudioCodec, ContainerFormat, ReadOutcome, SeekControl, SeekObserve, SeekState, SourceError,
    SourcePhase, StreamError,
};
use kithara_test_utils::kithara;
use url::Url;

use super::{
    HlsVariant, PlanCtx, SegmentActivateParams, SizeDemand, VariantParts, segment_placeholder_size,
};
use crate::{
    config::SizeProbeMethod,
    ids::SegmentIndex,
    playlist::{PlaylistState, SegmentState, VariantState},
    segment::{
        InitSegment, MediaSegment, PlannedFetch, Segment, SegmentContent, SegmentSize,
        SegmentSlotState,
    },
    signal::SizeSignal,
};

fn test_ctx(prefetch_budget: usize) -> PlanCtx {
    let cancel = CancelToken::never();
    let passthrough: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        });
    let backend = Arc::new(
        AssetStoreBuilder::new()
            .ephemeral(true)
            .cancel(cancel.clone())
            .process_fn(passthrough)
            .build(),
    );
    PlanCtx {
        prefetch_budget,
        master_cancel: cancel,
        scope: backend.scope("test"),
        seek_epoch: 0,
        look_ahead_bytes: None,
        headers: None,
        size_probe_method: SizeProbeMethod::Head,
        signal: SizeSignal::new(
            Arc::new(CondvarGate::<u64>::default()),
            Arc::new(OnceLock::new()),
        ),
    }
}

fn make_init(size: u64, scope: &AssetScope<DecryptContext>) -> Option<Segment> {
    if size == 0 {
        return None;
    }
    let url: Url = "https://example.com/init.mp4".parse().expect("valid url");
    let resource_id = scope.key_from_url(&url);
    Some(Segment::Init(InitSegment {
        url,
        resource_id,
        state: SegmentSlotState::missing(),
        size: SegmentSize::seed(size),
        content: SegmentContent::Plain,
    }))
}

fn make_seg(idx: u32, size: u64, scope: &AssetScope<DecryptContext>) -> Segment {
    let url: Url = format!("https://example.com/seg{idx}.m4s")
        .parse()
        .expect("valid url");
    let resource_id = scope.key_from_url(&url);
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

fn make_placeholder_seg(idx: u32, size: u64, scope: &AssetScope<DecryptContext>) -> Segment {
    let url: Url = format!("https://example.com/seg{idx}.m4s")
        .parse()
        .expect("valid url");
    let resource_id = scope.key_from_url(&url);
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
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
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
                duration: Duration::from_secs(2),
                byte_range_len: None,
                url,
                index: SegmentIndex::try_new(idx, count).expect("in-bounds segment"),
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

fn queue_has_init(v: &HlsVariant) -> bool {
    v.flow
        .queue
        .lock()
        .iter()
        .any(|p| matches!(p, PlannedFetch::Init))
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
fn range_ready_clamps_tail_seek_alias_to_eof() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: None,
        segments: (0..3).map(|idx| make_seg(idx, 10, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
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
        let bytes = vec![0u8; segment.len() as usize];
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
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
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
    assert_eq!(v.get_position(), 0);
}

/// Phase K.0.2 invariant. `HlsCoord::commit_variant_switch`
/// (same-codec branch) calls `activate_at_segment_with_shift` BEFORE
/// `abr.apply_decision`, so a reader observing `current_variant := new`
/// must see `v_new` with all activation atomics published. This test
/// asserts the variant-side half of that contract: every relevant
/// atomic carries the post-activation value the moment the call
/// returns.
#[kithara::test]
fn activate_at_segment_with_shift_publishes_all_state_before_returning() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400, 400, 400], &ctx);
    assert_eq!(v.served_from(), 0);
    assert_eq!(
        v.segment_byte_offset(2),
        Some(1_000),
        "natural offset of seg 2 = 200 init + 400 + 400"
    );

    let from_seg: u32 = 2;
    let seg_boundary: u64 = 1_500;
    let reader_pos: u64 = 1_600;
    v.activate_at_segment_with_shift(
        &ctx,
        SegmentActivateParams {
            from_seg,
            reader_pos,
            seg_boundary,
        },
    );

    assert_eq!(
        v.served_from(),
        from_seg,
        "served_from must equal the activation segment after return"
    );
    assert_eq!(
        v.segment_byte_offset(from_seg),
        Some(seg_boundary),
        "from_seg must be pinned at seg_boundary (encodes byte_shift = \
         seg_boundary - natural_offset = 500)"
    );
    let seg3_virtual = v.segment_byte_offset(3).expect("seg 3 addressable");
    assert!(
        v.find_at_offset(seg3_virtual).is_some(),
        "served_until must span all segments after a fresh activate"
    );
    assert_eq!(
        v.get_position(),
        reader_pos,
        "position must follow the requested reader_pos"
    );
    assert!(
        v.seek.exact_byte_seek.lock().is_none(),
        "ABR byte-continuity activation mirrors the old reader cursor; it must not register a seek-style exact-byte demand in the incoming variant"
    );
}

#[kithara::test]
fn descriptor_at_byte_uses_virtual_shifted_range() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400, 400, 400], &ctx);

    v.activate_at_segment_with_shift(
        &ctx,
        SegmentActivateParams {
            from_seg: 2,
            reader_pos: 1_500,
            seg_boundary: 1_500,
        },
    );

    let descriptor = v
        .descriptor_at_byte(1_550)
        .expect("shifted segment 2 resolves");

    assert_eq!(descriptor.segment_index, 2);
    assert_eq!(
        descriptor.byte_range,
        1_500..1_900,
        "ByteMap descriptors must use the same virtual coordinates as \
         find_at_offset/read_at/phase_at"
    );
}

#[kithara::test]
fn reset_to_full_range_uses_live_init_size_after_shifted_activation() {
    let ctx = test_ctx(3);
    let v = make_var(0, 600, &[400, 400], &ctx);

    v.activate_at_segment_with_shift(
        &ctx,
        SegmentActivateParams {
            from_seg: 1,
            reader_pos: 1_000,
            seg_boundary: 1_000,
        },
    );
    v.layout.apply_commit(v.segments(), || {
        v.apply_loaded_size(PlannedFetch::Init, 588);
        v.init_size()
    });

    v.reset_to_full_range();

    assert_eq!(
        v.segment_byte_offset(0),
        Some(588),
        "full-range reset must leave shifted/frozen init geometry and use \
         the committed live init length"
    );
}

/// Coordinate-frame coherence under a concurrent variant switch.
///
/// `find_virtual` reads `byte_shift` and the served bounds in separate
/// steps; a reader on the decode thread races
/// `activate_at_segment_with_shift` / `reset_to_full_range` on the
/// scheduler thread. Byte 2000 is served by BOTH the full frame
/// (shift 0, served [0,8) -> seg 1) and the activated frame
/// (shift -5000, served [4,8) -> seg 6), so a coherent read always
/// resolves it. The only way `find_virtual` returns `None` is a torn
/// read that pairs `byte_shift == 0` (full frame) with
/// `served_from == 4` (activated frame) -> seg 1 falls below the served
/// range. That `(0, 4)` pairing is never a real activation frame, so
/// every `None` is a split-lock tear. Offsets are value-identical across
/// both frames (seed 1000, fixed sizes), isolating the tear to
/// `byte_shift` + `served_from`.
#[kithara::test]
fn concurrent_switch_keeps_coordinate_reads_coherent() {
    let ctx = test_ctx(8);
    let v = make_var(0, 1000, &[1000; 8], &ctx);

    v.reset_to_full_range();
    assert!(
        v.find_at_offset(2000).is_some(),
        "full frame must serve byte 2000"
    );
    v.activate_at_segment_with_shift(
        &ctx,
        SegmentActivateParams {
            from_seg: 4,
            seg_boundary: 0,
            reader_pos: 0,
        },
    );
    assert!(
        v.find_at_offset(2000).is_some(),
        "activated frame must serve byte 2000"
    );

    const ITERS: usize = 50_000;
    let barrier = Barrier::new(2);
    let torn = AtomicUsize::new(0);

    std::thread::scope(|s| {
        s.spawn(|| {
            barrier.wait();
            for i in 0..ITERS {
                if i % 2 == 0 {
                    v.reset_to_full_range();
                } else {
                    v.activate_at_segment_with_shift(
                        &ctx,
                        SegmentActivateParams {
                            from_seg: 4,
                            seg_boundary: 0,
                            reader_pos: 0,
                        },
                    );
                }
            }
        });
        s.spawn(|| {
            barrier.wait();
            for _ in 0..ITERS {
                if v.find_at_offset(2000).is_none() {
                    torn.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    });

    assert_eq!(
        torn.load(Ordering::Relaxed),
        0,
        "byte 2000 is served by both activation frames; a None means \
         find_virtual mixed byte_shift and served_from from different \
         frames (split-lock torn read)"
    );
}

#[kithara::test]
fn advance_increments_position() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400], &ctx);
    v.advance(64);
    assert_eq!(v.get_position(), 64);
    v.advance(36);
    assert_eq!(v.get_position(), 100);
}

#[kithara::test]
fn set_position_overrides_cursor() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400], &ctx);
    v.advance(50);
    v.set_position(1234);
    assert_eq!(v.get_position(), 1234);
}

#[kithara::test]
fn find_at_offset_inside_init_prefix_is_none() {
    let ctx = test_ctx(3);
    let v = make_var(0, 200, &[400, 400], &ctx);
    assert!(v.find_at_offset(0).is_none());
    assert!(v.find_at_offset(199).is_none());
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
/// and a `set_served_until` range narrowing both republish the cached total.
#[kithara::test]
fn total_bytes_lock_free_tracks_commit_and_served_until() {
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

    // Serve only [0, 2): the cached total ends at seg 1's tail.
    v.set_served_until(2);
    assert_eq!(
        v.total_bytes(),
        200 + 384 + 400,
        "lock-free total reflects the narrowed served range"
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
    let cmds = v.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 2, "only the two media segments dispatch");
    let seg0_url = v.segments()[0].url().clone();
    assert_eq!(cmds[0].url, seg0_url, "first cmd is seg 0, not an init");
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
    let cmds = v.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 3, "init + two media segments dispatch");
    assert_eq!(cmds[0].url, init_url, "init dispatched first");
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

    let init = HlsVariant::build_init_entry(&playlist, 0, None, &ctx.scope);

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
        resource_id: ctx.scope.key_from_url(&init_url),
        url: init_url,
        state: SegmentSlotState::missing(),
        size: SegmentSize::seed(0),
        content: SegmentContent::Plain,
    }));
    let v = VariantParts {
        init,
        segments: vec![make_seg(0, 1024, &ctx.scope)],
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
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
fn descriptor_after_byte_finds_next_segment() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100, 100], &ctx);
    let d = v.descriptor_after_byte(50).expect("descriptor");
    assert_eq!(d.segment_index, 1);
    let d = v.descriptor_after_byte(100).expect("descriptor");
    assert_eq!(d.segment_index, 1);
}

#[kithara::test]
fn rebuild_refills_queue_without_touching_cancel_token() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100; 6], &ctx);
    push_planned(&v, 0);
    let token = v.cancel_handle();
    assert!(!token.is_cancelled());
    v.rebuild(&ctx, 2);
    assert!(
        !token.is_cancelled(),
        "rebuild must NOT cancel the variant token — that's reserved for variant deactivation"
    );
    assert_eq!(queue_seg_indices(&v), vec![2, 3, 4, 5]);
}

#[kithara::test]
fn segment_aware_rebuild_at_time_prefetches_seek_preroll_segment() {
    let ctx = test_ctx(3);
    let v = VariantParts {
        init: None,
        segments: (0..5).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
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
fn segment_aware_seek_time_anchor_fetches_preroll_segment() {
    let ctx = test_ctx(3);
    let playlist_state =
        make_playlist_state(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), 5);
    let v = VariantParts {
        init: None,
        segments: (0..5).map(|idx| make_seg(idx, 100, &ctx.scope)).collect(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        playlist_state,
        codec: Some(AudioCodec::AacLc),
        container: Some(ContainerFormat::Fmp4),
    }
    .into_variant(0, &ctx);

    let anchor = v
        .seek_time_anchor(Duration::from_secs(4))
        .expect("seek anchor")
        .expect("segment-aware anchor");

    assert_eq!(anchor.segment_index, Some(2));
    assert_eq!(
        anchor.byte_offset, 200,
        "decoder anchor remains the target segment boundary"
    );
    assert_eq!(
        v.get_position(),
        200,
        "reader position remains the decoder anchor"
    );
    assert_eq!(
        queue_seg_indices(&v),
        vec![1, 2, 3, 4],
        "fetch queue includes the segment a codec warmup backoff may read"
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
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
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
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
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
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);

    v.rebuild_with_decoder_probe(&ctx, 2);

    assert!(queue_has_init(&v));
    assert_eq!(queue_seg_indices(&v), vec![0, 2, 3, 4]);
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
    let cmds = v.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 4);
    assert_eq!(cmds[0].url, init_url, "init dispatched first");
    assert_eq!(cmds[1].url, seg0_url);
    assert_eq!(cmds[2].url, seg1_url);
    assert_eq!(cmds[3].url, seg2_url);
    for cmd in &cmds {
        assert!(cmd.cancel.is_some(), "every cmd carries a cancel token");
    }
}

#[kithara::test]
fn dispatch_respects_budget() {
    let ctx = test_ctx(5);
    let v = make_var(0, 0, &[100; 10], &ctx);
    v.rebuild(&ctx, 0);
    let cmds = v.dispatch(&ctx, 3);
    assert_eq!(cmds.len(), 3);
    assert_eq!(queue_seg_indices(&v), vec![3, 4, 5, 6, 7, 8, 9]);
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
    let cmds = v.dispatch(&ctx, 10);
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
        .try_claim(PlannedFetch::Segment(1), Arc::downgrade(&v))
        .expect("seg 1 must be claimable");

    v.flow.queue.lock().clear();
    for seg in 0..3_u32 {
        push_planned(&v, seg);
    }

    // First dispatch: seg 0 + seg 2 emit; seg 1 is Downloading -> claim fails.
    let cmds = v.dispatch(&ctx, 10);
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
    let cmds2 = v.dispatch(&ctx, 10);
    assert_eq!(
        cmds2.len(),
        1,
        "seg 1 (orphaned -> Missing) must be re-dispatched, not lost from the queue"
    );
}

#[kithara::test]
fn phase_at_reports_waiting_demand_for_claimed_segment() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100, 100], &ctx);
    let claim = v.segments()[0]
        .state()
        .try_claim(PlannedFetch::Segment(0), Arc::downgrade(&v))
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

#[kithara::test]
fn exact_seek_completion_keeps_stale_anchor_alias_until_reader_moves() {
    let ctx = test_ctx(3);
    let segments: Vec<Segment> = (0..3)
        .map(|idx| make_placeholder_seg(idx, 256, &ctx.scope))
        .collect();
    let v = VariantParts {
        init: None,
        segments,
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);
    let stale_anchor = 512;
    v.set_position(stale_anchor);
    v.set_prefetch_anchor(stale_anchor);
    v.set_seek_alias(stale_anchor, 2);
    v.rebuild(&ctx, 2);
    v.set_exact_seek_demand(stale_anchor, 2);
    v.set_exact_seek_demand(stale_anchor + 64, 2);

    v.apply_resolved_size(SizeDemand::Segment(0), 100);
    v.apply_resolved_size(SizeDemand::Segment(1), 100);
    v.apply_resolved_size(SizeDemand::Segment(2), 100);

    assert_eq!(v.get_position(), 200);
    v.advance(0);
    assert_eq!(v.find_at_offset(stale_anchor), Some((2, stale_anchor, 100)));
    assert_eq!(
        v.phase_at(stale_anchor..stale_anchor + 16),
        SourcePhase::WaitingDemand
    );

    v.advance(1);
    assert_eq!(v.get_position(), 201);
    assert_eq!(v.find_at_offset(stale_anchor), None);
}

#[kithara::test]
fn raw_byte_seek_registers_lazy_exact_demand_only_after_cursor_moves() {
    let ctx = test_ctx(3);
    let segments: Vec<Segment> = (0..10)
        .map(|idx| make_placeholder_seg(idx, 64, &ctx.scope))
        .collect();
    let v = VariantParts {
        init: None,
        segments,
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);

    v.set_position(0);
    assert!(
        v.dispatch(&ctx, 3).is_empty(),
        "a no-op seek to the current cursor must not issue startup size probes"
    );

    v.set_position(512);
    assert_eq!(v.phase_at(512..513), SourcePhase::WaitingDemand);
    let cmds = v.dispatch(&ctx, 3);
    assert_eq!(
        cmds.len(),
        3,
        "raw byte seek size probes must respect the dispatch budget"
    );
    assert_eq!(cmds[0].url, v.segments()[0].url().clone());
    assert_eq!(cmds[1].url, v.segments()[1].url().clone());
    assert_eq!(cmds[2].url, v.segments()[2].url().clone());
    let cmds = v.dispatch(&ctx, 3);
    assert_eq!(cmds.len(), 3);
    assert_eq!(cmds[0].url, v.segments()[3].url().clone());
    assert_eq!(cmds[1].url, v.segments()[4].url().clone());
    assert_eq!(cmds[2].url, v.segments()[5].url().clone());
}

#[kithara::test]
fn byte_continuity_boundary_preparation_sizes_only_prefix() {
    let ctx = test_ctx(8);
    let segments: Vec<Segment> = (0..4)
        .map(|idx| make_placeholder_seg(idx, 64, &ctx.scope))
        .collect();
    let v = VariantParts {
        init: None,
        segments,
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
    }
    .into_variant(0, &ctx);

    assert!(
        !v.prepare_exact_prefix_for_boundary(2),
        "WAV byte-continuity switch at segment 2 needs exact sizes for segments 0 and 1"
    );

    let cmds = v.dispatch_size_only(&ctx, 8);
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].url, v.segments()[0].url().clone());
    assert_eq!(cmds[1].url, v.segments()[1].url().clone());

    v.apply_resolved_size(SizeDemand::Segment(0), 100);
    v.apply_resolved_size(SizeDemand::Segment(1), 120);

    assert!(
        v.prepare_exact_prefix_for_boundary(2),
        "once the prefix is exact, coord may calculate byte_shift and commit"
    );
    assert!(
        !v.segments()[2].size().is_exact(),
        "the segment after the switch boundary must stay lazy"
    );
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
    let foreign_key = ctx.scope.key_from_url(&foreign);
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
    let resource_id = ctx.scope.key_from_url(&url);
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
        playlist_state: Arc::new(PlaylistState::new(Vec::new())),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        codec: None,
        container: None,
        segments: vec![seg],
    }
    .into_variant(0, &ctx);
    push_planned(&v, 0);
    let cmds = v.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 1);
    assert!(cmds[0].cancel.is_some());
    push_planned(&v, 0);
    assert!(
        v.dispatch(&ctx, 10).is_empty(),
        "claimed (in-flight) segment must not be re-dispatched"
    );
}

#[kithara::test]
fn dropped_fetch_cmd_reverts_segment_to_missing() {
    let ctx = test_ctx(5);
    let v = make_var(0, 0, &[100, 100], &ctx);
    push_planned(&v, 0);
    let cmds = v.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 1, "first dispatch claims and emits seg 0");
    // Drop the command without running its `on_complete`: the owned
    // download handle is dropped without a settle, so the Drop safety
    // net must revert the slot to Missing rather than strand it.
    drop(cmds);
    push_planned(&v, 0);
    assert_eq!(
        v.dispatch(&ctx, 10).len(),
        1,
        "dropped claim must revert the slot to Missing so it re-dispatches"
    );
}

#[kithara::test]
fn positions_of_two_variants_are_independent_after_flip() {
    let ctx = test_ctx(3);
    let v_old = make_var(0, 0, &[400; 20], &ctx);
    let v_new = make_var(1, 0, &[800; 20], &ctx);
    let v_new_seg10_offset = v_new.segment_byte_offset(10).expect("seg 10");
    v_old.set_position(5000);
    v_new.set_position(v_new_seg10_offset);
    assert_eq!(v_old.get_position(), 5000);
    assert_eq!(v_new.get_position(), v_new_seg10_offset);

    v_new.advance(123);
    assert_eq!(
        v_old.get_position(),
        5000,
        "advance(V_new) must not touch V_old"
    );
    assert_eq!(v_new.get_position(), v_new_seg10_offset + 123);
}

#[kithara::test]
fn position_advances_are_strictly_monotonic() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100], &ctx);
    let mut expected = 0_u64;
    let mut observed = Vec::new();
    for n in [10_u64, 25, 7, 64, 1, 100] {
        v.advance(n);
        expected += n;
        observed.push(v.get_position());
        assert_eq!(v.get_position(), expected);
    }
    let mut sorted = observed.clone();
    sorted.sort_unstable();
    assert_eq!(observed, sorted);
}

#[kithara::test]
fn dispatch_cmd_cancel_shares_cancellation_with_variant_cancel() {
    let ctx = test_ctx(5);
    let v = make_var(0, 0, &[100, 100], &ctx);
    let variant_cancel = v.cancel_handle();
    for seg in 0..2_u32 {
        push_planned(&v, seg);
    }
    let cmds = v.dispatch(&ctx, 10);
    for cmd in &cmds {
        let token = cmd.cancel.as_ref().expect("cmd carries cancel");
        assert!(!token.is_cancelled());
    }
    variant_cancel.cancel();
    for cmd in &cmds {
        let token = cmd.cancel.as_ref().expect("cmd carries cancel");
        assert!(
            token.is_cancelled(),
            "cmd cancel must follow variant.cancel"
        );
    }
}

#[kithara::test]
fn variant_flip_cancels_v_old_and_keeps_v_new_token_live() {
    let ctx = test_ctx(3);
    let v_old = make_var(0, 0, &[100; 20], &ctx);
    let v_new = make_var(1, 0, &[200; 20], &ctx);
    let v_old_token = v_old.cancel_handle();
    let v_new_token = v_new.cancel_handle();

    let from_seg = 7_u32;
    let v_new_seg7_offset = v_new.segment_byte_offset(from_seg).expect("seg 7");
    v_new.set_position(v_new_seg7_offset);
    v_old.cancel();
    v_new.rebuild(&ctx, from_seg);

    assert!(
        v_old_token.is_cancelled(),
        "v_old.cancel() cancels v_old's token"
    );
    assert!(
        !v_new_token.is_cancelled(),
        "rebuild on v_new must NOT touch v_new's cancel token"
    );
    assert_eq!(v_new.get_position(), v_new_seg7_offset);
}

#[kithara::test]
fn dispatch_skips_loaded_segments_in_queue_without_burning_budget() {
    let ctx = test_ctx(3);
    let v = make_var(0, 0, &[100; 20], &ctx);
    v.segments()[10].state().mark_loaded();

    v.rebuild(&ctx, 10);
    let cmds = v.dispatch(&ctx, 3);
    assert_eq!(cmds.len(), 3);
    let seg10_url = v.segments()[10].url().clone();
    assert!(
        cmds.iter().all(|c| c.url != seg10_url),
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
    let elapsed = started.elapsed();

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
    assert!(
        matches!(interrupted, Ok(WaitOutcome::Interrupted)),
        "flushing seek state must Interrupt the probe, got {interrupted:?}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(2),
        "flush short-circuit must not sleep"
    );
}
