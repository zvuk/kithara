use std::{fmt::Write, num::NonZeroU16};

use bytes::Bytes;
use url::Url;

use crate::{error::NetError, types::Headers};

pub(crate) fn status_error(url: Url, status: u16, body: &Bytes) -> NetError {
    let body = if body.is_empty() {
        None
    } else {
        Some(truncate_error_body(
            String::from_utf8_lossy(body).into_owned(),
        ))
    };
    match NonZeroU16::new(status) {
        Some(status) => NetError::Status {
            status,
            body,
            url: Some(url),
        },
        None => NetError::Network(format!("unexpected zero HTTP status for {url}")),
    }
}

// A 206 to a probe states the representation total in content-range alone.
pub(crate) fn normalize_head_headers(mut headers: Headers) -> Headers {
    if headers.get("content-length").is_none()
        && let Some(total) = content_length_from_range(&headers)
    {
        headers.insert("content-length", total);
    }
    headers
}

fn content_length_from_range(headers: &Headers) -> Option<String> {
    headers
        .get("content-range")
        .and_then(|header| header.split('/').nth(1))
        .filter(|total| *total != "*")
        .map(str::to_owned)
}

fn truncate_error_body(mut body: String) -> String {
    const MAX_ERROR_BODY_CHARS: usize = 200;

    let total = body.chars().count();
    if total <= MAX_ERROR_BODY_CHARS {
        return body;
    }
    let cut_at = body
        .char_indices()
        .nth(MAX_ERROR_BODY_CHARS)
        .map_or(body.len(), |(index, _)| index);
    body.truncate(cut_at);
    let _ = write!(body, "...(truncated, {total} chars total)");
    body
}
