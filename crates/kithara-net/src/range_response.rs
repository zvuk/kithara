use url::Url;

use crate::{
    error::{NetError, NetResult},
    types::{Headers, RangeSpec},
};

struct HttpStatus;

impl HttpStatus {
    const OK: u16 = 200;
    const PARTIAL_CONTENT: u16 = 206;
    const SUCCESS_END: u16 = 300;
}

struct ContentRange {
    end: u64,
    start: u64,
    total: Option<u64>,
}

pub(crate) fn accepts_response_status(status: u16, accept_partial: bool) -> bool {
    (HttpStatus::OK..HttpStatus::SUCCESS_END).contains(&status)
        && (accept_partial || status != HttpStatus::PARTIAL_CONTENT)
}

pub(crate) fn representation_total(partial: bool, headers: &Headers) -> Option<u64> {
    if partial {
        return headers
            .get("content-range")
            .and_then(parse_content_range)
            .and_then(|range| range.total);
    }
    headers
        .get("content-length")
        .and_then(|length| length.parse().ok())
}

pub(crate) fn validate_range_response(
    status: u16,
    requested: Option<&RangeSpec>,
    headers: &Headers,
    url: &Url,
) -> NetResult<()> {
    let Some(requested) = requested else {
        return match headers.get("content-range") {
            Some(_) => Err(NetError::Decode(format!(
                "full response for {url} included content-range"
            ))),
            None => Ok(()),
        };
    };

    match status {
        HttpStatus::OK if headers.get("content-range").is_none() => Ok(()),
        HttpStatus::OK => Err(NetError::Decode(format!(
            "full response for range {requested} at {url} included content-range"
        ))),
        HttpStatus::PARTIAL_CONTENT => validate_partial_response(requested, headers, url),
        _ => Err(NetError::Decode(format!(
            "range request {requested} for {url} returned HTTP {status}; expected 200 or 206"
        ))),
    }
}

fn validate_partial_response(requested: &RangeSpec, headers: &Headers, url: &Url) -> NetResult<()> {
    let raw = headers.get("content-range").ok_or_else(|| {
        NetError::Decode(format!(
            "partial response for range {requested} at {url} omitted content-range"
        ))
    })?;
    let parsed = parse_content_range(raw).ok_or_else(|| {
        NetError::Decode(format!(
            "partial response for range {requested} at {url} had invalid content-range: {raw}"
        ))
    })?;
    let interval_matches = parsed.start == requested.start
        && parsed.end >= parsed.start
        && requested.end.is_none_or(|end| parsed.end <= end)
        && parsed.total.is_some_and(|total| total > parsed.end);
    if !interval_matches {
        return Err(NetError::Decode(format!(
            "partial response for range {requested} at {url} returned content-range {raw}"
        )));
    }

    let span = parsed
        .end
        .checked_sub(parsed.start)
        .and_then(|span| span.checked_add(1))
        .ok_or_else(|| {
            NetError::Decode(format!(
                "partial response for range {requested} at {url} had overflowing content-range {raw}"
            ))
        })?;
    let length = headers.get("content-length").ok_or_else(|| {
        NetError::Decode(format!(
            "partial response for range {requested} at {url} omitted content-length for span {span}"
        ))
    })?;
    let length = length.parse::<u64>().map_err(|_| {
        NetError::Decode(format!(
            "partial response for range {requested} at {url} had invalid content-length: {length}"
        ))
    })?;
    if length != span {
        return Err(NetError::Decode(format!(
            "partial response for range {requested} at {url} declared {length} bytes for span {span}"
        )));
    }
    Ok(())
}

fn parse_content_range(raw: &str) -> Option<ContentRange> {
    let (unit, value) = raw.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (interval, total) = value.split_once('/')?;
    let (start, end) = interval.split_once('-')?;
    let total = total.trim();
    let total = match total {
        "*" => None,
        total => Some(total.parse().ok()?),
    };
    Some(ContentRange {
        end: end.trim().parse().ok()?,
        start: start.trim().parse().ok()?,
        total,
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use url::Url;

    use super::{accepts_response_status, validate_range_response};
    use crate::{Headers, NetError, RangeSpec};

    fn test_url() -> Url {
        Url::parse("https://example.com/audio.bin").expect("test URL")
    }

    #[kithara::test(native, flash(false))]
    fn partial_status_requires_range_permission() {
        assert!(accepts_response_status(200, false));
        assert!(accepts_response_status(206, true));
        assert!(!accepts_response_status(206, false));
        assert!(!accepts_response_status(404, true));
    }

    #[kithara::test(native, flash(false))]
    fn partial_range_requires_content_range() {
        let mut headers = Headers::default();
        headers.insert("content-length", "16");
        let range = RangeSpec::new(0, Some(15));

        assert!(matches!(
            validate_range_response(206, Some(&range), &headers, &test_url()),
            Err(NetError::Decode(_))
        ));
    }

    #[kithara::test(native, flash(false))]
    fn partial_range_requires_content_length() {
        let mut headers = Headers::default();
        headers.insert("content-range", "bytes 0-3/8");
        let range = RangeSpec::new(0, Some(3));

        assert!(matches!(
            validate_range_response(206, Some(&range), &headers, &test_url()),
            Err(NetError::Decode(_))
        ));
    }

    #[kithara::test(native, flash(false))]
    fn full_response_may_ignore_range() {
        let mut headers = Headers::default();
        headers.insert("content-length", "60");
        let range = RangeSpec::new(0, Some(15));

        assert!(validate_range_response(200, Some(&range), &headers, &test_url()).is_ok());
    }

    #[kithara::test(native, flash(false))]
    fn partial_range_accepts_content_range() {
        let mut headers = Headers::default();
        headers.insert("content-length", "16");
        headers.insert("content-range", "bytes 0-15/60");
        let range = RangeSpec::new(0, Some(15));

        assert!(validate_range_response(206, Some(&range), &headers, &test_url()).is_ok());
    }

    #[kithara::test(native, flash(false))]
    fn partial_range_rejects_an_unknown_total() {
        let mut headers = Headers::default();
        headers.insert("content-length", "8");
        headers.insert("content-range", "bytes 8-15/*");
        let range = RangeSpec::new(8, Some(15));

        assert!(matches!(
            validate_range_response(206, Some(&range), &headers, &test_url()),
            Err(NetError::Decode(_))
        ));
    }

    #[kithara::test(native, flash(false))]
    fn full_response_rejects_content_range() {
        let mut headers = Headers::default();
        headers.insert("content-length", "60");
        headers.insert("content-range", "bytes 0-15/60");
        let range = RangeSpec::new(0, Some(15));

        assert!(matches!(
            validate_range_response(200, Some(&range), &headers, &test_url()),
            Err(NetError::Decode(_))
        ));
    }

    #[kithara::test(native, flash(false))]
    fn resumed_range_rejects_a_different_interval() {
        let mut headers = Headers::default();
        headers.insert("content-length", "8");
        headers.insert("content-range", "bytes 0-7/60");
        let range = RangeSpec::new(8, Some(15));

        assert!(matches!(
            validate_range_response(206, Some(&range), &headers, &test_url()),
            Err(NetError::Decode(_))
        ));
    }

    #[kithara::test(native, flash(false))]
    fn partial_range_rejects_an_inconsistent_span() {
        let mut headers = Headers::default();
        headers.insert("content-length", "7");
        headers.insert("content-range", "bytes 8-15/60");
        let range = RangeSpec::new(8, Some(15));

        assert!(matches!(
            validate_range_response(206, Some(&range), &headers, &test_url()),
            Err(NetError::Decode(_))
        ));
    }

    #[kithara::test(native, flash(false))]
    fn range_request_rejects_other_success_statuses() {
        let range = RangeSpec::new(0, Some(15));

        assert!(matches!(
            validate_range_response(204, Some(&range), &Headers::default(), &test_url()),
            Err(NetError::Decode(_))
        ));
    }
}
