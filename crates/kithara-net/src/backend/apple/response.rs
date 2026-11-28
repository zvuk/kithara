use bytes::Bytes;
use kithara_apple::foundation::{
    ns::{NSData, NSError, NSInteger, NSURLResponse},
    urlsession::{self, ResponseParts},
};

use crate::{
    backend::pooled::{ByteBuffers, pooled_bytes},
    error::NetError,
    types::{AcceptEncodingPolicy, Headers},
};

pub(in crate::backend::apple) mod consts {
    pub(in crate::backend::apple) const HTTP_PARTIAL_CONTENT: u16 = 206;
}

pub(super) type HttpResponseParts = (Option<u16>, Headers);

pub(super) struct StreamHead {
    pub(super) headers: Headers,
    pub(super) status: Option<u16>,
}

pub(super) fn http_parts(
    response: &NSURLResponse,
    accept_encoding: AcceptEncodingPolicy,
) -> Option<HttpResponseParts> {
    urlsession::response_parts(response)
        .map(|parts| headers_from_response_parts(parts, accept_encoding))
}

pub(super) fn copy_data(data: &NSData, buffers: &ByteBuffers) -> Result<Bytes, NetError> {
    let len = data.len();
    if len == 0 {
        return Ok(Bytes::new());
    }

    let mut bytes = buffers
        .get_with_len(len)
        .map_err(|error| NetError::Network(error.to_string()))?;
    bytes.copy_from_slice(urlsession::data_bytes(data));
    Ok(pooled_bytes(bytes))
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

fn headers_from_response_parts(
    parts: ResponseParts,
    accept_encoding: AcceptEncodingPolicy,
) -> HttpResponseParts {
    let (status, pairs) = parts.into();
    let is_decoded =
        accept_encoding == AcceptEncodingPolicy::Configured && has_content_encoding(&pairs);
    let mut headers = Headers::default();
    for (key, value) in pairs {
        if is_decoded
            && (key.eq_ignore_ascii_case("content-encoding")
                || key.eq_ignore_ascii_case("content-length"))
        {
            continue;
        }
        headers.insert(key, value);
    }
    (status, headers)
}

fn has_content_encoding(pairs: &[(String, String)]) -> bool {
    pairs
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case("content-encoding"))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::has_content_encoding;

    #[kithara::test(platform = apple, native, flash(false))]
    fn content_encoding_detection_is_ascii_case_insensitive() {
        let pairs = [("CoNtEnT-EnCoDiNg".to_string(), "identity, GZIP".to_string())];

        assert!(has_content_encoding(&pairs));
    }
}
