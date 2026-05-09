//! T13 — `Demuxer::next_frame` count соответствует длительности
//! декодированного аудио (контракт C-B3).
//!
//! Без seek'а и без ABR switch'а демуксер должен вызвать `next_frame`
//! не меньше, чем нужно для покрытия декодированной длительности.
//! Существенно меньшее число вызовов означает, что демуксер скакнул
//! через часть сегмента без её декодирования — это chunk-skip,
//! который пользователь слышит.
//!
//! Для AAC LC sample_rate=44100, frame_size=1024 samples →
//! `frames_per_second ≈ 43.07`.

use kithara::{assets::StoreOptions, decode::DecoderBackend, hls::AbrMode};
use kithara_platform::time::Duration;
use kithara_test_utils::{TestServerHelper, TestTempDir, probe_capture, temp_dir};

use crate::abr_contract::helpers::{
    Consts,
    params::{open_audio, wave_fixture_4_variants},
};

const AAC_FRAME_SIZE: f64 = 1024.0;

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn t13_next_frame_count_matches_segment_duration(temp_dir: TestTempDir) {
    let recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants())
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let store = StoreOptions::new(temp_dir.path());

    let _audio = open_audio(
        &url,
        store,
        AbrMode::Manual(Consts::VARIANT_AAC_LQ),
        DecoderBackend::Symphonia,
        3,
    )
    .await;

    // Ждём пока декодер выпустит chunk с timestamp >= 5*segment_duration.
    let target_secs = Consts::SEGMENT_DURATION_SECS * 5.0;
    let target_us = (target_secs * 1_000_000.0) as u64;
    let last_build_chunk = recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("build_chunk")
                    && e.u64("timestamp").is_some_and(|ts| ts >= target_us)
            },
            Duration::from_secs(20),
        )
        .expect("T13 setup — `build_chunk` >= 5*segment_duration не сработал в 20s.");

    // Реально достигнутый decode timestamp (микросекунды → секунды).
    let achieved_us = last_build_chunk
        .u64("timestamp")
        .expect("build_chunk must carry timestamp");
    let decoded_secs = (achieved_us as f64) / 1_000_000.0;

    let next_frame_count = recorder
        .snapshot()
        .into_iter()
        .filter(|e| e.probe_name() == Some("next_frame"))
        .count();

    let expected_frames = decoded_secs * f64::from(Consts::SAMPLE_RATE) / AAC_FRAME_SIZE;

    // 50% slack downward — Pending-calls добавляют сверху, но skip
    // явно режет count ниже даже 50% от expected.
    let lower_bound = (expected_frames * 0.5) as usize;

    assert!(
        next_frame_count >= lower_bound,
        "T13 RED (C-B3) — `next_frame` сработал всего {next_frame_count} раз за \
         {decoded_secs:.2}s декодированного аудио, ожидалось ≥ {lower_bound} \
         (50% от {expected_frames:.0} = decoded_secs * {sr}/{frame_size}). \
         Демуксер скакнул через часть сегмента без декодирования.",
        sr = Consts::SAMPLE_RATE,
        frame_size = AAC_FRAME_SIZE,
    );
}
