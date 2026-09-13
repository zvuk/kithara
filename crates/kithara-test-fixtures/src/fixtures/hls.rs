use std::{fs, io, path::Path, str::from_utf8, sync::OnceLock};

use kithara_test_macros as kithara;

use crate::{
    assets,
    fmp4::Fmp4Package,
    signal::Wave,
    variant_input::{VariantCatalog, VariantInput},
};

/// Finite stereo saw WAV header and PCM for 6 200 KB segments.
#[kithara::fixture]
#[must_use]
pub fn hls_saw_6() -> (Vec<u8>, Vec<u8>) {
    let (header, pcm) = assets::hls_saw_six().bytes().split_at(44);
    (header.to_vec(), pcm.to_vec())
}

/// Finite stereo saw WAV header and PCM for 8 200 KB segments.
#[kithara::fixture]
#[must_use]
pub fn hls_saw_8() -> (Vec<u8>, Vec<u8>) {
    let (header, pcm) = assets::hls_saw_eight().bytes().split_at(44);
    (header.to_vec(), pcm.to_vec())
}

/// Finite stereo saw WAV header and PCM for 15 200 KB segments.
#[kithara::fixture]
#[must_use]
pub fn hls_saw_15() -> (Vec<u8>, Vec<u8>) {
    let (header, pcm) = assets::hls_saw_fifteen().bytes().split_at(44);
    (header.to_vec(), pcm.to_vec())
}

/// Finite stereo saw WAV header and PCM for 20 200 KB segments.
#[kithara::fixture]
#[must_use]
pub fn hls_saw_20() -> (Vec<u8>, Vec<u8>) {
    let (header, pcm) = assets::hls_saw_twenty().bytes().split_at(44);
    (header.to_vec(), pcm.to_vec())
}

/// Finite stereo saw WAV header and PCM for 30 200 KB segments.
#[kithara::fixture]
#[must_use]
pub fn hls_saw_30() -> (Vec<u8>, Vec<u8>) {
    let (header, pcm) = assets::hls_saw_thirty().bytes().split_at(44);
    (header.to_vec(), pcm.to_vec())
}

/// Streaming stereo WAV header prepared at build time.
#[kithara::fixture]
#[must_use]
pub fn hls_stream_header() -> Vec<u8> {
    assets::hls_stream_header_stereo().bytes().to_vec()
}

/// Sized stereo WAV header for the corresponding HLS PCM fixture.
#[kithara::fixture]
#[must_use]
pub fn hls_header_boundary() -> Vec<u8> {
    assets::hls_finite_header_boundary().bytes().to_vec()
}

/// Sized stereo WAV header for the corresponding HLS PCM fixture.
#[kithara::fixture]
#[must_use]
pub fn hls_header_thirty() -> Vec<u8> {
    assets::hls_finite_header_thirty().bytes().to_vec()
}

/// Sized stereo WAV header for the corresponding HLS PCM fixture.
#[kithara::fixture]
#[must_use]
pub fn hls_header_forty() -> Vec<u8> {
    assets::hls_finite_header_forty().bytes().to_vec()
}

/// Sized stereo WAV header for the corresponding HLS PCM fixture.
#[kithara::fixture]
#[must_use]
pub fn hls_header_fifty() -> Vec<u8> {
    assets::hls_finite_header_fifty().bytes().to_vec()
}

/// Prepared stereo PCM input for HLS segment tests.
#[kithara::fixture]
#[must_use]
pub fn hls_pcm_boundary() -> Vec<u8> {
    assets::hls_pcm_boundary().bytes().to_vec()
}

/// Prepared stereo PCM input for HLS segment tests.
#[kithara::fixture]
#[must_use]
pub fn hls_pcm_thirty() -> Vec<u8> {
    assets::hls_pcm_thirty().bytes().to_vec()
}

/// Prepared stereo PCM input for HLS segment tests.
#[kithara::fixture]
#[must_use]
pub fn hls_pcm_forty() -> Vec<u8> {
    assets::hls_pcm_forty().bytes().to_vec()
}

/// Prepared stereo PCM input for HLS segment tests.
#[kithara::fixture]
#[must_use]
pub fn hls_pcm_forty_descending() -> Vec<u8> {
    assets::hls_pcm_forty_descending().bytes().to_vec()
}

/// Prepared stereo PCM input for HLS segment tests.
#[kithara::fixture]
#[must_use]
pub fn hls_pcm_forty_shifted() -> Vec<u8> {
    assets::hls_pcm_forty_shifted().bytes().to_vec()
}

/// Prepared stereo PCM input for HLS segment tests.
#[kithara::fixture]
#[must_use]
pub fn hls_pcm_fifty() -> Vec<u8> {
    assets::hls_pcm_fifty().bytes().to_vec()
}

/// Prepared stereo PCM input for HLS segment tests.
#[kithara::fixture]
#[must_use]
pub fn hls_pcm_fifty_descending() -> Vec<u8> {
    assets::hls_pcm_fifty_descending().bytes().to_vec()
}

/// Prepared WAV including its header in the segment byte budget.
#[kithara::fixture]
#[must_use]
pub fn hls_sized_wav_three() -> Vec<u8> {
    assets::hls_sized_wav_three().bytes().to_vec()
}

/// Prepared WAV including its header in the segment byte budget.
#[kithara::fixture]
#[must_use]
pub fn hls_sized_wav_forty_eight() -> Vec<u8> {
    assets::hls_sized_wav_forty_eight().bytes().to_vec()
}

/// Prepared WAV including its header in the segment byte budget.
#[kithara::fixture]
#[must_use]
pub fn hls_sized_wav_hundred() -> Vec<u8> {
    assets::hls_sized_wav_hundred().bytes().to_vec()
}

/// Read the exact prepared HLS variant; unregistered inputs never trigger encoding.
///
/// # Errors
///
/// Returns `NotFound` for an unregistered input, or an I/O error when a prepared
/// init or media segment cannot be read.
///
/// # Panics
///
/// Panics if the generated catalog is malformed, has no parent directory, or
/// the input uses a codec unsupported by [`VariantInput::key`].
pub fn load_variant(input: &VariantInput) -> io::Result<Fmp4Package> {
    let catalog = variant_catalog();
    let asset = assets::hls_variants_catalog_with_native_gapless();
    let key = input.key();
    let artifact = catalog.variants.get(&key).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("unregistered HLS fixture: {key}"),
        )
    })?;
    let relative_root = Path::new(asset.entry().path)
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HLS catalog has no namespace"))?;
    Ok(Fmp4Package {
        init_segment: fs::read(crate::store::file(&relative_root.join(&artifact.init))?)?,
        media_segments: artifact
            .media
            .iter()
            .map(|path| fs::read(crate::store::file(&relative_root.join(path))?))
            .collect::<io::Result<_>>()?,
        segment_durations_secs: artifact.durations.clone(),
    })
}

/// Read a registered raw HLS WAV body.
///
/// # Errors
///
/// Returns `NotFound` for an unregistered WAV shape.
pub fn load_wav(sample_rate: u32, channels: u16, frames: usize) -> io::Result<Vec<u8>> {
    let asset = match (sample_rate, channels, frames) {
        (44_100, 2, 300_000) => assets::hls_saw_six(),
        (44_100, 2, 2_400_000) => assets::hls_raw_wav_web(),
        (44_100, 2, 2_160_000) => assets::hls_raw_wav_web_jitter(),
        shape => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("unregistered raw HLS WAV: {shape:?}"),
            ));
        }
    };
    Ok(asset.bytes().to_vec())
}

/// Read a registered streaming WAV header.
///
/// # Errors
///
/// Returns `NotFound` for an unregistered sample rate or channel count.
pub fn load_header(sample_rate: u32, channels: u16) -> io::Result<Vec<u8>> {
    if (sample_rate, channels) != (44_100, 2) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("unregistered HLS WAV header: {sample_rate}/{channels}"),
        ));
    }
    Ok(hls_stream_header())
}

/// Read a registered raw PCM body without synthesizing samples.
///
/// # Errors
///
/// Returns `NotFound` for an unregistered PCM shape or waveform.
pub fn load_pcm(sample_rate: u32, channels: u16, frames: usize, wave: Wave) -> io::Result<Vec<u8>> {
    let asset = match (sample_rate, channels, frames, wave) {
        (44_100, 2, 65_536, Wave::Sawtooth) => assets::hls_pcm_boundary(),
        (44_100, 2, 1_500_000, Wave::Sawtooth) => assets::hls_pcm_thirty(),
        (44_100, 2, 2_000_000, Wave::Sawtooth) => assets::hls_pcm_forty(),
        (44_100, 2, 2_000_000, Wave::SawtoothDescending) => assets::hls_pcm_forty_descending(),
        (44_100, 2, 2_000_000, Wave::SawtoothShifted) => assets::hls_pcm_forty_shifted(),
        (44_100, 2, 2_500_000, Wave::Sawtooth) => assets::hls_pcm_fifty(),
        (44_100, 2, 2_500_000, Wave::SawtoothDescending) => assets::hls_pcm_fifty_descending(),
        shape => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("unregistered raw HLS PCM: {shape:?}"),
            ));
        }
    };
    Ok(asset.bytes().to_vec())
}

fn variant_catalog() -> &'static VariantCatalog {
    static CATALOG: OnceLock<VariantCatalog> = OnceLock::new();
    let asset = assets::hls_variants_catalog_with_native_gapless();
    CATALOG.get_or_init(|| {
        toml::from_str(from_utf8(asset.bytes()).expect("UTF-8 HLS catalog"))
            .expect("valid build-time HLS catalog")
    })
}

/// Frame size recorded by the host encoder that produced the HLS fixtures.
///
/// # Errors
///
/// Returns `NotFound` when the catalog contains no fixture for this codec.
///
/// # Panics
///
/// Panics if the generated catalog is malformed.
pub fn frame_samples(codec: kithara_stream::AudioCodec) -> io::Result<usize> {
    variant_catalog()
        .frame_samples
        .get(&format!("{codec:?}"))
        .copied()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no prepared HLS frame size for {codec:?}"),
            )
        })
}
