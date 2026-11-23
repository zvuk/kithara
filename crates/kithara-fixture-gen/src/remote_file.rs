use std::io::{Read, Write as _};

use kithara_platform::time::{Duration, SystemTime};
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
    header::{CONTENT_RANGE, RANGE},
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::hls_hydrate::{Deadline, HydrateError, RedactedUrl};

#[derive(Debug, Error)]
pub(crate) enum RemoteFileError {
    #[error(transparent)]
    Fetch(#[from] HydrateError),
    #[error("invalid remote fixture URL")]
    Url(#[from] url::ParseError),
    #[error("{url}: expected {expected} bytes, received {received}")]
    Length {
        url: RedactedUrl,
        expected: u64,
        received: u64,
    },
    #[error("{url}: answered {status} instead of resuming at byte {offset}")]
    Resume {
        url: RedactedUrl,
        offset: u64,
        status: StatusCode,
    },
    #[error("{url}: SHA-256 mismatch, expected {expected}, received {received}")]
    Digest {
        url: RedactedUrl,
        expected: String,
        received: String,
    },
}

/// Downloads one public file and verifies its size and SHA-256 digest.
///
/// A request or transfer silent for `stall` is asked again from the byte it
/// reached: a CDN edge can hold a request or a response open without sending
/// anything, and one such stall must not spend the whole `timeout`.
/// The blocking-client timeout bounds each response/read wait rather than the whole
/// transfer. Interrupted bodies retain received bytes for the next resumed request.
pub(crate) fn fetch_verified(
    url: &Url,
    sha256_hex: &str,
    length: u64,
    timeout: Duration,
    stall: Duration,
) -> Result<Vec<u8>, RemoteFileError> {
    let client = Client::builder().timeout(stall).build().map_err(|source| {
        diagnostic(
            url,
            "complete",
            format_args!("outcome=client_error requests_started=0 retained_bytes=0"),
        );
        HydrateError::Request {
            url: RedactedUrl::new(url),
            source: source.without_url(),
        }
    })?;
    let deadline = Deadline::new(timeout);
    diagnostic(
        url,
        "budget_start",
        format_args!(
            "deadline={deadline:?} timeout_ms={} stall_ms={}",
            timeout.as_millis(),
            stall.as_millis()
        ),
    );
    let mut bytes = Vec::new();
    let mut requests_started = 0_u64;
    loop {
        let offset = bytes.len() as u64;
        let response =
            request(&client, url, offset, deadline, &mut requests_started).inspect_err(|_| {
                diagnostic(
                    url,
                    "complete",
                    format_args!(
                        "outcome=request_error requests_started={requests_started} retained_bytes={offset}"
                    ),
                );
            })?;
        let Some(mut response) = response else {
            continue;
        };
        let complete = response.read_to_end(&mut bytes).is_ok();
        diagnostic(
            url,
            "body",
            format_args!(
                "request_sequence={requests_started} offset={offset} retained_bytes={} complete={complete}",
                bytes.len()
            ),
        );
        if complete {
            break;
        }
    }
    let received = bytes.len() as u64;
    if received != length {
        diagnostic(
            url,
            "complete",
            format_args!(
                "outcome=length_error requests_started={requests_started} retained_bytes={received}"
            ),
        );
        return Err(RemoteFileError::Length {
            received,
            url: RedactedUrl::new(url),
            expected: length,
        });
    }
    let digest = hex::encode(Sha256::digest(&bytes));
    if digest != sha256_hex {
        diagnostic(
            url,
            "complete",
            format_args!(
                "outcome=digest_error requests_started={requests_started} retained_bytes={received}"
            ),
        );
        return Err(RemoteFileError::Digest {
            url: RedactedUrl::new(url),
            expected: sha256_hex.to_owned(),
            received: digest,
        });
    }
    diagnostic(
        url,
        "complete",
        format_args!(
            "outcome=verified requests_started={requests_started} retained_bytes={received}"
        ),
    );
    Ok(bytes)
}

fn diagnostic(url: &Url, phase: &str, fields: std::fmt::Arguments<'_>) {
    // A diagnostic write failure must not change the fixture result.
    let _ = writeln!(
        std::io::stdout().lock(),
        "cargo:warning=fixture_fetch pid={} at={:?} url={} phase={phase} {fields}",
        std::process::id(),
        SystemTime::now(),
        RedactedUrl::new(url)
    );
}

/// Requests `url` from byte `offset` on, refusing an answer that restarts or
/// skips the body instead of continuing it; `None` when the request went
/// unanswered for the client's stall timeout.
/// `requests_started` records the request sequence after the deadline check;
/// its value is used only by diagnostics.
fn request(
    client: &Client,
    url: &Url,
    offset: u64,
    deadline: Deadline,
    requests_started: &mut u64,
) -> Result<Option<Response>, RemoteFileError> {
    let remaining = deadline.remaining(url).inspect_err(|_| {
        diagnostic(
            url,
            "budget_expired",
            format_args!("requests_started={requests_started} retained_bytes={offset}"),
        );
    })?;
    *requests_started += 1;
    diagnostic(
        url,
        "request",
        format_args!(
            "request_sequence={requests_started} offset={offset} remaining_ms={}",
            remaining.as_millis()
        ),
    );
    let mut request = client.get(url.clone());
    if offset > 0 {
        request = request.header(RANGE, format!("bytes={offset}-"));
    }
    let response = match request.send() {
        Ok(response) => response,
        Err(source) if source.is_timeout() => {
            diagnostic(
                url,
                "response",
                format_args!("request_sequence={requests_started} offset={offset} outcome=timeout"),
            );
            return Ok(None);
        }
        Err(source) => {
            return Err(HydrateError::Request {
                url: RedactedUrl::new(url),
                source: source.without_url(),
            }
            .into());
        }
    };
    let status = response.status();
    diagnostic(
        url,
        "response",
        format_args!(
            "request_sequence={requests_started} offset={offset} status={}",
            status.as_u16()
        ),
    );
    if !status.is_success() {
        return Err(HydrateError::Status {
            status,
            url: RedactedUrl::new(url),
        }
        .into());
    }
    let resumes_here = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|range| range.to_str().ok())
        .is_some_and(|range| range.starts_with(&format!("bytes {offset}-")));
    if offset > 0 && (status != StatusCode::PARTIAL_CONTENT || !resumes_here) {
        return Err(RemoteFileError::Resume {
            url: RedactedUrl::new(url),
            offset,
            status,
        });
    }
    Ok(Some(response))
}
