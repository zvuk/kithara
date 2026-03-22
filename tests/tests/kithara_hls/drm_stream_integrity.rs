//! Byte-stream integrity validation for HLS vs DRM across ABR modes and storage backends.
//!
//! Scans raw fMP4 box structure from the virtual byte stream to verify that
//! decrypted segment data is contiguous, box headers are valid, and
//! `total_bytes` (stream length) matches the actual box coverage.
//!
//! Parametrized: `{hls, drm} × {ephemeral, disk} × {manual(0), manual(3), auto(0)}`.

use std::{
    io::{ErrorKind, Read, Seek, SeekFrom},
    num::NonZeroUsize,
};

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
use kithara_platform::{
    thread,
    time::{Duration, Instant},
    tokio::task::spawn_blocking,
};
use kithara_test_utils::{TestTempDir, serve_assets, temp_dir};
use tokio_util::sync::CancellationToken;

fn is_known_box(tag: &[u8; 4]) -> bool {
    matches!(
        tag,
        b"moof" | b"mdat" | b"styp" | b"sidx" | b"free" | b"ftyp" | b"moov" | b"emsg"
    )
}

fn parse_box_header(buf: &[u8]) -> Option<(u64, [u8; 4])> {
    if buf.len() < 8 {
        return None;
    }
    let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let tag = [buf[4], buf[5], buf[6], buf[7]];
    if size == 1 && buf.len() >= 16 {
        let ext = u64::from_be_bytes(buf[8..16].try_into().ok()?);
        Some((ext, tag))
    } else {
        Some((u64::from(size), tag))
    }
}

fn read_exact_retry(stream: &mut Stream<Hls>, buf: &mut [u8], timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    let mut filled = 0;
    while filled < buf.len() && Instant::now() < deadline {
        match stream.read(&mut buf[filled..]) {
            Ok(0) => thread::sleep(Duration::from_millis(5)),
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                if e.to_string().contains("variant change") {
                    stream.clear_variant_fence();
                    continue;
                }
                break;
            }
        }
    }
    filled
}

/// Scan fMP4 boxes from `start_pos`. Returns `(boxes, last_end)`.
fn scan_boxes(
    stream: &mut Stream<Hls>,
    start_pos: u64,
    max_end: u64,
    label: &str,
) -> (Vec<(u64, u64, String)>, u64) {
    let mut boxes = Vec::new();
    let mut pos = start_pos;

    if stream.seek(SeekFrom::Start(pos)).is_err() {
        return (boxes, pos);
    }

    while pos < max_end {
        let mut header = [0u8; 16];
        let n = read_exact_retry(stream, &mut header[..8], Duration::from_secs(5));
        if n < 8 {
            eprintln!("[{label}] scan: EOF at pos={pos} (read {n} bytes)");
            break;
        }
        let Some((size, tag)) = parse_box_header(&header[..n]) else {
            break;
        };
        if size < 8 && size != 0 {
            let hex: String = header[..8]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            eprintln!("[{label}] scan: INVALID box at pos={pos} size={size} hex=[{hex}]");
            break;
        }
        let tag_str = String::from_utf8_lossy(&tag).to_string();
        if !is_known_box(&tag) {
            let hex: String = header[..8]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            eprintln!("[{label}] scan: UNKNOWN box at pos={pos} tag='{tag_str}' hex=[{hex}]");
            break;
        }
        eprintln!(
            "[{label}] scan: box #{:<3} pos={:<10} size={:<10} tag='{tag_str}'",
            boxes.len(),
            pos,
            size
        );
        boxes.push((pos, size, tag_str));

        if size == 0 {
            break;
        }
        pos += size;
        if stream.seek(SeekFrom::Start(pos)).is_err() {
            eprintln!("[{label}] scan: seek to {pos} failed");
            break;
        }
    }
    let last_end = boxes.last().map_or(start_pos, |(o, s, _)| o + s);
    (boxes, last_end)
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(45)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
// path × label × ephemeral × abr_variant
#[case::hls_disk_v0("/hls/master.m3u8", "HLS-disk-v0", false, 0)]
#[case::hls_disk_v3("/hls/master.m3u8", "HLS-disk-v3", false, 3)]
#[case::hls_eph_v0("/hls/master.m3u8", "HLS-eph-v0", true, 0)]
#[case::hls_eph_v3("/hls/master.m3u8", "HLS-eph-v3", true, 3)]
#[case::drm_disk_v0("/drm/master.m3u8", "DRM-disk-v0", false, 0)]
#[case::drm_disk_v3("/drm/master.m3u8", "DRM-disk-v3", false, 3)]
#[case::drm_eph_v0("/drm/master.m3u8", "DRM-eph-v0", true, 0)]
#[case::drm_eph_v3("/drm/master.m3u8", "DRM-eph-v3", true, 3)]
// Auto cases: start on v0, ABR will switch up
#[case::hls_disk_auto("/hls/master.m3u8", "HLS-disk-auto", false, 99)]
#[case::hls_eph_auto("/hls/master.m3u8", "HLS-eph-auto", true, 99)]
#[case::drm_disk_auto("/drm/master.m3u8", "DRM-disk-auto", false, 99)]
#[case::drm_eph_auto("/drm/master.m3u8", "DRM-eph-auto", true, 99)]
async fn drm_stream_byte_integrity(
    temp_dir: TestTempDir,
    #[case] path: &str,
    #[case] label: &str,
    #[case] ephemeral: bool,
    #[case] abr_variant: usize,
) {
    let server = serve_assets().await;
    let url = server.url(path);
    let cancel = CancellationToken::new();

    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        store.ephemeral = true;
        store.cache_capacity = Some(NonZeroUsize::new(40).expect("nonzero"));
    }

    let abr_mode = if abr_variant == 99 {
        AbrMode::Auto(Some(0))
    } else {
        AbrMode::Manual(abr_variant)
    };

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_cancel(cancel.clone())
        .with_abr(AbrOptions {
            mode: abr_mode,
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_millis(100),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(hls_config)
        .await
        .expect("create HLS stream");

    let label = label.to_string();
    let result = spawn_blocking(move || {
        let label = &label;

        // Phase 1: Read to EOF, clearing variant fence on each VariantChangeError.
        eprintln!("[{label}] Phase 1: reading to EOF");
        let mut buf = vec![0u8; 65536];
        let mut total_read = 0u64;
        let deadline = Instant::now() + Duration::from_secs(25);
        let mut hit_deadline = false;
        let mut read_error = None;
        let mut saw_eof = false;

        while Instant::now() < deadline {
            match stream.read(&mut buf) {
                Ok(0) => {
                    thread::sleep(Duration::from_millis(50));
                    if let Ok(0) = stream.read(&mut buf) {
                        saw_eof = true;
                        break;
                    }
                }
                Ok(n) => total_read += n as u64,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    if e.to_string().contains("variant change") {
                        stream.clear_variant_fence();
                        continue;
                    }
                    eprintln!("[{label}] read error: {e}");
                    read_error = Some(e.to_string());
                    break;
                }
            }
        }

        if !saw_eof && read_error.is_none() && Instant::now() >= deadline {
            hit_deadline = true;
        }

        let stream_len = stream.len().unwrap_or(0);
        eprintln!("[{label}] Phase 1 done: total_read={total_read} stream_len={stream_len}");

        // Phase 2: Seek to beginning and scan fMP4 box structure.
        stream.clear_variant_fence();
        eprintln!("[{label}] Phase 2: scanning fMP4 from pos 0");
        let (boxes, last_end) = scan_boxes(&mut stream, 0, stream_len.max(total_read), label);

        // Verify contiguity.
        let mut expected_pos = 0u64;
        for (i, (offset, size, tag)) in boxes.iter().enumerate() {
            if *offset != expected_pos {
                let gap = (*offset as i64) - (expected_pos as i64);
                panic!(
                    "[{label}] GAP at box #{i} '{tag}': expected pos {expected_pos}, \
                     got {offset} (gap={gap})"
                );
            }
            expected_pos = offset + size;
        }

        let moof_count = boxes.iter().filter(|(_, _, t)| t == "moof").count();
        let mdat_count = boxes.iter().filter(|(_, _, t)| t == "mdat").count();

        eprintln!(
            "[{label}] Phase 2 result: {} boxes, {moof_count} moofs, {mdat_count} mdats, \
             last_end={last_end}, stream_len={stream_len}",
            boxes.len()
        );

        // Phase 3: Assertions.
        assert!(
            boxes.len() >= 4,
            "[{label}] Expected at least 4 fMP4 boxes, found {}",
            boxes.len()
        );
        assert_eq!(moof_count, mdat_count, "[{label}] moof/mdat count mismatch");

        // total_read should match stream_len (what we actually read == declared total).
        // Allow small delta for DRM padding reconciliation timing.
        let read_vs_len = (total_read as i64) - (stream_len as i64);
        eprintln!("[{label}] total_read - stream_len = {read_vs_len}");
        assert!(
            !hit_deadline || read_vs_len.unsigned_abs() < 1024,
            "[{label}] Phase 1 hit deadline before EOF: total_read={total_read} \
             stream_len={stream_len} delta={read_vs_len}"
        );
        if let Some(error) = read_error {
            panic!("[{label}] Phase 1 ended with read error before EOF: {error}");
        }
        assert!(
            read_vs_len.unsigned_abs() < 1024,
            "[{label}] Phase 1 did not read full stream: total_read={total_read} \
             stream_len={stream_len} delta={read_vs_len} saw_eof={saw_eof}"
        );

        // fMP4 box coverage should match total_read.
        // last_end = where the last box SAYS it ends (based on box size headers).
        // total_read = how many bytes we actually read.
        // If last_end > total_read → mdat sizes extend past what we read (BAD: possible encrypted size leak).
        // If last_end < total_read → boxes don't cover all data (gap at tail).
        let coverage_delta = (last_end as i64) - (total_read as i64);
        eprintln!("[{label}] box coverage delta (last_end - total_read) = {coverage_delta}");

        assert!(
            coverage_delta.unsigned_abs() < 1024,
            "[{label}] fMP4 box coverage doesn't match actual data: \
             last_end={last_end} total_read={total_read} delta={coverage_delta}"
        );

        eprintln!("[{label}] PASSED");
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await
    .expect("join");

    if let Err(e) = result {
        panic!("{e}");
    }
    cancel.cancel();
}
