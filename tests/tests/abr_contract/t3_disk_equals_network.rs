//! T3 — disk inventory equals network ground truth (axiom A8).
//!
//! For every variant the post-test set equality must hold:
//!
//! ```
//! { (variant, seg) | emit_fetch_cmd, plan_need_init = false }
//!   == { (variant, seg) | file on disk }
//! ```
//!
//! Both sides are independent observations of the same scheduler
//! decisions: `emit_fetch_cmd` is the in-process probe, the disk
//! listing is the OS-level filesystem state of the asset store. A
//! mismatch in either direction is a bug:
//!
//! * file on disk that no `emit_fetch_cmd` event mentions ⇒ scheduler
//!   wrote a segment without recording the decision (race in the
//!   probe wiring or a stray writer outside the scheduler);
//! * `emit_fetch_cmd` event without a corresponding file ⇒ scheduler
//!   spawned a fetch that never landed (cancellation race or silent
//!   write failure that didn't propagate to the probe).
//!
//! The comparison is run AFTER the audio pipeline has been dropped
//! and the asset store has flushed everything to the filesystem.

use std::collections::BTreeSet;

use kithara::{assets::StoreOptions, audio::PcmReader, decode::DecoderBackend, hls::AbrMode};
use kithara_platform::time::Duration;
use kithara_test_utils::{TestServerHelper, TestTempDir, probe_capture, temp_dir};

use super::helpers::{
    Consts,
    assertions::assert_sets_equal,
    counters::{all_fetch_emits, disk_files_per_variant},
    params::{NetworkProfile, open_audio, wave_fixture_4_variants},
};

#[expect(clippy::too_many_arguments, reason = "test parameter context")]
async fn run_case(
    temp_dir: TestTempDir,
    variant_from: usize,
    variant_to: usize,
    backend: DecoderBackend,
    network: NetworkProfile,
) {
    let recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants().delay_rules(network.delay_rules()))
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let temp_path = temp_dir.path().to_path_buf();
    let store = StoreOptions::new(temp_dir.path());

    let audio = open_audio(&url, store, AbrMode::Manual(variant_from), backend, 3).await;

    let label = format!(
        "{variant_from}->{variant_to} {backend:?} net={net}",
        net = network.label(),
    );

    // Warmup — ждём пока декодер выпустит chunk с timestamp >= PRE_SWITCH_TARGET.
    let pre_switch_target_us = (Consts::PRE_SWITCH_TARGET_SECS * 1_000_000.0) as u64;
    recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("build_chunk")
                    && e.u64("timestamp")
                        .is_some_and(|ts| ts >= pre_switch_target_us)
            },
            Duration::from_secs(35),
        )
        .unwrap_or_else(|| {
            panic!("T3 [{label}]: warmup `build_chunk` >= {pre_switch_target_us}μs not seen in 35s")
        });

    // Триггер switch.
    audio
        .abr_handle()
        .expect("HLS Audio must expose an ABR handle")
        .set_mode(AbrMode::Manual(variant_to))
        .expect("set_mode");

    // Ждём commit V_new seg-0 init — сигнал, что V_new пошёл по сети.
    recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("commit_segment")
                    && e.u64("variant") == Some(variant_to as u64)
                    && e.u64("seg_idx") == Some(0)
                    && e.u64("init_len").is_some_and(|l| l > 0)
            },
            Duration::from_secs(10),
        )
        .unwrap_or_else(|| {
            panic!("T3 [{label}]: commit_segment(V_new, 0, init_len>0) not seen in 10s")
        });

    // Дополнительные `commit_segment` для V_new чтобы убедиться, что
    // post-switch фетчи реально записались.
    recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("commit_segment")
                    && e.u64("variant") == Some(variant_to as u64)
                    && e.u64("seg_idx").is_some_and(|s| s >= 1)
            },
            Duration::from_secs(10),
        )
        .unwrap_or_else(|| {
            panic!("T3 [{label}]: commit_segment(V_new, seg_idx>=1) not seen in 10s")
        });

    drop(audio);

    let emits = all_fetch_emits(&recorder);
    let mut emitted: std::collections::BTreeMap<usize, BTreeSet<usize>> =
        std::collections::BTreeMap::new();
    for emit in emits.iter().filter(|e| !e.plan_need_init) {
        emitted
            .entry(emit.variant)
            .or_default()
            .insert(emit.segment_index);
    }
    let on_disk = disk_files_per_variant(&temp_path);

    let mut variants: BTreeSet<usize> = BTreeSet::new();
    for v in emitted.keys() {
        variants.insert(*v);
    }
    for v in on_disk.keys() {
        variants.insert(*v);
    }
    for variant in variants {
        let emitted_set = emitted.get(&variant).cloned().unwrap_or_default();
        let disk_set = on_disk.get(&variant).cloned().unwrap_or_default();
        assert_sets_equal(
            &disk_set,
            &emitted_set,
            &format!("T3 [{label}] A8: disk vs emit_fetch_cmd mismatch on variant {variant}"),
        );
    }
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(60)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
#[case::lq_to_hq(
    Consts::VARIANT_AAC_LQ,
    Consts::VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::hq_to_lq(
    Consts::VARIANT_AAC_HQ,
    Consts::VARIANT_AAC_LQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::hq_to_flac(
    Consts::VARIANT_AAC_HQ,
    Consts::VARIANT_FLAC,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::flac_to_lq(
    Consts::VARIANT_FLAC,
    Consts::VARIANT_AAC_LQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::lq_to_hq_slow(
    Consts::VARIANT_AAC_LQ,
    Consts::VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Slow { target_variant: Consts::VARIANT_AAC_LQ }
)]
#[case::lq_to_hq_flaky(
    Consts::VARIANT_AAC_LQ,
    Consts::VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Flaky
)]
async fn t3_disk_equals_network(
    temp_dir: TestTempDir,
    #[case] variant_from: usize,
    #[case] variant_to: usize,
    #[case] backend: DecoderBackend,
    #[case] network: NetworkProfile,
) {
    run_case(temp_dir, variant_from, variant_to, backend, network).await;
}
