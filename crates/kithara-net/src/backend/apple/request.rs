use bytes::Bytes;
use kithara_apple::foundation::{
    ns::{NSData, NSMutableURLRequest, NSString, NSURL},
    objc::rc::Retained,
};
use url::Url;

use crate::{
    error::{NetError, NetResult},
    types::{AcceptEncodingPolicy, Headers, RangeSpec},
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
    accept_encoding: AcceptEncodingPolicy,
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
        accept_encoding: AcceptEncodingPolicy,
    ) -> NetResult<Self> {
        validate_url(url)?;
        Ok(Self {
            accept_encoding,
            body,
            method,
            headers,
            range,
            url: url.clone(),
        })
    }

    pub(super) fn accept_encoding(&self) -> AcceptEncodingPolicy {
        self.accept_encoding
    }

    pub(super) fn url(&self) -> &Url {
        &self.url
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

        if let Some(headers) = self.headers {
            for (key, value) in headers.iter() {
                if !key.eq_ignore_ascii_case("accept-encoding") {
                    set_header(&request, key, value);
                }
            }
        }
        let accept_encoding = match self.accept_encoding {
            AcceptEncodingPolicy::Configured => accept_encoding,
            AcceptEncodingPolicy::Identity => "identity",
        };
        set_header(&request, "Accept-Encoding", accept_encoding);
        if let Some(range) = self.range {
            let value = range.to_string();
            set_header(&request, "Range", &value);
        }

        Ok(request)
    }
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
