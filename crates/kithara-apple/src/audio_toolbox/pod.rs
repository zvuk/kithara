use std::{mem::size_of, ptr};

use super::sys::{
    AudioConverterPrimeInfo, AudioFormatListItem, AudioStreamBasicDescription,
    AudioStreamPacketDescription,
};

pub trait ApplePod: Copy + Default {}

impl ApplePod for AudioStreamPacketDescription {}
impl ApplePod for AudioStreamBasicDescription {}
impl ApplePod for AudioConverterPrimeInfo {}
impl ApplePod for AudioFormatListItem {}
impl ApplePod for super::sys::AudioFormatInfo {}
impl ApplePod for u32 {}
impl ApplePod for u64 {}

#[must_use]
pub fn pod_from_prefix<T>(bytes: &[u8]) -> Option<T>
where
    T: ApplePod,
{
    if bytes.len() < size_of::<T>() {
        return None;
    }
    let mut value = T::default();
    // SAFETY: value is initialized POD storage.
    // SAFETY: bytes has at least the number of bytes copied into it.
    unsafe {
        ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            ptr::from_mut(&mut value).cast::<u8>(),
            size_of::<T>(),
        );
    }
    Some(value)
}

#[must_use]
pub fn pod_to_vec<T>(value: &T) -> Vec<u8>
where
    T: ApplePod,
{
    let mut out = vec![0_u8; size_of::<T>()];
    debug_assert!(pod_write_to_slice(value, &mut out));
    out
}

#[must_use]
pub fn pod_write_to_slice<T>(value: &T, out: &mut [u8]) -> bool
where
    T: ApplePod,
{
    if out.len() < size_of::<T>() {
        return false;
    }
    // SAFETY: value is POD and out has at least size_of::<T>() bytes.
    unsafe {
        ptr::copy_nonoverlapping(
            ptr::from_ref(value).cast::<u8>(),
            out.as_mut_ptr(),
            size_of::<T>(),
        );
    }
    true
}
