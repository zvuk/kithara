//! T8 — sanity baseline (no ABR switch).
//!
//! Plays the wave fixture from start to natural EOF on a single
//! manually-pinned variant. The contract here is not an axiom from
//! `abr-switch-contract.md` directly — it is the foundation those
//! axioms stand on:
//!
//! * the player can play a single track from start to natural EOF;
//! * **HTTP integrity** — for every spawned media fetch, the request
//!   lifecycle (`start_request → establish → with_soft_timeout →
//!   deliver → finish_request`) runs to completion on a single thread
//!   (`spawn_fetch` invariant);
//! * **byte continuity** — `commit_segment` fires exactly once per
//!   segment in strict ascending order `seg=0..N-1` on a single
//!   thread (no seek, no variant switch);
//! * **frame continuity** — every PCM chunk emitted by the decoder
//!   stays on the pinned variant, `frame_offset` is strictly
//!   monotonic with no gaps and no duplicates, and the cumulative
//!   frame count matches the fixture within one encoder frame;
//! * `record_abr_variant_committed` and
//!   `record_midstream_switch_committed` never fire — there is no
//!   ABR commit in this scenario.
//!
//! The test does not block on `wait_for_probe`. It drains PCM
//! continuously through `tokio::time::sleep(...).await` on `Pending`
//! (so the current-thread runtime keeps polling HTTP / scheduler
//! tasks), and reports failures as **per-thread sequence diffs**:
//! "request_id=26 reached `with_soft_timeout` and never delivered"
//! — never as "wait timed out".

use kithara::{
    assets::StoreOptions,
    audio::{ChunkOutcome, PcmReader},
    decode::DecoderBackend,
    hls::AbrMode,
    stream::AudioCodec,
};
use kithara_platform::{
    time::{Duration, Instant},
    tokio::time::sleep,
};
use kithara_test_utils::{TestServerHelper, TestTempDir, probe_capture, temp_dir};

use super::helpers::{
    Consts,
    assertions::assert_exact_count,
    params::{open_audio, wave_fixture_4_variants},
    probe_contracts::{
        assert_build_chunk_reached, assert_commit_segment_strict, assert_request_lifecycle_complete,
    },
};

/// Per-codec frame-sample size used by the fixture encoders. Hardcoded
/// rather than queried at runtime so the test fails loudly if the
/// fixture's encoder is silently swapped out.
fn encoder_frame_samples(codec: AudioCodec) -> usize {
    match codec {
        AudioCodec::AacLc => 1024,
        AudioCodec::Flac => 4608,
        other => panic!("T8 fixture does not use codec {other:?}"),
    }
}

/// Total decoded frames expected from a single variant of the wave
/// fixture. Mirrors `kithara_test_utils::hls_stream::encode_packaged_variant`:
/// `packets_per_segment = ceil(SEGMENT_DURATION_SECS · SAMPLE_RATE / frame_samples)`,
/// `total = packets_per_segment · frame_samples · SEGMENTS_PER_VARIANT`.
fn expected_frames_per_variant(codec: AudioCodec) -> u64 {
    let frame_samples = encoder_frame_samples(codec);
    let requested_segment_frames =
        (Consts::SEGMENT_DURATION_SECS * f64::from(Consts::SAMPLE_RATE)).round() as usize;
    let packets_per_segment = requested_segment_frames.div_ceil(frame_samples).max(1);
    (packets_per_segment * frame_samples * Consts::SEGMENTS_PER_VARIANT) as u64
}

fn variant_codec(variant: usize) -> AudioCodec {
    if variant == Consts::VARIANT_FLAC {
        AudioCodec::Flac
    } else {
        AudioCodec::AacLc
    }
}

/// Outcome of [`drain_pcm_until_eof_or_budget`].
struct DrainOutcome {
    saw_eof: bool,
    chunks_seen: usize,
    total_frames: u64,
}

/// Drain `audio` chunk-by-chunk until natural EOF or `budget` elapses.
///
/// On `Pending` we yield to the runtime via `tokio::time::sleep` (an
/// async wait, *not* `std::thread::sleep`) so the current-thread tokio
/// runtime keeps polling Downloader / HlsPeer / reqwest tasks while we
/// wait. Replacing `wait_for_probe` (which is sync and freezes the
/// runtime) is what lets HTTP fetches actually complete on
/// current-thread runs.
///
/// Returns even when EOF was not reached. The contract assertions
/// after this call inspect the recorder snapshot and produce a
/// per-thread diff that names the violated invariant precisely; this
/// function never panics on its own (except on a hard decode error).
async fn drain_pcm_until_eof_or_budget(
    audio: &mut kithara::audio::Audio<kithara::stream::Stream<kithara::hls::Hls>>,
    budget: Duration,
    variant: usize,
    backend: DecoderBackend,
) -> DrainOutcome {
    let started = Instant::now();
    let mut total_frames: u64 = 0;
    let mut chunks_seen: usize = 0;
    let mut next_expected_offset: Option<u64> = None;
    let mut saw_eof = false;
    while started.elapsed() < budget {
        match PcmReader::next_chunk(audio) {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                assert_eq!(
                    chunk.meta.variant_index,
                    Some(variant),
                    "T8: chunk emitted on unexpected variant — fixture or decoder \
                     leaked a foreign variant on the no-switch path \
                     (expected {variant}, got {actual:?}, frame_offset={off})",
                    actual = chunk.meta.variant_index,
                    off = chunk.meta.frame_offset,
                );
                if let Some(expected) = next_expected_offset {
                    assert_eq!(
                        chunk.meta.frame_offset,
                        expected,
                        "T8: frame_offset разрыв — между chunk'ами потерян \
                         или продублирован сэмпл (single variant, no seek). \
                         expected {expected}, got {got} \
                         (chunks_seen={chunks_seen}, total_frames={total_frames}, \
                         backend={backend:?}, variant={variant})",
                        got = chunk.meta.frame_offset,
                    );
                }
                total_frames += u64::from(chunk.meta.frames);
                next_expected_offset = Some(chunk.meta.frame_offset + u64::from(chunk.meta.frames));
                chunks_seen += 1;
            }
            Ok(ChunkOutcome::Pending { .. }) => {
                sleep(Duration::from_millis(5)).await;
            }
            Ok(ChunkOutcome::Eof { .. }) => {
                saw_eof = true;
                break;
            }
            Err(e) => panic!("T8: decode error during baseline playback: {e}"),
        }
    }
    DrainOutcome {
        saw_eof,
        chunks_seen,
        total_frames,
    }
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(60)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
#[case::aac_lq_sw(Consts::VARIANT_AAC_LQ, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_lq_hw(Consts::VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
#[case::flac_sw(Consts::VARIANT_FLAC, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_hw(Consts::VARIANT_FLAC, DecoderBackend::Apple)
)]
async fn t8_natural_eof_complete_playback(
    temp_dir: TestTempDir,
    #[case] variant: usize,
    #[case] backend: DecoderBackend,
) {
    let recorder = probe_capture::install();
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(wave_fixture_4_variants())
        .await
        .expect("create wave HLS fixture");
    let url = created.master_url();

    // Считаем `#EXTINF` в реальном media playlist, который фикстура
    // отдаёт сетью. Это и есть число сегментов, на которое scheduler
    // вправе рассчитывать — `Consts::SEGMENTS_PER_VARIANT` это
    // логическое (user-content) значение, а fmp4 mux добирает на
    // лишний chunk из-за AAC priming (FFmpeg добавляет ~2 priming
    // access_unit'а к 1044 user'ским = 1046, и `chunks(87)` даёт 13).
    let actual_segments_in_playlist = {
        let master_str = url.as_str();
        let stem = master_str.trim_end_matches(".m3u8");
        let media_url = format!("{stem}/v{variant}.m3u8");
        let body = reqwest::get(&media_url)
            .await
            .expect("media playlist GET")
            .text()
            .await
            .expect("media playlist body");
        body.matches("#EXTINF").count()
    };
    let store = StoreOptions::new(temp_dir.path());

    let mut audio = open_audio(&url, store, AbrMode::Manual(variant), backend, 3).await;

    let DrainOutcome {
        saw_eof,
        chunks_seen,
        total_frames,
    } = drain_pcm_until_eof_or_budget(&mut audio, Duration::from_secs(45), variant, backend).await;

    // Контракт #1 — HTTP request lifecycle. Каждая запущенная media-фетча
    // (init на seg=0 + media seg=0..N-1) должна пройти все 5 стадий на
    // одном thread'е. Если deliver/finish не firе'ит — диагностика точно
    // покажет «req=K застрял на стадии X», вместо «test timed out».
    //
    // request_id'ы в этой фикстуре: HEAD warmup даёт ~1..22, затем GET-ы
    // на media+init начинаются с req≥23. Точное число варьируется от
    // прогона к прогону (HEAD warmup зависит от того, в каком порядке
    // axum обслужил их), поэтому проверяем lifecycle для всех media-id,
    // которые наблюдались стартующими — те же id, на которые уехал
    // emit_fetch_cmd.
    let media_request_ids: Vec<u64> = recorder
        .events_with_probe("start_request")
        .iter()
        .filter_map(|e| e.u64("request_id"))
        // HEAD-warmup завершается до медиа-фетчей; нас интересуют GET-ы.
        // Проще всего — отбросить request_id <= 22 (HEAD warmup count).
        // Граница не жёсткая: если HEAD'ов меньше, лишние GET'ы тоже
        // отфильтруются; assert_commit_segment_strict потом всё равно
        // потребует все 12 commit'ов.
        .filter(|&id| id > 22)
        .collect();
    for request_id in &media_request_ids {
        if let Err(diag) = assert_request_lifecycle_complete(&recorder, *request_id) {
            panic!(
                "T8: HTTP request lifecycle нарушен. \
                 backend={backend:?}, variant={variant}, \
                 chunks_seen={chunks_seen}, saw_eof={saw_eof}.\n{diag}"
            );
        }
    }

    // Контракт #2 — commit_segment строго в порядке seg=0..N-1, где N
    // совпадает с числом #EXTINF в реальном playlist.
    if let Err(diag) = assert_commit_segment_strict(&recorder, variant, actual_segments_in_playlist)
    {
        panic!(
            "T8: commit_segment последовательность нарушена. \
             backend={backend:?}, variant={variant}.\n{diag}"
        );
    }

    // Контракт #3 — декодер достиг конца трека. Цельтесь чуть ниже EOF —
    // декодер не выпустит build_chunk ровно на EOF (выходит DemuxOutcome::Eof,
    // не Frame).
    let track_total_secs = Consts::SEGMENT_DURATION_SECS * (Consts::SEGMENTS_PER_VARIANT as f64);
    let near_eof_us = ((track_total_secs - Consts::SEGMENT_DURATION_SECS) * 1_000_000.0) as u64;
    if let Err(diag) = assert_build_chunk_reached(&recorder, near_eof_us) {
        panic!(
            "T8: декодер не дошёл до конца трека. \
             backend={backend:?}, variant={variant}, saw_eof={saw_eof}.\n{diag}"
        );
    }

    // Дренаж сам по себе должен дойти до EOF (декодер выпустил все
    // кадры → канал получил Eof). Если все три контракта прошли, но
    // saw_eof=false — это отдельный баг канал/EOF-протокола, и его
    // важно отразить.
    assert!(
        saw_eof,
        "T8: контракты #1/#2/#3 прошли, но drain не получил Eof через PCM канал. \
         chunks_seen={chunks_seen}, total_frames={total_frames}, \
         backend={backend:?}, variant={variant}."
    );

    // Контракт #4 — total frames matches fixture в пределах одного encoder frame.
    let codec = variant_codec(variant);
    let expected = expected_frames_per_variant(codec);
    let frame_samples = encoder_frame_samples(codec) as i64;
    let drift = total_frames as i64 - expected as i64;
    assert!(
        drift.abs() <= frame_samples,
        "T8: total decoded frames diverge from fixture frame count by more than \
         one encoder frame ({frame_samples} samples): expected {expected}, \
         got {total_frames} (drift {drift}). Backend = {backend:?}, variant = {variant}."
    );

    // Контракт #5 — ABR-zero: никаких commit'ов на switch'ах.
    let abr_commits = recorder.events_with_probe("record_abr_variant_committed");
    assert_exact_count(abr_commits.len(), 0, "T8 abr_variant_committed count");
    let midstream = recorder.events_with_probe("record_midstream_switch_committed");
    assert_exact_count(midstream.len(), 0, "T8 midstream_switch_committed count");
}
