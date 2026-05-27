use axum::{
    body::Body,
    http::{HeaderMap, Response, StatusCode, header},
};

pub(crate) fn parse_range_header(headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let value = headers.get(header::RANGE)?.to_str().ok()?.trim();
    let range = value.strip_prefix("bytes=")?;
    let mut parts = range.splitn(2, '-');
    let start = parts.next()?.trim().parse::<u64>().ok()?;
    let end = match parts.next()?.trim() {
        "" => None,
        value => Some(value.parse::<u64>().ok()?.saturating_add(1)),
    };
    Some((start, end))
}

pub(crate) fn build_range_response(
    data: &[u8],
    headers: &HeaderMap,
    include_body: bool,
    accept_ranges: bool,
    content_type: Option<&'static str>,
) -> Response<Body> {
    build_range_response_with_len(
        data,
        headers,
        include_body,
        accept_ranges,
        None,
        content_type,
    )
}

pub(crate) fn build_range_response_with_len(
    data: &[u8],
    headers: &HeaderMap,
    include_body: bool,
    accept_ranges: bool,
    content_length_override: Option<usize>,
    content_type: Option<&'static str>,
) -> Response<Body> {
    let total = data.len();
    let range = parse_range_header(headers);
    if let Some((start, end_opt)) = range {
        if start >= total as u64 {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                .body(Body::empty())
                .expect("range response");
        }
        let end = end_opt.unwrap_or(total as u64).min(total as u64);
        if start >= end {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                .body(Body::empty())
                .expect("range response");
        }
        let start = start as usize;
        let end = end as usize;
        let content_len = if include_body {
            end - start
        } else {
            content_length_override.unwrap_or(end - start)
        };
        let mut builder = Response::builder()
            .status(if start == 0 && end == total {
                StatusCode::OK
            } else {
                StatusCode::PARTIAL_CONTENT
            })
            .header(header::CONTENT_LENGTH, content_len.to_string());
        if let Some(content_type) = content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        if accept_ranges {
            builder = builder.header(header::ACCEPT_RANGES, "bytes");
        }
        if start != 0 || end != total {
            builder = builder.header(
                header::CONTENT_RANGE,
                format!("bytes {}-{}/{}", start, end.saturating_sub(1), total),
            );
        }
        let body = if include_body {
            Body::from(data[start..end].to_vec())
        } else {
            Body::empty()
        };
        return builder.body(body).expect("range response");
    }

    let mut builder = Response::builder().status(StatusCode::OK).header(
        header::CONTENT_LENGTH,
        content_length_override.unwrap_or(total).to_string(),
    );
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    if accept_ranges {
        builder = builder.header(header::ACCEPT_RANGES, "bytes");
    }
    let body = if include_body {
        Body::from(data.to_vec())
    } else {
        Body::empty()
    };
    builder.body(body).expect("range response")
}
