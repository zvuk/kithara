use bytes::Bytes;
use kithara_apple::foundation::{
    ns::{NSData, NSError, NSInteger, NSURLResponse},
    urlsession::{self, DataCompletion, ResponseParts},
};
use kithara_bufpool::{BudgetExhausted, BytePool, PooledOwned};
use kithara_platform::{sync::Mutex, tokio::sync::oneshot};

use crate::{error::NetError, types::Headers};

pub(super) const HTTP_PARTIAL_CONTENT: u16 = 206;

pub(crate) struct AppleDataResponse {
    pub(crate) body: Bytes,
    pub(crate) headers: Headers,
    pub(crate) status: Option<u16>,
}

pub(super) struct StreamHead {
    pub(super) headers: Headers,
    pub(super) status: Option<u16>,
}

struct PooledBytes {
    bytes: PooledOwned<32, Vec<u8>>,
}

impl AsRef<[u8]> for PooledBytes {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_slice()
    }
}

pub(super) fn completion_result(
    completion: DataCompletion<'_>,
    byte_pool: &BytePool,
) -> Result<AppleDataResponse, NetError> {
    if let Some(error) = completion.error() {
        return Err(error_from_nserror(error));
    }

    let body = completion
        .data()
        .map_or_else(|| Ok(Bytes::new()), |data| copy_data(data, byte_pool))?;
    let (status, headers) = completion
        .response_parts()
        .map(headers_from_response_parts)
        .unwrap_or_else(|| (None, Headers::new()));
    Ok(AppleDataResponse {
        body,
        headers,
        status,
    })
}

pub(super) fn http_parts(response: &NSURLResponse) -> Option<(Option<u16>, Headers)> {
    urlsession::response_parts(response).map(headers_from_response_parts)
}

pub(super) fn copy_data(data: &NSData, byte_pool: &BytePool) -> Result<Bytes, NetError> {
    let len = data.len();
    if len == 0 {
        return Ok(Bytes::new());
    }

    let mut bytes = byte_pool.get();
    bytes.ensure_len(len).map_err(byte_budget_error)?;
    bytes.truncate(len);
    bytes
        .as_mut_slice()
        .copy_from_slice(urlsession::data_bytes(data));
    Ok(Bytes::from_owner(PooledBytes { bytes }))
}

fn byte_budget_error(_error: BudgetExhausted) -> NetError {
    NetError::Network("byte budget exhausted".to_string())
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
    if let Some(sender) = slot.lock().take() {
        sender.send(value).ok();
    }
}

fn headers_from_response_parts(parts: ResponseParts) -> (Option<u16>, Headers) {
    let (status, pairs) = parts.into();
    let is_decoded = pairs
        .iter()
        .any(|(key, _)| key.as_str() == "content-encoding");
    let mut headers = Headers::new();
    for (key, value) in pairs {
        let key = key.to_ascii_lowercase();
        if is_decoded && (key == "content-encoding" || key == "content-length") {
            continue;
        }
        headers.insert(key, value);
    }
    (status, headers)
}
