use std::sync::{Arc, atomic::AtomicU8};

use kithara_assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{HlsVariant, InitEntry, PlanCtx, PlannedFetch, SegmentEntry, SegmentState};

fn test_ctx(prefetch_budget: usize) -> PlanCtx {
    let cancel = CancellationToken::new();
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
        master_cancel: cancel,
        asset_store: backend,
        prefetch_budget,
    }
}

fn make_init(size: u64) -> InitEntry {
    let url: Url = "https://example.com/init.mp4".parse().expect("valid url");
    let resource_id = ResourceKey::from_url(&url);
    InitEntry {
        url,
        resource_id,
        size,
        state: AtomicU8::new(SegmentState::Missing as u8),
    }
}

fn make_seg(idx: u32, byte_offset: u64, size: u64) -> SegmentEntry {
    let url: Url = format!("https://example.com/seg{idx}.m4s")
        .parse()
        .expect("valid url");
    let resource_id = ResourceKey::from_url(&url);
    SegmentEntry {
        url,
        resource_id,
        byte_offset,
        size,
        state: AtomicU8::new(SegmentState::Missing as u8),
        decrypt_ctx: None,
        decode_time: Duration::from_millis(u64::from(idx) * 2000),
        duration: Duration::from_secs(2),
    }
}

fn push_planned(v: &HlsVariant, seg: u32) {
    v.queue.lock_sync().push_back(PlannedFetch {
        variant: v.variant,
        segment: seg,
    });
}

fn queue_seg_indices(v: &HlsVariant) -> Vec<u32> {
    v.queue.lock_sync().iter().map(|p| p.segment).collect()
}

#[kithara::test]
fn position_starts_at_zero() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(0, make_init(200), vec![make_seg(0, 200, 400)], &ctx);
    assert_eq!(v.get_position(), 0);
}

#[kithara::test]
fn advance_increments_position() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(0, make_init(200), vec![make_seg(0, 200, 400)], &ctx);
    v.advance(64);
    assert_eq!(v.get_position(), 64);
    v.advance(36);
    assert_eq!(v.get_position(), 100);
}

#[kithara::test]
fn set_position_overrides_cursor() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(0, make_init(200), vec![make_seg(0, 200, 400)], &ctx);
    v.advance(50);
    v.set_position(1234);
    assert_eq!(v.get_position(), 1234);
}

#[kithara::test]
fn find_at_offset_below_init_returns_none() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(
        0,
        make_init(200),
        vec![make_seg(0, 200, 400), make_seg(1, 600, 400)],
        &ctx,
    );
    assert!(v.find_at_offset(0).is_none());
    assert!(v.find_at_offset(199).is_none());
}

#[kithara::test]
fn find_at_offset_at_init_size_returns_segment_zero() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(
        0,
        make_init(200),
        vec![make_seg(0, 200, 400), make_seg(1, 600, 400)],
        &ctx,
    );
    let (idx, byte_offset, _) = v.find_at_offset(200).expect("hit");
    assert_eq!(idx, 0);
    assert_eq!(byte_offset, 200);
}

#[kithara::test]
fn find_at_offset_mid_segment_binary_search() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(
        0,
        make_init(0),
        vec![
            make_seg(0, 200, 400),
            make_seg(1, 600, 400),
            make_seg(2, 1000, 400),
            make_seg(3, 1400, 400),
        ],
        &ctx,
    );
    let (idx, _, _) = v.find_at_offset(750).expect("mid-segment");
    assert_eq!(idx, 1);
    let (idx, _, _) = v.find_at_offset(1399).expect("last byte of seg 2");
    assert_eq!(idx, 2);
    let (idx, _, _) = v.find_at_offset(1400).expect("first byte of seg 3");
    assert_eq!(idx, 3);
}

#[kithara::test]
fn total_bytes_includes_segments() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(
        0,
        make_init(200),
        vec![
            make_seg(0, 200, 400),
            make_seg(1, 600, 400),
            make_seg(2, 1000, 400),
            make_seg(3, 1400, 400),
        ],
        &ctx,
    );
    assert_eq!(v.total_bytes(), 1800);
}

#[kithara::test]
fn init_byte_range_present_when_size_positive() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(0, make_init(200), vec![], &ctx);
    assert_eq!(v.init_byte_range(), Some(0..200));
}

#[kithara::test]
fn init_byte_range_absent_when_size_zero() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(0, make_init(0), vec![], &ctx);
    assert!(v.init_byte_range().is_none());
}

#[kithara::test]
fn descriptor_at_time_clamps_to_last() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(
        0,
        make_init(0),
        vec![
            make_seg(0, 0, 100),
            make_seg(1, 100, 100),
            make_seg(2, 200, 100),
        ],
        &ctx,
    );
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
    let v = HlsVariant::new(
        0,
        make_init(0),
        vec![
            make_seg(0, 0, 100),
            make_seg(1, 100, 100),
            make_seg(2, 200, 100),
        ],
        &ctx,
    );
    let d = v.descriptor_after_byte(50).expect("descriptor");
    assert_eq!(d.segment_index, 1);
    let d = v.descriptor_after_byte(100).expect("descriptor");
    assert_eq!(d.segment_index, 1);
}

#[kithara::test]
fn rebuild_cancels_old_token_and_refills_queue() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(
        0,
        make_init(0),
        vec![
            make_seg(0, 0, 100),
            make_seg(1, 100, 100),
            make_seg(2, 200, 100),
            make_seg(3, 300, 100),
            make_seg(4, 400, 100),
            make_seg(5, 500, 100),
        ],
        &ctx,
    );
    push_planned(&v, 0);
    let old_token = v.cancel_handle();
    assert!(!old_token.is_cancelled());
    v.rebuild(&ctx, 2);
    assert!(old_token.is_cancelled(), "old token must be cancelled");
    assert!(
        !v.cancel_handle().is_cancelled(),
        "fresh token must be live"
    );
    assert_eq!(queue_seg_indices(&v), vec![2, 3, 4, 5]);
}

#[kithara::test]
fn dispatch_emits_init_first_then_segments_under_budget() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(
        0,
        make_init(200),
        vec![
            make_seg(0, 200, 400),
            make_seg(1, 600, 400),
            make_seg(2, 1000, 400),
        ],
        &ctx,
    );
    let init_url = v.init.url.clone();
    let seg0_url = v.segments[0].url.clone();
    let seg1_url = v.segments[1].url.clone();
    let seg2_url = v.segments[2].url.clone();
    v.fill_queue(&ctx, 0);
    let cmds = v.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 4);
    assert_eq!(cmds[0].url, init_url, "init dispatched first");
    assert_eq!(cmds[1].url, seg0_url);
    assert_eq!(cmds[2].url, seg1_url);
    assert_eq!(cmds[3].url, seg2_url);
    for cmd in &cmds {
        assert!(cmd.cancel.is_some(), "every cmd carries a cancel token");
    }
    assert_eq!(v.init.state(), SegmentState::Downloading);
}

#[kithara::test]
fn dispatch_respects_budget() {
    let ctx = test_ctx(5);
    let init = make_init(0);
    init.set_state(SegmentState::Loaded);
    let v = HlsVariant::new(
        0,
        init,
        (0..10)
            .map(|i| make_seg(i, u64::from(i) * 100, 100))
            .collect(),
        &ctx,
    );
    v.fill_queue(&ctx, 0);
    let cmds = v.dispatch(&ctx, 3);
    assert_eq!(cmds.len(), 3);
    assert_eq!(queue_seg_indices(&v), vec![3, 4, 5]);
}

#[kithara::test]
fn dispatch_skips_non_missing_segments() {
    let ctx = test_ctx(5);
    let init = make_init(0);
    init.set_state(SegmentState::Loaded);
    let v = HlsVariant::new(
        0,
        init,
        vec![
            make_seg(0, 0, 100),
            make_seg(1, 100, 100),
            make_seg(2, 200, 100),
        ],
        &ctx,
    );
    v.segments[1].set_state(SegmentState::Loaded);
    v.queue.lock_sync().clear();
    for seg in 0..3_u32 {
        push_planned(&v, seg);
    }
    let cmds = v.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 2);
    assert_eq!(v.segments[1].state(), SegmentState::Loaded);
}

#[kithara::test]
fn on_evict_returns_minus_one_for_init() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(
        0,
        make_init(200),
        vec![
            make_seg(0, 200, 100),
            make_seg(1, 300, 100),
            make_seg(2, 400, 100),
        ],
        &ctx,
    );
    v.init.set_state(SegmentState::Loaded);
    v.segments[1].set_state(SegmentState::Loaded);
    let key = v.init.resource_id.clone();
    let res = v.on_evict(&key);
    assert_eq!(res, Some(-1));
    assert_eq!(v.init.state(), SegmentState::Missing);
    assert_eq!(
        v.segments[1].state(),
        SegmentState::Loaded,
        "init eviction must not touch segment states"
    );
}

#[kithara::test]
fn on_evict_returns_seg_idx_for_segment() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(
        0,
        make_init(0),
        vec![make_seg(0, 0, 100), make_seg(1, 100, 100)],
        &ctx,
    );
    v.segments[1].set_state(SegmentState::Loaded);
    let key = v.segments[1].resource_id.clone();
    let res = v.on_evict(&key);
    assert_eq!(res, Some(1));
    assert_eq!(v.segments[1].state(), SegmentState::Missing);
}

#[kithara::test]
fn on_evict_returns_none_for_foreign_asset() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(0, make_init(0), vec![make_seg(0, 0, 100)], &ctx);
    let foreign: Url = "https://other.example.com/x.m4s".parse().expect("url");
    let foreign_key = ResourceKey::from_url(&foreign);
    let res = v.on_evict(&foreign_key);
    assert_eq!(res, None);
}

#[kithara::test]
fn on_reader_advance_extends_prefetch_tail() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(
        0,
        make_init(0),
        (0..10)
            .map(|i| make_seg(i, u64::from(i) * 100, 100))
            .collect(),
        &ctx,
    );
    v.on_reader_advance(&ctx, 2);
    assert_eq!(queue_seg_indices(&v), vec![2, 3, 4, 5]);
}

#[kithara::test]
fn skeleton_types_instantiate() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(0, make_init(200), Vec::new(), &ctx);
    assert_eq!(v.num_segments(), 0);
}

#[kithara::test]
fn dispatch_drm_segment_routes_through_with_ctx() {
    let ctx = test_ctx(3);
    let init = make_init(0);
    init.set_state(SegmentState::Loaded);
    let mut seg = make_seg(0, 0, 100);
    let key = *b"0123456789abcdef";
    seg.decrypt_ctx = Some(DecryptContext::new(key, [0u8; 16]));
    let v = HlsVariant::new(0, init, vec![seg], &ctx);
    push_planned(&v, 0);
    let cmds = v.dispatch(&ctx, 10);
    assert_eq!(cmds.len(), 1);
    assert!(cmds[0].cancel.is_some());
    assert_eq!(v.segments[0].state(), SegmentState::Downloading);
}

#[kithara::test]
fn positions_of_two_variants_are_independent_after_flip() {
    let ctx = test_ctx(3);
    let v_old = HlsVariant::new(
        0,
        make_init(0),
        (0..20)
            .map(|i| make_seg(i, u64::from(i) * 400, 400))
            .collect(),
        &ctx,
    );
    let v_new = HlsVariant::new(
        1,
        make_init(0),
        (0..20)
            .map(|i| make_seg(i, u64::from(i) * 800, 800))
            .collect(),
        &ctx,
    );
    v_old.set_position(5000);
    v_new.set_position(v_new.segments[10].byte_offset);
    assert_eq!(v_old.get_position(), 5000);
    assert_eq!(v_new.get_position(), v_new.segments[10].byte_offset);

    v_new.advance(123);
    assert_eq!(
        v_old.get_position(),
        5000,
        "advance(V_new) must not touch V_old"
    );
    assert_eq!(v_new.get_position(), v_new.segments[10].byte_offset + 123);
}

#[kithara::test]
fn position_advances_are_strictly_monotonic() {
    let ctx = test_ctx(3);
    let v = HlsVariant::new(0, make_init(0), vec![make_seg(0, 0, 100)], &ctx);
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
    let init = make_init(0);
    init.set_state(SegmentState::Loaded);
    let v = HlsVariant::new(
        0,
        init,
        vec![make_seg(0, 0, 100), make_seg(1, 100, 100)],
        &ctx,
    );
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
fn variant_flip_cancels_v_old_and_replaces_v_new_token_via_rebuild() {
    let ctx = test_ctx(3);
    let segs_old: Vec<SegmentEntry> = (0..20)
        .map(|i| make_seg(i, u64::from(i) * 100, 100))
        .collect();
    let segs_new: Vec<SegmentEntry> = (0..20)
        .map(|i| make_seg(i, u64::from(i) * 200, 200))
        .collect();
    let init_old = make_init(0);
    init_old.set_state(SegmentState::Loaded);
    let init_new = make_init(0);
    init_new.set_state(SegmentState::Loaded);
    let v_old = HlsVariant::new(0, init_old, segs_old, &ctx);
    let v_new = HlsVariant::new(1, init_new, segs_new, &ctx);
    let v_old_token = v_old.cancel_handle();
    let v_new_token_before = v_new.cancel_handle();

    let from_seg = 7_u32;
    v_new.set_position(v_new.segments[from_seg as usize].byte_offset);
    v_old.cancel();
    v_new.rebuild(&ctx, from_seg);

    assert!(v_old_token.is_cancelled());
    assert!(v_new_token_before.is_cancelled(), "rebuild reissues cancel");
    assert!(!v_new.cancel_handle().is_cancelled());
    assert_eq!(
        v_new.get_position(),
        v_new.segments[from_seg as usize].byte_offset
    );
}

#[kithara::test]
fn rebuild_skips_loaded_segment_at_front_of_queue() {
    let ctx = test_ctx(3);
    let init = make_init(0);
    init.set_state(SegmentState::Loaded);
    let segs: Vec<SegmentEntry> = (0..20)
        .map(|i| make_seg(i, u64::from(i) * 100, 100))
        .collect();
    segs[10].set_state(SegmentState::Loaded);
    let v = HlsVariant::new(0, init, segs, &ctx);

    v.rebuild(&ctx, 10);

    let first_seg = v
        .queue
        .lock_sync()
        .front()
        .map(|p| p.segment)
        .expect("queue has at least one segment after rebuild");
    assert_ne!(first_seg, 10, "Loaded segment must not be requeued");
    assert_eq!(first_seg, 11, "first MISSING segment from from_seg");
}
