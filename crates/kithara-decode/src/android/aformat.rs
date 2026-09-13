#![allow(unsafe_code)]

use std::{ffi::CStr, ptr::NonNull};

use derive_more::From;

use super::{error::AndroidBackendError, ffi};

#[derive(From)]
pub(crate) struct OwnedFormat {
    raw: NonNull<ffi::AMediaFormat>,
}

impl OwnedFormat {
    pub(crate) fn get_str(&self, key: &CStr) -> Option<&CStr> {
        let mut value = std::ptr::null();
        // SAFETY: the format and key are live; value is a writable out-parameter.
        let found = unsafe { ffi::AMediaFormat_getString(self.raw(), key.as_ptr(), &mut value) };
        if !found || value.is_null() {
            return None;
        }
        // SAFETY: the successful query returns a NUL-terminated string owned by this format.
        Some(unsafe { CStr::from_ptr(value) })
    }

    pub(crate) fn get_i32(&self, key: &CStr) -> Option<i32> {
        let mut value = 0;
        // SAFETY: `raw` is live; `key` is NUL-terminated and `value` is an out-param.
        unsafe { ffi::AMediaFormat_getInt32(self.raw(), key.as_ptr(), &mut value) }.then_some(value)
    }

    pub(crate) fn get_u16(&self, key: &CStr) -> Result<Option<u16>, AndroidBackendError> {
        self.get_uint(key, "media-format-u16")
    }

    pub(crate) fn get_u32(&self, key: &CStr) -> Result<Option<u32>, AndroidBackendError> {
        self.get_uint(key, "media-format-u32")
    }

    fn get_uint<T>(&self, key: &CStr, op: &'static str) -> Result<Option<T>, AndroidBackendError>
    where
        T: TryFrom<i32>,
    {
        self.get_i32(key)
            .map(|value| {
                T::try_from(value).map_err(|_| {
                    AndroidBackendError::operation(
                        op,
                        format!("{}={value} is out of range", key.to_string_lossy()),
                    )
                })
            })
            .transpose()
    }

    pub(crate) fn raw(&self) -> *mut ffi::AMediaFormat {
        self.raw.as_ptr()
    }

    pub(crate) fn set_i32(&mut self, key: &CStr, value: i32) {
        // SAFETY: `raw` is live and exclusively borrowed; `key` is NUL-terminated.
        unsafe { ffi::AMediaFormat_setInt32(self.raw(), key.as_ptr(), value) };
    }
}

impl Drop for OwnedFormat {
    fn drop(&mut self) {
        // SAFETY: `raw` is live and freed exactly once, here.
        unsafe { ffi::AMediaFormat_delete(self.raw()) };
    }
}
