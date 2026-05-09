//! T12 — read_at offsets contiguous и timeline byte_position
//! монотонен во время steady playback (контракты C-B1, C-B2).
//!
//! Без seek'а и без ABR switch'а:
//! - последовательность offset'ов `read_at` должна быть монотонно
//!   неубывающей;
//! - последовательность `Timeline::set_byte_position(position)` должна
//!   быть монотонно неубывающей.
//!
//! Любой "скачок назад" в любой плоскости — это chunk-skip, который
//! пользователь слышит как пропуск/прыжок внутри сегмента.
//!
//! Ждём через пробу `build_chunk` пока playback покроет ~5 сегментов,
//! потом снимаем snapshot всех `read_at`/`set_byte_position` событий.

use kithara::{assets::StoreOptions, decode::DecoderBackend, hls::AbrMode};
use kithara_platform::time::Duration;
use kithara_test_utils::{TestServerHelper, TestTempDir, probe_capture, temp_dir};

use crate::abr_contract::helpers::{
    Consts,
    params::{open_audio, wave_fixture_4_variants},
};

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn t12_read_at_offsets_contiguous_within_segment(temp_dir: TestTempDir) {
    let recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants())
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();
    let store = StoreOptions::new(temp_dir.path());

    // Manual variant — никаких ABR switch'ей. Никакого seek — только linear playback.
    let _audio = open_audio(
        &url,
        store,
        AbrMode::Manual(Consts::VARIANT_AAC_LQ),
        DecoderBackend::Symphonia,
        3,
    )
    .await;

    // Ждём пока декодер выпустит chunk с timestamp >= 5*segment_duration.
    let target_us = (Consts::SEGMENT_DURATION_SECS * 5.0 * 1_000_000.0) as u64;
    recorder
        .wait_for_probe(
            |e| {
                e.probe_name() == Some("build_chunk")
                    && e.u64("timestamp").is_some_and(|ts| ts >= target_us)
            },
            Duration::from_secs(20),
        )
        .expect(
            "T12 setup — `build_chunk` >= 5*segment_duration не сработал в 20s. \
             Либо декодер застрял, либо playback не дошёл до 5 сегментов.",
        );

    // C-B1: read_at offsets монотонно неубывают.
    let read_offsets: Vec<(u64, u64)> = recorder
        .snapshot()
        .into_iter()
        .filter(|e| e.probe_name() == Some("read_at"))
        .filter_map(|e| Some((e.seq()?, e.u64("offset")?)))
        .collect();

    assert!(
        read_offsets.len() >= 4,
        "T12 setup — слишком мало read_at событий ({n}); проба не активна или \
         playback не дошёл до сегментов.",
        n = read_offsets.len(),
    );

    let mut backward_jumps: Vec<String> = Vec::new();
    for window in read_offsets.windows(2) {
        let (seq_prev, off_prev) = window[0];
        let (seq_next, off_next) = window[1];
        if off_next < off_prev {
            backward_jumps.push(format!(
                "  seq {seq_prev}->{seq_next}: offset {off_prev} -> {off_next} (back by {})",
                off_prev - off_next
            ));
        }
    }

    assert!(
        backward_jumps.is_empty(),
        "T12 RED (C-B1) — `read_at` offsets идут назад {n} раз во время steady playback \
         (без seek'а, без ABR-switch'а). Это chunk-skip внутри сегмента: \
         плеер прочитал часть, скакнул назад. Скачки:\n{events}",
        n = backward_jumps.len(),
        events = backward_jumps.join("\n"),
    );

    // C-B2: timeline byte_position монотонно неубывает.
    let timeline_positions: Vec<(u64, u64)> = recorder
        .snapshot()
        .into_iter()
        .filter(|e| e.probe_name() == Some("set_byte_position"))
        .filter_map(|e| Some((e.seq()?, e.u64("position")?)))
        .collect();

    let mut tl_backward: Vec<String> = Vec::new();
    for window in timeline_positions.windows(2) {
        let (seq_prev, pos_prev) = window[0];
        let (seq_next, pos_next) = window[1];
        if pos_next < pos_prev {
            tl_backward.push(format!(
                "  seq {seq_prev}->{seq_next}: position {pos_prev} -> {pos_next} (back by {})",
                pos_prev - pos_next
            ));
        }
    }

    assert!(
        tl_backward.is_empty(),
        "T12 RED (C-B2) — Timeline `set_byte_position` идёт назад {n} раз во время \
         steady playback. Это означает, что reader-side state перепрыгнул назад \
         внутри сегмента. Скачки:\n{events}",
        n = tl_backward.len(),
        events = tl_backward.join("\n"),
    );
}
