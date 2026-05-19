#![allow(unsafe_code)]

use std::{ptr, sync::Once};

#[repr(C)]
#[derive(Default)]
struct AudioStreamBasicDescription {
    m_sample_rate: f64,
    m_format_id: u32,
    m_format_flags: u32,
    m_bytes_per_packet: u32,
    m_frames_per_packet: u32,
    m_bytes_per_frame: u32,
    m_channels_per_frame: u32,
    m_bits_per_channel: u32,
    m_reserved: u32,
}

type AudioConverterRef = *mut std::ffi::c_void;

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioConverterNew(
        in_source_format: *const AudioStreamBasicDescription,
        in_destination_format: *const AudioStreamBasicDescription,
        out_audio_converter: *mut AudioConverterRef,
    ) -> i32;

    fn AudioConverterDispose(in_audio_converter: AudioConverterRef) -> i32;
}

const K_AUDIO_FORMAT_MPEG4_AAC: u32 = u32::from_be_bytes(*b"aac ");
const K_AUDIO_FORMAT_LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");
const K_AUDIO_FORMAT_FLAG_IS_FLOAT: u32 = 1 << 0;
const K_AUDIO_FORMAT_FLAG_IS_PACKED: u32 = 1 << 3;

/// Sugar over [`ensure_apple_decoder_warm`] that is a no-op for
/// non-Apple backends. Lets parameterised tests call the warm-up
/// unconditionally:
///
/// ```ignore
/// #[cfg(any(target_os = "macos", target_os = "ios"))]
/// kithara_integration_tests::apple_warmup::warm_if_apple(backend);
/// ```
pub fn warm_if_apple(backend: kithara_decode::DecoderBackend) {
    if matches!(backend, kithara_decode::DecoderBackend::Apple) {
        ensure_apple_decoder_warm();
    }
}

/// Force `CoreAudio`'s per-process `_dispatch_once` init by creating and
/// disposing a throw-away AAC `AudioConverter`. Safe to call from any
/// test; subsequent invocations are no-ops (`std::sync::Once`).
pub fn ensure_apple_decoder_warm() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let input = AudioStreamBasicDescription {
            m_sample_rate: 44_100.0,
            m_format_id: K_AUDIO_FORMAT_MPEG4_AAC,
            m_frames_per_packet: 1024,
            m_channels_per_frame: 2,
            ..Default::default()
        };
        let output = AudioStreamBasicDescription {
            m_sample_rate: 44_100.0,
            m_format_id: K_AUDIO_FORMAT_LINEAR_PCM,
            m_format_flags: K_AUDIO_FORMAT_FLAG_IS_FLOAT | K_AUDIO_FORMAT_FLAG_IS_PACKED,
            m_bytes_per_packet: 8,
            m_frames_per_packet: 1,
            m_bytes_per_frame: 8,
            m_channels_per_frame: 2,
            m_bits_per_channel: 32,
            ..Default::default()
        };
        let mut converter: AudioConverterRef = ptr::null_mut();
        // SAFETY: ASBDs are stack-local valid C-layout values; `converter`
        // is a writable out-pointer initialised to null.
        let _status = unsafe { AudioConverterNew(&input, &output, &mut converter) };
        if !converter.is_null() {
            // SAFETY: `converter` is non-null and owned here; AudioToolbox
            // documents `AudioConverterDispose` accepts any handle
            // produced by `AudioConverterNew`.
            unsafe {
                AudioConverterDispose(converter);
            }
        }
    });
}
