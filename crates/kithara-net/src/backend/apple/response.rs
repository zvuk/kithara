#![allow(unsafe_code)]

use std::{
    ptr,
    sync::{Mutex, MutexGuard},
};

use bytes::Bytes;
use kithara_platform::tokio::sync::oneshot;
use objc2::{ClassType, runtime::NSObjectProtocol};
use objc2_foundation::{NSData, NSError, NSHTTPURLResponse, NSInteger, NSString, NSURLResponse};

use crate::{error::NetError, types::Headers};

pub(crate) struct AppleDataResponse {
    pub(crate) body: Bytes,
    pub(crate) headers: Headers,
    pub(crate) status: Option<u16>,
}

pub(super) struct StreamHead {
    pub(super) headers: Headers,
    pub(super) status: Option<u16>,
}

pub(super) fn completion_result(
    data: *mut NSData,
    response: *mut NSURLResponse,
    error: *mut NSError,
) -> Result<AppleDataResponse, NetError> {
    // SAFETY: NSURLSession provides these pointers for the duration of the
    // completion callback; null is represented as None.
    let error = unsafe { error.as_ref() };
    if let Some(error) = error {
        return Err(error_from_nserror(error));
    }

    // SAFETY: NSURLSession provides these pointers for the duration of the
    // completion callback; null is represented as None.
    let data = unsafe { data.as_ref() };
    // SAFETY: NSURLSession provides these pointers for the duration of the
    // completion callback; null is represented as None.
    let response = unsafe { response.as_ref() };
    let body = data.map_or_else(Bytes::new, copy_data);
    let (status, headers) = response
        .and_then(http_parts)
        .unwrap_or_else(|| (None, Headers::new()));
    Ok(AppleDataResponse {
        body,
        headers,
        status,
    })
}

pub(super) fn http_parts(response: &NSURLResponse) -> Option<(Option<u16>, Headers)> {
    let http = http_response(response)?;
    let code = http.statusCode();
    let status = u16::try_from(code).ok();
    Some((status, headers_from_response(http)))
}

pub(super) fn copy_data(data: &NSData) -> Bytes {
    Bytes::from(data.to_vec())
}

pub(super) fn error_from_nserror(error: &NSError) -> NetError {
    const URL_ERROR_CANCELLED: NSInteger = -999;
    const URL_ERROR_TIMED_OUT: NSInteger = -1001;

    if error.code() == URL_ERROR_CANCELLED {
        return NetError::Cancelled;
    }
    if error.code() == URL_ERROR_TIMED_OUT {
        return NetError::Timeout;
    }
    NetError::Network(format!(
        "{} (domain={}, code={})",
        error.localizedDescription(),
        error.domain(),
        error.code()
    ))
}

pub(super) fn send_once<T>(slot: &Mutex<Option<oneshot::Sender<T>>>, value: T) {
    if let Some(sender) = lock_or_recover(slot).take() {
        sender.send(value).ok();
    }
}

pub(super) fn store_terminal(slot: &Mutex<Option<NetError>>, error: NetError) {
    let mut terminal = lock_or_recover(slot);
    if terminal.is_none() {
        *terminal = Some(error);
    }
}

pub(super) fn take_terminal(slot: &Mutex<Option<NetError>>) -> Option<NetError> {
    lock_or_recover(slot).take()
}

pub(super) fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn http_response(response: &NSURLResponse) -> Option<&NSHTTPURLResponse> {
    if response.isKindOfClass(NSHTTPURLResponse::class()) {
        let raw = ptr::from_ref(response).cast::<NSHTTPURLResponse>();
        // SAFETY: The Objective-C object reported that it is an
        // NSHTTPURLResponse, so this reference cast preserves the object type.
        Some(unsafe { &*raw })
    } else {
        None
    }
}

fn headers_from_response(response: &NSHTTPURLResponse) -> Headers {
    let fields = response.allHeaderFields();
    // SAFETY: Foundation documents HTTP header dictionaries as NSString keys
    // and values. Non-string entries, if ever returned by a custom protocol,
    // are outside NSURLSession's HTTP response contract.
    let fields = unsafe { fields.cast_unchecked::<NSString, NSString>() };
    let (keys, values) = fields.to_vecs();
    let pairs: Vec<(String, String)> = keys
        .into_iter()
        .zip(values)
        .map(|(key, value)| (key.to_string().to_ascii_lowercase(), value.to_string()))
        .collect();
    let is_decoded = pairs
        .iter()
        .any(|(key, _)| key.as_str() == "content-encoding");
    let mut headers = Headers::new();
    for (key, value) in pairs {
        if is_decoded && (key == "content-encoding" || key == "content-length") {
            continue;
        }
        headers.insert(key, value);
    }
    headers
}
