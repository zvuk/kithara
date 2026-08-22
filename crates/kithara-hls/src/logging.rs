use std::fmt;

use kithara_net::NetError;
use url::{Host, Url};

pub(crate) struct RedactedUrl<'a>(&'a Url);

impl<'a> RedactedUrl<'a> {
    pub(crate) const fn new(url: &'a Url) -> Self {
        Self(url)
    }
}

impl fmt::Display for RedactedUrl<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.0.scheme())?;
        if let Some(host) = self.0.host() {
            f.write_str("//")?;
            match host {
                Host::Domain(domain) => f.write_str(domain)?,
                Host::Ipv4(address) => write!(f, "{address}")?,
                Host::Ipv6(address) => write!(f, "[{address}]")?,
            }
            if let Some(port) = self.0.port() {
                write!(f, ":{port}")?;
            }
        }
        f.write_str(self.0.path())
    }
}

pub(crate) struct RedactedNetError<'a>(&'a NetError);

impl<'a> RedactedNetError<'a> {
    pub(crate) const fn new(error: &'a NetError) -> Self {
        Self(error)
    }
}

impl fmt::Display for RedactedNetError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            NetError::Status { status, .. } => write!(f, "HTTP {status}"),
            NetError::Timeout => f.write_str("timeout"),
            NetError::Decode(_) => f.write_str("decode error"),
            NetError::RetryExhausted {
                max_retries,
                source,
            } => write!(
                f,
                "request failed after {max_retries} retries: {}",
                Self::new(source)
            ),
            NetError::Unimplemented => f.write_str("not implemented"),
            NetError::Cancelled => f.write_str("cancelled"),
            NetError::InvalidContentType(_) => f.write_str("invalid content type"),
            _ => f.write_str("network error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn display_omits_credentials_query_and_fragment() {
        let url = Url::parse(
            "https://api-user:api-password@keys.example.com:8443/key.bin?access_token=secret#part",
        )
        .expect("valid URL");

        let display = RedactedUrl::new(&url).to_string();

        assert_eq!(display, "https://keys.example.com:8443/key.bin");
        assert!(!display.contains("api-user"));
        assert!(!display.contains("api-password"));
        assert!(!display.contains("secret"));
        assert!(!display.contains("part"));
    }

    #[kithara::test(native, flash(false))]
    fn network_error_display_omits_url_body_and_backend_details() {
        let url = Url::parse("https://api-user:api-password@keys.example/key?token=secret")
            .expect("valid URL");
        let status = NetError::Status {
            status: NonZeroU16::new(403).expect("non-zero status"),
            url: Some(url),
            body: Some("secret response".to_string()),
        };
        let network = NetError::Network(
            "request failed for https://keys.example/key?token=secret".to_string(),
        );

        assert_eq!(RedactedNetError::new(&status).to_string(), "HTTP 403");
        assert_eq!(RedactedNetError::new(&network).to_string(), "network error");
    }
}
