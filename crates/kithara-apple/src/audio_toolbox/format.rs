use std::{ffi::c_void, mem::size_of, ptr};

use super::{
    pod::ApplePod,
    sys::{
        AudioFormatPropertyID, OSStatus, UInt32, audio_format_get_property_info_raw,
        audio_format_get_property_raw,
    },
};

/// Reads the byte size for an `AudioFormat` property.
///
/// # Errors
///
/// Returns the `AudioToolbox` status when the property info cannot be read.
pub fn audio_format_get_property_info<T>(
    property: AudioFormatPropertyID,
    specifier: &T,
) -> Result<UInt32, OSStatus>
where
    T: ApplePod,
{
    let mut size = 0;
    let specifier_size = UInt32::try_from(size_of::<T>()).map_err(|_| super::sys::PARAM_ERR)?;
    // SAFETY: specifier points at a live POD value and size is a writable out-param.
    let status = unsafe {
        audio_format_get_property_info_raw(
            property,
            specifier_size,
            ptr::from_ref(specifier).cast::<c_void>(),
            &mut size,
        )
    };
    if status == super::sys::NO_ERR {
        Ok(size)
    } else {
        Err(status)
    }
}

/// Reads an `AudioFormat` property into `output`.
///
/// # Errors
///
/// Returns the `AudioToolbox` status when the property cannot be read.
pub fn audio_format_get_property<T, U>(
    property: AudioFormatPropertyID,
    specifier: &T,
    output: &mut [U],
    output_bytes: UInt32,
) -> Result<UInt32, OSStatus>
where
    T: ApplePod,
    U: ApplePod,
{
    let specifier_size = UInt32::try_from(size_of::<T>()).map_err(|_| super::sys::PARAM_ERR)?;
    let mut io_size = output_bytes;
    // SAFETY: specifier points at a live POD value and output is writable.
    // SAFETY: io_size came from the matching audio_format_get_property_info_raw call.
    let status = unsafe {
        audio_format_get_property_raw(
            property,
            specifier_size,
            ptr::from_ref(specifier).cast::<c_void>(),
            &mut io_size,
            output.as_mut_ptr().cast::<c_void>(),
        )
    };
    if status == super::sys::NO_ERR {
        Ok(io_size)
    } else {
        Err(status)
    }
}
