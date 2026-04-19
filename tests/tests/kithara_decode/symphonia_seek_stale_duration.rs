//! Root-cause isolation for the "seek-to-near-end" hang on streaming tracks.
//!
//! Context: `StreamAudioSource::apply_seek_from_decoder` calls
//! `decoder.seek(target)`. When that call returns an error the FSM can't
//! proceed, and the symptom bubbles up as a silent hang for the user.
//!
//! Empirical finding (documented here so the next reader doesn't repeat
//! the trail): `test.mp3` ships with a Xing/Info header, so Symphonia's
//! MP3 format reader reports the *same* track duration whether it probed
//! 6 % or 100 % of the file. Stale-duration is therefore **not** what
//! makes `decoder.seek` fail in this scenario — `duration()` is fine.
//!
//! What does fail is the read that Symphonia issues *inside* `seek`: it
//! computes a target byte from the Xing TOC and then tries to read from
//! that byte. When the source only has a fraction of the file, the read
//! lands past end-of-data, Symphonia surfaces an I/O error, and the FSM
//! sees `SeekFailed`. That's the exact callsite we fall back to decoder
//! recreation for.
//!
//! The guards below lock this understanding down:
//! 1. A partial MP3 decoder still reports the full duration (Xing-driven).
//! 2. Seeking far into the track on the partial decoder errors because
//!    the target byte lies past the partially-available data.
//! 3. The same seek succeeds on a freshly-probed full decoder — proof
//!    that the fault is stream-coverage, not the target being invalid.

#![forbid(unsafe_code)]

use std::io::Cursor;

use kithara::decode::{DecoderConfig, DecoderFactory};
use kithara_platform::time::Duration;

struct Consts;
impl Consts {
    /// Fraction of the embedded MP3 handed to the "partial" decoder. Small
    /// enough to keep the target byte well beyond the slice; large enough
    /// that Symphonia's probe still succeeds.
    const PARTIAL_FRACTION_NUM: usize = 6;
    const PARTIAL_FRACTION_DEN: usize = 100;

    /// Fraction of the full duration used as the seek target. 80 % picks a
    /// position that reliably maps to a byte offset past the 6 % partial
    /// slice, yet stays well inside the true track duration.
    const TARGET_FRACTION_NUM: u32 = 80;
    const TARGET_FRACTION_DEN: u32 = 100;
}

fn track_mp3() -> &'static [u8] {
    include_bytes!("../../../assets/test.mp3")
}

fn partial_slice(full: &'static [u8]) -> &'static [u8] {
    let cut = full.len() * Consts::PARTIAL_FRACTION_NUM / Consts::PARTIAL_FRACTION_DEN;
    &full[..cut]
}

fn decoder_config() -> DecoderConfig {
    DecoderConfig::default()
}

/// Xing-headered MP3 reports the full duration even when probed against
/// a 6 % slice of the file. Duration is therefore *not* the bit that
/// goes stale; it's the byte availability that does.
#[kithara::test]
fn xing_partial_probe_reports_full_duration() {
    let full = track_mp3();
    let partial = partial_slice(full);

    let full_decoder =
        DecoderFactory::create_with_probe(Cursor::new(full), Some("mp3"), decoder_config())
            .expect("full mp3 decoder");
    let partial_decoder =
        DecoderFactory::create_with_probe(Cursor::new(partial), Some("mp3"), decoder_config())
            .expect("partial mp3 decoder");

    assert_eq!(
        partial_decoder.duration(),
        full_decoder.duration(),
        "Xing-header MP3 reports the same duration regardless of probed byte range; \
         if this ever stops being true, the partial-probe bug looks different than \
         assumed here and `apply_seek_from_decoder`'s recreation fallback may need \
         revisiting"
    );
}

/// Regression guard for the real failure mode: when the decoder's source
/// only holds part of the file, a seek whose target byte lies past the
/// available data must surface an error (not hang, not silently succeed).
/// This is the `decoder.seek` error path that the FSM hands off to
/// recreation.
#[kithara::test]
fn partial_decoder_seek_past_available_bytes_errors() {
    let full = track_mp3();
    let partial = partial_slice(full);

    let mut decoder =
        DecoderFactory::create_with_probe(Cursor::new(partial), Some("mp3"), decoder_config())
            .expect("partial mp3 decoder");
    let duration = decoder.duration().expect("duration known");

    // Target 80 % into the true track — comfortably inside the duration
    // Symphonia reports, but well past the 6 % of bytes the decoder can
    // actually read.
    let target_nanos = duration.as_nanos() * u128::from(Consts::TARGET_FRACTION_NUM)
        / u128::from(Consts::TARGET_FRACTION_DEN);
    let target = Duration::from_nanos(u64::try_from(target_nanos).expect("fits u64"));

    let result = decoder.seek(target);
    assert!(
        result.is_err(),
        "seek to {:?} (inside {:?} total duration) must fail when the source only \
         holds {:.1} % of the bytes — otherwise the FSM would never reach the \
         recreation path and the user-visible hang wouldn't exist",
        target,
        duration,
        (Consts::PARTIAL_FRACTION_NUM as f64 / Consts::PARTIAL_FRACTION_DEN as f64) * 100.0,
    );
}

/// Companion proof: the same seek target *succeeds* on a decoder built
/// from the full file. This is what justifies recreating the decoder
/// (with a fresh view of the now-complete byte range) instead of
/// bailing out of seek entirely.
#[kithara::test]
fn full_decoder_seeks_to_same_target_without_error() {
    let full = track_mp3();

    let mut decoder =
        DecoderFactory::create_with_probe(Cursor::new(full), Some("mp3"), decoder_config())
            .expect("full mp3 decoder");
    let duration = decoder.duration().expect("duration known");

    let target_nanos = duration.as_nanos() * u128::from(Consts::TARGET_FRACTION_NUM)
        / u128::from(Consts::TARGET_FRACTION_DEN);
    let target = Duration::from_nanos(u64::try_from(target_nanos).expect("fits u64"));

    decoder
        .seek(target)
        .expect("full-byte decoder must seek to the same target the partial decoder fails on");
}
