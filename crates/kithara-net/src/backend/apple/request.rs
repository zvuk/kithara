use bytes::Bytes;
use objc2::rc::Retained;
use objc2_foundation::{NSData, NSMutableURLRequest, NSString, NSURL};
use url::Url;

use crate::{
    error::{NetError, NetResult},
    types::{Compression, Headers, RangeSpec},
};

#[derive(Clone, Copy)]
pub(super) enum Method {
    Get,
    Head,
    Post,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
        }
    }
}

pub(super) struct AppleRequest {
    method: Method,
    body: Option<Bytes>,
    headers: Option<Headers>,
    range: Option<RangeSpec>,
    url: Url,
}

impl AppleRequest {
    pub(super) fn new(
        url: &Url,
        method: Method,
        range: Option<RangeSpec>,
        headers: Option<Headers>,
        body: Option<Bytes>,
    ) -> NetResult<Self> {
        validate_url(url)?;
        Ok(Self {
            body,
            method,
            headers,
            range,
            url: url.clone(),
        })
    }

    pub(super) fn into_ns_request(
        self,
        accept_encoding: &str,
    ) -> NetResult<Retained<NSMutableURLRequest>> {
        let ns_url = ns_url(&self.url)?;
        let request = NSMutableURLRequest::requestWithURL(&ns_url);
        request.setHTTPMethod(&NSString::from_str(self.method.as_str()));
        if let Some(body) = self.body {
            let body = NSData::with_bytes(&body);
            request.setHTTPBody(Some(&body));
        }

        let mut has_accept_encoding = false;
        if let Some(headers) = self.headers {
            for (key, value) in headers.iter() {
                if key.eq_ignore_ascii_case("accept-encoding") {
                    has_accept_encoding = true;
                }
                set_header(&request, key, value);
            }
        }
        if !has_accept_encoding {
            set_header(&request, "Accept-Encoding", accept_encoding);
        }
        if let Some(range) = self.range {
            let value = range.to_string();
            set_header(&request, "Range", &value);
        }

        Ok(request)
    }
}

pub(super) fn accept_encoding_value(compression: Compression) -> String {
    let mut codings = Vec::new();
    if compression.contains(Compression::GZIP) {
        codings.push("gzip");
    }
    if compression.contains(Compression::DEFLATE) {
        codings.push("deflate");
    }
    if compression.contains(Compression::BROTLI) {
        codings.push("br");
    }
    if compression.contains(Compression::ZSTD) {
        codings.push("zstd");
    }
    if codings.is_empty() {
        return "identity".to_string();
    }
    codings.join(", ")
}

fn validate_url(url: &Url) -> NetResult<()> {
    ns_url(url).map(|_| ())
}

fn ns_url(url: &Url) -> NetResult<Retained<NSURL>> {
    let url_string = NSString::from_str(url.as_str());
    NSURL::URLWithString(&url_string)
        .ok_or_else(|| NetError::Network(format!("NSURL rejected request URL {url}")))
}

fn set_header(request: &NSMutableURLRequest, key: &str, value: &str) {
    let key = NSString::from_str(key);
    let value = NSString::from_str(value);
    request.setValue_forHTTPHeaderField(Some(&value), &key);
}
