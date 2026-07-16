use bytes::Bytes;
use kithara_apple::foundation::{
    ns::{NSData, NSError, NSInteger, NSURLResponse},
    urlsession::{self, DataCompletion, ResponseParts},
};
use kithara_bufpool::{BudgetExhausted, BytePool, PooledOwned};
use kithara_platform::{sync::Mutex, tokio::sync::oneshot};
use url::Url;

use crate::{
    error::NetError,
    types::{AcceptEncodingPolicy, Headers},
};

pub(super) const HTTP_PARTIAL_CONTENT: u16 = 206;
pub(super) type HttpResponseParts = (Option<u16>, Headers);

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
    completion: &DataCompletion<'_>,
    byte_pool: &BytePool,
    accept_encoding: AcceptEncodingPolicy,
    url: &Url,
) -> Result<AppleDataResponse, NetError> {
    if let Some(error) = completion.error() {
        return Err(error_from_nserror(error));
    }

    let (status, headers) = match completion.response_parts() {
        Some(parts) => headers_from_response_parts(parts, accept_encoding, url)?,
        None => (None, Headers::new()),
    };
    let body = completion
        .data()
        .map_or_else(|| Ok(Bytes::new()), |data| copy_data(data, byte_pool))?;
    Ok(AppleDataResponse {
        body,
        headers,
        status,
    })
}

pub(super) fn http_parts(
    response: &NSURLResponse,
    accept_encoding: AcceptEncodingPolicy,
    url: &Url,
) -> Result<Option<HttpResponseParts>, NetError> {
    urlsession::response_parts(response)
        .map(|parts| headers_from_response_parts(parts, accept_encoding, url))
        .transpose()
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

fn headers_from_response_parts(
    parts: ResponseParts,
    accept_encoding: AcceptEncodingPolicy,
    url: &Url,
) -> Result<(Option<u16>, Headers), NetError> {
    let (status, pairs) = parts.into();
    if accept_encoding == AcceptEncodingPolicy::Identity
        && status.is_some_and(|status| (200..300).contains(&status))
        && let Some(value) = non_identity_content_encoding(&pairs)
    {
        return Err(NetError::Decode(format!(
            "response body for {url} retained content-encoding under identity policy: {value}"
        )));
    }
    let is_decoded =
        accept_encoding == AcceptEncodingPolicy::Configured && has_content_encoding(&pairs);
    let mut headers = Headers::new();
    for (key, value) in pairs {
        let key = key.to_ascii_lowercase();
        if is_decoded && (key == "content-encoding" || key == "content-length") {
            continue;
        }
        headers.insert(key, value);
    }
    Ok((status, headers))
}

fn has_content_encoding(pairs: &[(String, String)]) -> bool {
    pairs
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case("content-encoding"))
}

fn non_identity_content_encoding(pairs: &[(String, String)]) -> Option<&str> {
    pairs.iter().find_map(|(key, value)| {
        (key.eq_ignore_ascii_case("content-encoding")
            && value
                .split(',')
                .map(str::trim)
                .any(|coding| !coding.eq_ignore_ascii_case("identity")))
        .then_some(value.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::{has_content_encoding, non_identity_content_encoding};

    #[test]
    fn content_encoding_detection_is_ascii_case_insensitive() {
        let pairs = [("CoNtEnT-EnCoDiNg".to_string(), "identity, GZIP".to_string())];

        assert!(has_content_encoding(&pairs));
        assert_eq!(
            non_identity_content_encoding(&pairs),
            Some("identity, GZIP")
        );
    }

    #[test]
    fn identity_content_encoding_is_not_rejected() {
        let pairs = [("Content-Encoding".to_string(), "IDENTITY".to_string())];

        assert!(has_content_encoding(&pairs));
        assert_eq!(non_identity_content_encoding(&pairs), None);
    }
}
