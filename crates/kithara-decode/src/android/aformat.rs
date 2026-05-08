#![allow(unsafe_code)]

use std::{ffi::CStr, ptr::NonNull};

use super::{error::AndroidBackendError, ffi};

pub(crate) struct OwnedFormat {
    raw: NonNull<ffi::AMediaFormat>,
}

impl OwnedFormat {
    pub(crate) fn from_raw(raw: NonNull<ffi::AMediaFormat>) -> Self {
        Self { raw }
    }

    pub(crate) fn get_i32(&self, key: &CStr) -> Option<i32> {
        let mut value = 0;
        unsafe { ffi::AMediaFormat_getInt32(self.raw(), key.as_ptr(), &mut value) }.then_some(value)
    }

    pub(crate) fn get_i64(&self, key: &CStr) -> Result<Option<i64>, AndroidBackendError> {
        let mut value = 0;
        Ok(
            unsafe { ffi::AMediaFormat_getInt64(self.raw(), key.as_ptr(), &mut value) }
                .then_some(value),
        )
    }

    pub(crate) fn get_string(&self, key: &CStr) -> Result<Option<String>, AndroidBackendError> {
        let mut value = std::ptr::null();
        let found = unsafe { ffi::AMediaFormat_getString(self.raw(), key.as_ptr(), &mut value) };
        if !found {
            return Ok(None);
        }
        let value = NonNull::new(value.cast_mut()).ok_or_else(|| {
            AndroidBackendError::operation("media-format-string", "string pointer was null")
        })?;
        Ok(Some(
            unsafe { CStr::from_ptr(value.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
        ))
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

    pub(crate) fn set_i32(&mut self, key: &CStr, value: i32) -> bool {
        unsafe { ffi::AMediaFormat_setInt32(self.raw(), key.as_ptr(), value) }
    }
}

impl Drop for OwnedFormat {
    fn drop(&mut self) {
        unsafe { ffi::AMediaFormat_delete(self.raw()) };
    }
}
