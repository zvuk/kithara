use bytes::Bytes;
use url::Url;

use super::transport::HostMethod;
use crate::{
    backend::common::status_error,
    error::NetError,
    range_response::accepts_response_status,
    types::{AcceptEncodingPolicy, Headers, RangeSpec},
};

pub(super) const HTTP_PARTIAL_CONTENT: u16 = 206;

impl From<HostMethod> for AcceptEncodingPolicy {
    fn from(method: HostMethod) -> Self {
        match method {
            HostMethod::Get | HostMethod::Post => Self::Configured,
            HostMethod::Head => Self::Identity,
        }
    }
}

pub(super) fn request_headers(
    headers: Option<&Headers>,
    range: Option<&RangeSpec>,
    accept_encoding: AcceptEncodingPolicy,
) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = headers
        .into_iter()
        .flat_map(Headers::iter)
        .filter(|(name, _)| !name.eq_ignore_ascii_case("accept-encoding"))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    if accept_encoding == AcceptEncodingPolicy::Identity {
        pairs.push(("Accept-Encoding".to_owned(), "identity".to_owned()));
    }
    if let Some(range) = range {
        pairs.push(("Range".to_owned(), range.to_string()));
    }
    pairs
}

pub(super) fn response_headers(
    pairs: Vec<(String, String)>,
    status: u16,
    url: &Url,
) -> Result<Headers, NetError> {
    // A surviving Content-Encoding names a coding the host's client left undecoded.
    if (200..300).contains(&status)
        && let Some(value) = non_identity_content_encoding(&pairs)
    {
        return Err(NetError::Decode(format!(
            "response body for {url} retained content-encoding: {value}"
        )));
    }
    let mut headers = Headers::default();
    for (mut name, value) in pairs {
        name.make_ascii_lowercase();
        headers.insert(name, value);
    }
    Ok(headers)
}

pub(super) fn check_status(
    url: &Url,
    status: u16,
    body: &Bytes,
    accept_partial: bool,
) -> Result<u16, NetError> {
    if accepts_response_status(status, accept_partial) {
        return Ok(status);
    }
    Err(status_error(url.clone(), status, body))
}

fn non_identity_content_encoding(pairs: &[(String, String)]) -> Option<&str> {
    pairs.iter().find_map(|(name, value)| {
        (name.eq_ignore_ascii_case("content-encoding")
            && value
                .split(',')
                .map(str::trim)
                .any(|coding| !coding.eq_ignore_ascii_case("identity")))
        .then_some(value.as_str())
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::backend::common::normalize_head_headers;

    fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn headers(entries: &[(&str, &str)]) -> Headers {
        let mut headers = Headers::default();
        for (name, value) in entries {
            headers.insert(*name, *value);
        }
        headers
    }

    fn url() -> Url {
        Url::parse("http://127.0.0.1/probe").expect("BUG: hard-coded test URL is valid")
    }

    #[kithara::test(native, flash(false))]
    fn request_headers_let_the_policy_decide_accept_encoding() {
        let caller = headers(&[("AcCePt-EnCoDiNg", "br")]);

        let configured = request_headers(Some(&caller), None, AcceptEncodingPolicy::Configured);
        assert!(configured.is_empty(), "whole body sent {configured:?}");

        let identity = request_headers(Some(&caller), None, AcceptEncodingPolicy::Identity);
        assert_eq!(identity, pairs(&[("Accept-Encoding", "identity")]));
    }

    #[kithara::test(native, flash(false))]
    fn request_headers_carry_the_range_and_the_caller_headers() {
        let caller = headers(&[("X-Key", "value"), ("X-Other", "other")]);

        let sent: HashSet<(String, String)> = request_headers(
            Some(&caller),
            Some(&RangeSpec::new(4, Some(9))),
            AcceptEncodingPolicy::Identity,
        )
        .into_iter()
        .collect();

        let expected: HashSet<(String, String)> = pairs(&[
            ("X-Key", "value"),
            ("X-Other", "other"),
            ("Accept-Encoding", "identity"),
            ("Range", "bytes=4-9"),
        ])
        .into_iter()
        .collect();
        assert_eq!(sent, expected);
    }

    #[kithara::test(native, flash(false))]
    fn response_headers_are_lowercased() {
        let reported = pairs(&[("Content-Length", "12"), ("ETag", "\"a\"")]);

        let headers = response_headers(reported, 200, &url()).expect("reported headers translate");

        assert_eq!(headers.get("content-length"), Some("12"));
        assert_eq!(headers.get("etag"), Some("\"a\""));
    }

    #[kithara::test(native, flash(false))]
    fn a_surviving_content_encoding_is_rejected() {
        let reported = pairs(&[("CoNtEnT-EnCoDiNg", "identity, GZIP")]);

        let error = response_headers(reported, 200, &url())
            .expect_err("encoded bytes must not reach the caller");

        assert!(matches!(error, NetError::Decode(detail) if detail.contains("GZIP")));
    }

    #[kithara::test(native, flash(false))]
    fn an_identity_content_encoding_is_accepted() {
        let reported = pairs(&[("Content-Encoding", "IDENTITY")]);

        let headers =
            response_headers(reported, 200, &url()).expect("identity is not a coding to reject");

        assert_eq!(headers.get("content-encoding"), Some("IDENTITY"));
    }

    #[kithara::test(native, flash(false))]
    fn head_backfills_content_length_from_content_range() {
        let reported = pairs(&[("Content-Range", "bytes 0-0/1234")]);

        let headers = normalize_head_headers(
            response_headers(reported, 206, &url()).expect("reported headers translate"),
        );

        assert_eq!(headers.get("content-length"), Some("1234"));
    }

    #[kithara::test(native, flash(false))]
    fn a_client_error_carries_its_status_and_body() {
        let error = check_status(&url(), 404, &Bytes::from_static(b"gone"), false)
            .expect_err("404 is not a body");

        assert!(matches!(error, NetError::Status { status, body, .. }
                if status.get() == 404 && body.as_deref() == Some("gone")));
    }
}
