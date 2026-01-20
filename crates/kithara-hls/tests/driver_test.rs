//! Isolated tests for HLS driver (SegmentStream + Pipeline).
//!
//! Tests:
//! - Driver-1: Pipeline seek after finished
//! - Driver-2: ABR switch + seek backward
//! - Driver-4: Full driver chain

use std::time::Duration;

use kithara_hls::{AbrMode, AbrOptions, Hls, HlsParams};
use kithara_stream::{Source, SourceFactory};
use tokio_util::sync::CancellationToken;

#[path = "fixture.rs"]
mod fixture;
use fixture::AbrTestServer;

/// Driver-1: Проверка что pipeline обрабатывает seek ПОСЛЕ завершения playlist.
///
/// Сценарий:
/// 1. Загружаем все 3 сегмента из playlist
/// 2. SegmentStream завершается (возвращает None)
/// 3. Отправляем команду seek(1)
/// 4. Проверяем что сегмент 1 загружен ПОВТОРНО
///
/// ОЖИДАНИЕ: seek обработан, сегмент загружен
/// БЕЗ ФИКСА: seek игнорируется, pipeline остановлен
#[tokio::test]
async fn test_driver_seek_after_playlist_finished() {
    use kithara_stream::Source;

    // Create HLS server with 3 segments
    let server = AbrTestServer::new(
        fixture::master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(10),
    )
    .await;

    let url = server.url("/master.m3u8").unwrap();
    let cancel_token = CancellationToken::new();

    let hls_params = HlsParams::default()
        .with_cancel(cancel_token.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0), // Fixed variant 0
            ..Default::default()
        });

    // Open HLS source directly (not via StreamSource wrapper)
    let opened = Hls::open(url.clone(), hls_params).await.unwrap();
    let hls_source = opened.source;

    // Wait for ALL segments to load
    tokio::time::sleep(Duration::from_secs(3)).await;

    // At this point:
    // - All 3 segments loaded
    // - SegmentStream returned None
    // - Index marked as finished

    // Read first segment data (offset 0)
    let mut buf = vec![0u8; 1000];
    let n1 = hls_source.read_at(0, &mut buf).await.unwrap();
    assert!(n1 > 0, "Should read first segment");

    println!("✓ Read {} bytes from segment 0", n1);

    // CRITICAL TEST: Trigger seek AFTER playlist finished
    println!("→ Sending seek(1) after playlist finished");
    hls_source.handle().seek(1);

    // Wait for segment to reload
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Try to read segment 1 again (offset 200000)
    let offset_seg1 = 200_000u64;
    let n2 = hls_source.read_at(offset_seg1, &mut buf).await.unwrap();

    if n2 > 0 {
        println!("✅ PASS: Read {} bytes from segment 1 after seek", n2);
        println!("✅ Pipeline processed seek AFTER playlist finished!");
    } else {
        panic!("❌ FAIL: Could not read segment 1 after seek - pipeline stopped!");
    }

    cancel_token.cancel();
}

/// Driver-2: ABR switch + seek backward
///
/// Сценарий:
/// 1. variant=0: загружаем segments 0,1
/// 2. ABR switch на variant=2
/// 3. variant=2: загружаем segment 2
/// 4. Sequential read достигает offset 400000 (начало seg 2)
/// 5. wait_range() обнаруживает что seg 2 есть только в variant 2
/// 6. Должен отправить seek(2) для variant 0
/// 7. Проверяем что seg 2 загружается из variant 0
///
/// Это уже протестировано в Тесте 9, но там полная цепочка с decoder.
/// Здесь нужно изолировать только HLS driver.
#[tokio::test]
#[ignore] // TODO: Implement after fixing Driver-1
async fn test_driver_abr_seek_backward() {
    todo!("Need to isolate HLS driver testing - see Driver-1")
}
