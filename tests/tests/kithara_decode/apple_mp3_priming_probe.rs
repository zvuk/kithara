use std::io::Cursor;

use kithara::decode::{DecoderBackend, DecoderConfig, DecoderFactory, PcmChunk};
use kithara_integration_tests::{SignalFormat, SignalSpec, SignalSpecLength, TestServerHelper};
use kithara_platform::time::Duration;
use reqwest::Client;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;

/// Locate the LAME encoder tag inside an MP3's Xing/Info header.
///
/// LAME tag layout convention (LAME tech FAQ):
///   1. The first MP3 frame after the ID3 header carries a Xing or Info
///      header (4-byte marker `Xing` or `Info`).
///   2. Exactly 120 bytes after that marker is the start of the 36-byte
///      LAME extension (encoder name "LAME"/"Lavc"/"Lavf" at offset 0,
///      enc_delay/enc_padding 24-bit field at offset +21..+24).
///
/// A bare search for "LAME"/"Lavc"/"Lavf" inside the bitstream is wrong:
/// those four-byte sequences appear in audio data by coincidence.
fn find_lame_tag_offset(bytes: &[u8]) -> Option<usize> {
    let xing = bytes
        .windows(4)
        .position(|w| w == b"Xing" || w == b"Info")?;
    let lame = xing + 120;
    if lame + 24 > bytes.len() {
        return None;
    }
    Some(lame)
}

fn read_enc_delay_padding(bytes: &[u8], lame_tag_off: usize) -> Option<(u32, u32)> {
    let s = lame_tag_off + 21;
    let triple = bytes.get(s..s + 3)?;
    let val = (u32::from(triple[0]) << 16) | (u32::from(triple[1]) << 8) | u32::from(triple[2]);
    let enc_delay = val >> 12;
    let enc_padding = val & 0xFFF;
    Some((enc_delay, enc_padding))
}

fn write_enc_delay(bytes: &mut [u8], lame_tag_off: usize, new_enc_delay: u32) {
    let s = lame_tag_off + 21;
    let (_, old_padding) = read_enc_delay_padding(bytes, lame_tag_off).unwrap_or((0, 0));
    let val = ((new_enc_delay & 0xFFF) << 12) | (old_padding & 0xFFF);
    bytes[s] = ((val >> 16) & 0xFF) as u8;
    bytes[s + 1] = ((val >> 8) & 0xFF) as u8;
    bytes[s + 2] = (val & 0xFF) as u8;
}

/// Decode the given MP3 bytes via the chosen backend at the requested
/// `gapless` setting and return the index of the first sample whose
/// absolute value exceeds `threshold`. For a sawtooth signal that
/// starts at amplitude ~1.0, this index is the count of leading
/// silence samples the decoder emitted.
fn measure_leading_silence(
    mp3_bytes: Vec<u8>,
    backend: DecoderBackend,
    gapless: bool,
) -> (usize, f32, usize) {
    let mut config = DecoderConfig::default();
    config.backend = backend;
    config.gapless = gapless;
    config.hint = Some("mp3".to_owned());
    let mut decoder =
        DecoderFactory::create_with_probe(Cursor::new(mp3_bytes), Some("mp3"), config)
            .expect("probe MP3 decoder");
    let mut left = Vec::<f32>::with_capacity(SAMPLE_RATE as usize);
    let chan = usize::from(CHANNELS);
    for _ in 0..200 {
        let outcome = decoder.next_chunk().expect("decode chunk");
        let Ok(chunk) = PcmChunk::try_from(outcome) else {
            break;
        };
        if chunk.samples.is_empty() {
            continue;
        }
        for frame_chunk in chunk.samples.chunks_exact(chan) {
            left.push(frame_chunk[0]);
        }
        if left.len() >= (SAMPLE_RATE as usize) {
            break;
        }
    }
    let threshold = 0.5_f32;
    let total = left.len();
    let idx = left
        .iter()
        .position(|v| v.abs() > threshold)
        .unwrap_or(total);
    let value = left.get(idx).copied().unwrap_or(0.0);
    (idx, value, total)
}

#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::symphonia_default(DecoderBackend::Symphonia, None)]
#[case::symphonia_320k(DecoderBackend::Symphonia, Some(320_000))]
#[case::symphonia_64k(DecoderBackend::Symphonia, Some(64_000))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_default(DecoderBackend::Apple, None),
    case::apple_320k(DecoderBackend::Apple, Some(320_000)),
    case::apple_64k(DecoderBackend::Apple, Some(64_000))
)]
async fn mp3_raw_decoder_shift_vs_reference(
    #[case] backend: DecoderBackend,
    #[case] bit_rate: Option<u64>,
) {
    let server = TestServerHelper::new().await;
    let client = Client::new();
    let spec = SignalSpec {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        length: SignalSpecLength::Seconds(2.0),
        format: SignalFormat::Mp3,
        bit_rate,
    };
    let url = server.sawtooth(&spec).await;
    let response = client.get(url).send().await.expect("fetch sawtooth mp3");
    assert_eq!(response.status(), 200);
    let bytes = response.bytes().await.expect("mp3 body").to_vec();

    let lame_off = find_lame_tag_offset(&bytes);
    let lame_tag = lame_off.and_then(|off| read_enc_delay_padding(&bytes, off));
    let (leading_silence, first_value, total) =
        measure_leading_silence(bytes.clone(), backend, false);
    let (leading_silence_gapless_on, first_value_on, _) =
        measure_leading_silence(bytes.clone(), backend, true);

    println!(
        "\n===== {backend:?}, bit_rate={bit_rate:?} =====\n\
        LAME tag at offset = {lame_off:?}, (enc_delay, enc_padding) = {lame_tag:?}\n\
        gapless=false → leading silence = {leading_silence:5} samples (decoded[{leading_silence}] = {first_value:.6})\n\
        gapless=true  → leading silence = {leading_silence_gapless_on:5} samples (decoded[{leading_silence_gapless_on}] = {first_value_on:.6})\n\
        total decoded frames in window = {total}",
    );
}

/// Second probe: patch the LAME tag's `enc_delay` field (no re-encoding)
/// and re-measure leading silence. Distinguishes "decoder reads tag and
/// trims" from "decoder ignores tag, emits raw encoder priming".
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn mp3_decoder_reads_lame_enc_delay_or_not(#[case] backend: DecoderBackend) {
    let server = TestServerHelper::new().await;
    let client = Client::new();
    let spec = SignalSpec {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        length: SignalSpecLength::Seconds(2.0),
        format: SignalFormat::Mp3,
        bit_rate: None,
    };
    let url = server.sawtooth(&spec).await;
    let response = client.get(url).send().await.expect("fetch sawtooth mp3");
    let original = response.bytes().await.expect("mp3 body").to_vec();
    let lame_off = find_lame_tag_offset(&original).expect("LAME tag must exist");
    let (orig_enc_delay, orig_padding) =
        read_enc_delay_padding(&original, lame_off).expect("read enc_delay/padding");
    let encoder_name = std::str::from_utf8(&original[lame_off..lame_off + 9])
        .unwrap_or("?")
        .to_owned();
    let raw_at_21 = format!(
        "{:02x} {:02x} {:02x}",
        original[lame_off + 21],
        original[lame_off + 22],
        original[lame_off + 23]
    );

    println!(
        "\n===== {backend:?} — LAME tag enc_delay sensitivity =====\n\
        encoder name at tag-offset = {encoder_name:?}\n\
        raw bytes at LAME+21..24 = {raw_at_21}\n\
        original LAME tag enc_delay = {orig_enc_delay}, enc_padding = {orig_padding}",
    );

    // Also probe the alternative offset (some pipelines write enc_delay at +21, others at +120 (Xing+120))
    if let Some(xing) = original
        .windows(4)
        .position(|w| w == b"Xing" || w == b"Info")
    {
        let alt_tag = xing + 120;
        if alt_tag + 24 <= original.len() {
            let alt_enc_name = std::str::from_utf8(&original[alt_tag..alt_tag + 9]).unwrap_or("?");
            let alt_raw = format!(
                "{:02x} {:02x} {:02x}",
                original[alt_tag + 21],
                original[alt_tag + 22],
                original[alt_tag + 23]
            );
            println!(
                "  alternative path: Xing/Info at offset {xing:#x}, jumping +120 → \
                encoder {alt_enc_name:?}, raw[21..24] = {alt_raw}"
            );
        }
    }

    let overrides: [u32; 5] = [orig_enc_delay, 0, 100, 1152, 2400];
    for &override_val in &overrides {
        let mut patched = original.clone();
        write_enc_delay(&mut patched, lame_off, override_val);
        let (round_trip_delay, _) = read_enc_delay_padding(&patched, lame_off).expect("read back");
        let (leading_silence, first_value, total) =
            measure_leading_silence(patched, backend, false);
        println!(
            "  enc_delay_override = {override_val:>4} (round-trip read = {round_trip_delay:>4}) → \
            leading_silence = {leading_silence:>5} samples (decoded[{leading_silence}] = {first_value:+.6}, total_window = {total})",
        );
    }
    println!(
        "  interpretation:\n\
        \tif leading_silence == override → decoder READS LAME tag and trims accordingly\n\
        \tif leading_silence stays constant (== actual encoder priming) → decoder IGNORES tag, emits raw priming"
    );
}
