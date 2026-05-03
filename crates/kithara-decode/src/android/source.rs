#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    io::{Read, Seek, SeekFrom},
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use tracing::warn;

use super::{error::AndroidBackendError, ffi};
use crate::{GaplessInfo, gapless::probe_mp4_gapless_dyn, hardware::BoxedSource};

pub(crate) struct SourceState {
    source: Mutex<BoxedSource>,
    byte_len_handle: Arc<AtomicU64>,
    closed: AtomicBool,
    last_error: Mutex<Option<String>>,
}

pub(crate) struct OwnedMediaDataSource {
    raw: Option<NonNull<ffi::AMediaDataSource>>,
    state: Option<NonNull<SourceState>>,
}

unsafe impl Send for OwnedMediaDataSource {}

impl OwnedMediaDataSource {
    pub(crate) fn new(
        source: BoxedSource,
        byte_len_handle: Arc<AtomicU64>,
    ) -> Result<Self, (BoxedSource, AndroidBackendError)> {
        let raw = match NonNull::new(unsafe { ffi::AMediaDataSource_new() }) {
            Some(raw) => raw,
            None => {
                return Err((
                    source,
                    AndroidBackendError::operation(
                        "media-data-source-new",
                        "AMediaDataSource_new returned null",
                    ),
                ));
            }
        };

        let state = Box::new(SourceState {
            source: Mutex::new(source),
            byte_len_handle,
            closed: AtomicBool::new(false),
            last_error: Mutex::new(None),
        });
        let state_ptr = NonNull::from(Box::leak(state));

        unsafe {
            ffi::AMediaDataSource_setUserdata(raw.as_ptr(), state_ptr.as_ptr().cast::<c_void>());
            ffi::AMediaDataSource_setReadAt(raw.as_ptr(), Some(read_at_callback));
            ffi::AMediaDataSource_setGetSize(raw.as_ptr(), Some(get_size_callback));
            ffi::AMediaDataSource_setClose(raw.as_ptr(), Some(close_callback));
        }
        Ok(Self {
            raw: Some(raw),
            state: Some(state_ptr),
        })
    }

    pub(crate) fn raw(&self) -> *mut ffi::AMediaDataSource {
        self.raw.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }

    pub(crate) fn probe_mp4_gapless(&self) -> Result<Option<GaplessInfo>, AndroidBackendError> {
        let state = self.state.ok_or_else(|| {
            AndroidBackendError::operation(
                "media-data-source-gapless-probe",
                "media source state already taken",
            )
        })?;
        let state = unsafe { state.as_ref() };
        let mut source = state.source.lock().map_err(|_| {
            AndroidBackendError::operation(
                "media-data-source-gapless-probe",
                "source mutex poisoned",
            )
        })?;
        let current = source.seek(SeekFrom::Current(0)).map_err(|error| {
            AndroidBackendError::operation(
                "media-data-source-gapless-probe-current",
                error.to_string(),
            )
        })?;
        source.seek(SeekFrom::Start(0)).map_err(|error| {
            AndroidBackendError::operation(
                "media-data-source-gapless-probe-seek-start",
                error.to_string(),
            )
        })?;

        let probe = probe_mp4_gapless_dyn(&mut **source).map_err(|error| {
            AndroidBackendError::operation("media-data-source-gapless-probe", error.to_string())
        });
        let restore = source.seek(SeekFrom::Start(current)).map_err(|error| {
            AndroidBackendError::operation(
                "media-data-source-gapless-probe-restore",
                error.to_string(),
            )
        });

        match (probe, restore) {
            (Ok(gapless), Ok(_)) => Ok(gapless),
            (Err(error), Ok(_)) => Err(error),
            (_, Err(error)) => Err(error),
        }
    }

    pub(crate) fn into_source(mut self) -> Result<BoxedSource, AndroidBackendError> {
        if let Some(raw) = self.raw.take() {
            unsafe { ffi::AMediaDataSource_delete(raw.as_ptr()) };
        }

        let state = self
            .state
            .take()
            .map(|state| unsafe { Box::from_raw(state.as_ptr()) })
            .ok_or_else(|| {
                AndroidBackendError::operation("recover-source", "media source state already taken")
            })?;
        let state = *state;
        state
            .source
            .into_inner()
            .map_err(|_| AndroidBackendError::operation("recover-source", "source mutex poisoned"))
    }
}

impl Drop for OwnedMediaDataSource {
    fn drop(&mut self) {
        if let Some(raw) = self.raw.take() {
            unsafe { ffi::AMediaDataSource_delete(raw.as_ptr()) };
        }
        if let Some(state) = self.state.take() {
            drop(unsafe { Box::from_raw(state.as_ptr()) });
        }
    }
}

unsafe extern "C" fn read_at_callback(
    userdata: *mut c_void,
    position: ffi::Off64,
    buffer: *mut c_void,
    size: usize,
) -> ffi::SSize {
    let Some(state) = state_from_userdata(userdata) else {
        return -1;
    };
    if state.closed.load(Ordering::Acquire) {
        return -1;
    }
    if size == 0 {
        return 0;
    }
    if position < 0 {
        record_error(state, "read-at", "negative position");
        return -1;
    }

    let mut source = match state.source.lock() {
        Ok(source) => source,
        Err(_) => {
            record_error(state, "read-at", "source mutex poisoned");
            return -1;
        }
    };

    if let Err(error) = source.seek(SeekFrom::Start(position as u64)) {
        record_error(state, "read-at-seek", error.to_string());
        return -1;
    }

    let output = unsafe { std::slice::from_raw_parts_mut(buffer.cast::<u8>(), size) };
    match source.read(output) {
        Ok(0) => -1,
        Ok(bytes) => bytes as ffi::SSize,
        Err(error) => {
            record_error(state, "read-at-read", error.to_string());
            -1
        }
    }
}

unsafe extern "C" fn get_size_callback(userdata: *mut c_void) -> ffi::Off64 {
    let Some(state) = state_from_userdata(userdata) else {
        return -1;
    };

    let known = state.byte_len_handle.load(Ordering::Acquire);
    if known > 0 {
        return known as ffi::Off64;
    }

    let mut source = match state.source.lock() {
        Ok(source) => source,
        Err(_) => {
            record_error(state, "get-size", "source mutex poisoned");
            return -1;
        }
    };

    let current = match source.seek(SeekFrom::Current(0)) {
        Ok(offset) => offset,
        Err(error) => {
            record_error(state, "get-size-current", error.to_string());
            return -1;
        }
    };

    let size = match source.seek(SeekFrom::End(0)) {
        Ok(size) => size,
        Err(error) => {
            record_error(state, "get-size-end", error.to_string());
            let _ = source.seek(SeekFrom::Start(current));
            return -1;
        }
    };

    if let Err(error) = source.seek(SeekFrom::Start(current)) {
        record_error(state, "get-size-restore", error.to_string());
        return -1;
    }

    state.byte_len_handle.store(size, Ordering::Release);
    size as ffi::Off64
}

unsafe extern "C" fn close_callback(userdata: *mut c_void) {
    if let Some(state) = state_from_userdata(userdata) {
        state.closed.store(true, Ordering::Release);
    }
}

fn state_from_userdata(userdata: *mut c_void) -> Option<&'static SourceState> {
    NonNull::new(userdata.cast::<SourceState>()).map(|ptr| unsafe { ptr.as_ref() })
}

fn record_error(state: &SourceState, operation: &'static str, details: impl Into<String>) {
    let details = details.into();
    warn!(
        operation,
        details, "Android media data source callback failed"
    );
    if let Ok(mut slot) = state.last_error.lock() {
        *slot = Some(format!("{operation}: {details}"));
    }
}
