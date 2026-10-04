use std::{ffi::c_void, fs, path::Path};

use crate::{BytesEncodeRequest, BytesEncodeTarget, EncodeError, EncodeResult, EncodedBytes};

#[cfg(windows)]
type WideChar = u16;
#[cfg(not(windows))]
type WideChar = u32;

// Monkey's Audio SDK 13.27, MACLib.h: paths are native wchar_t strings.
unsafe extern "system" {
    fn CompressFileW2(
        input: *const WideChar,
        output: *const WideChar,
        level: i32,
        progress: *mut c_void,
        threads: i32,
        tag: *mut c_void,
        read_full: bool,
    ) -> i32;
}

/// Encode finite PCM with the SDK's file encoder; `FFmpeg` only supplies the WAV input.
pub(crate) fn encode(request: &BytesEncodeRequest<'_>) -> EncodeResult<EncodedBytes> {
    const NORMAL_COMPRESSION_LEVEL: i32 = 2000;

    let wav = crate::ffmpeg::bytes::encode_bytes_audio(&BytesEncodeRequest {
        pcm: request.pcm,
        target: BytesEncodeTarget::Wav16,
        bit_rate: None,
    })?;
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("input.wav");
    let output = dir.path().join("output.ape");
    fs::write(&input, wav.bytes)?;
    let input = wide_path(&input)?;
    let output_path = wide_path(&output)?;
    // SAFETY: both paths are terminated native-width Unicode strings and stay
    // alive for this synchronous call. Null pointers disable callbacks and tags.
    let status = unsafe {
        CompressFileW2(
            input.as_ptr(),
            output_path.as_ptr(),
            NORMAL_COMPRESSION_LEVEL,
            std::ptr::null_mut(),
            1,
            std::ptr::null_mut(),
            false,
        )
    };
    if status != 0 {
        return Err(EncodeError::backend_message(format!(
            "Monkey's Audio encoding failed: {status}"
        )));
    }
    Ok(EncodedBytes {
        bytes: fs::read(output)?,
        content_type: "audio/ape",
        media_info: request.media_info(),
    })
}

#[cfg(windows)]
fn wide_path(path: &Path) -> EncodeResult<Vec<WideChar>> {
    use std::os::windows::ffi::OsStrExt;
    let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
    if value.contains(&0) {
        return Err(EncodeError::InvalidInput("path contains NUL".to_owned()));
    }
    value.push(0);
    Ok(value)
}

#[cfg(not(windows))]
fn wide_path(path: &Path) -> EncodeResult<Vec<WideChar>> {
    let text = path
        .to_str()
        .ok_or_else(|| EncodeError::InvalidInput("path is not Unicode".to_owned()))?;
    if text.contains('\0') {
        return Err(EncodeError::InvalidInput("path contains NUL".to_owned()));
    }
    Ok(text
        .chars()
        .map(u32::from)
        .chain(std::iter::once(0))
        .collect())
}
