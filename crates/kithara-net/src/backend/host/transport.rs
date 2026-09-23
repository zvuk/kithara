use std::fmt;

use bytes::Bytes;
use kithara_bufpool::ByteBuffer;
use kithara_platform::sync::Arc;
use url::Url;

use super::call::CallState;
use crate::{
    backend::pooled::{ByteBuffers, pooled_bytes},
    error::NetError,
};

/// The HTTP transport a host application runs every request through.
///
/// A transport:
/// - returns at once from every method Kithara calls;
/// - reports [`HostEvents::response`] once per call, for any status, before
///   the first read completes, after following redirects;
/// - answers each [`HostCall::read`] with [`HostEvents::read`] or
///   [`HostEvents::end`];
/// - ends each call with exactly one [`HostEvents::end`] or
///   [`HostEvents::fail`] with [`HostFailure::Transport`] or
///   [`HostFailure::Permanent`], a cancelled call included, and writes no
///   lent buffer after it;
/// - owns trust, proxies, cookies, timeouts, pooling and content coding.
pub trait HostTransport: fmt::Debug + Send + Sync {
    /// Start `request` without blocking; its progress goes to `events`.
    ///
    /// # Errors
    ///
    /// Returns [`HostFailure`] when the call cannot start.
    fn start(
        &self,
        request: HostRequest,
        events: HostEvents,
    ) -> Result<Box<dyn HostCall>, HostFailure>;
}

/// One call a [`HostTransport`] started.
pub trait HostCall: Send + Sync {
    /// Fill `buffer` from its start and hand it back through
    /// [`HostEvents::read`]. Kithara lends one buffer at a time.
    fn read(&self, buffer: HostBuffer);

    /// Stop the call from any thread. Idempotent.
    fn cancel(&self);
}

/// An HTTP method a [`HostTransport`] runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostMethod {
    Get,
    Head,
    Post,
}

impl HostMethod {
    /// The method name as it goes on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
        }
    }
}

/// One request for a [`HostTransport`] to run.
#[derive(Debug)]
pub struct HostRequest {
    pub method: HostMethod,
    pub url: Url,
    /// Header names and values, sent as given.
    pub headers: Vec<(String, String)>,
    /// The body of a POST, readable until the call's terminal report.
    pub body: Option<HostRequestBody>,
}

/// The body of a request, lent to a transport until the call's terminal
/// report.
pub struct HostRequestBody(ByteBuffer);

impl HostRequestBody {
    pub(super) fn with_bytes(buffers: &ByteBuffers, bytes: &Bytes) -> Result<Self, NetError> {
        let mut body = buffers
            .get_with_len(bytes.len())
            .map_err(|error| NetError::Network(error.to_string()))?;
        body.copy_from_slice(bytes);
        Ok(Self(body))
    }
}

impl AsRef<[u8]> for HostRequestBody {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsMut<[u8]> for HostRequestBody {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl fmt::Debug for HostRequestBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostRequestBody")
            .field("len", &self.0.len())
            .finish()
    }
}

/// Memory Kithara lends a transport as the destination of one read.
pub struct HostBuffer(ByteBuffer);

impl HostBuffer {
    pub(super) fn with_len(buffers: &ByteBuffers, len: usize) -> Result<Self, NetError> {
        buffers
            .get_with_len(len)
            .map(Self)
            .map_err(|error| NetError::Network(error.to_string()))
    }

    pub(super) fn into_bytes(self, len: usize) -> Bytes {
        let mut bytes = self.0;
        bytes.truncate(len);
        pooled_bytes(bytes)
    }
}

impl AsRef<[u8]> for HostBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsMut<[u8]> for HostBuffer {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl fmt::Debug for HostBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostBuffer")
            .field("len", &self.0.len())
            .finish()
    }
}

/// Why a call ended without the end of its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostFailure {
    /// The exchange failed in transit; the request may be retried.
    Transport(String),
    /// The host refused the request for good, for example by its network or
    /// trust policy; the request is not retried.
    Permanent(String),
    /// The transport broke its protocol; Kithara cancels the call and does not
    /// retry the request.
    Protocol(String),
}

impl From<HostFailure> for NetError {
    fn from(failure: HostFailure) -> Self {
        match failure {
            HostFailure::Transport(message) => Self::Network(message),
            HostFailure::Permanent(message) => Self::Refused(message),
            HostFailure::Protocol(message) => Self::Protocol(message),
        }
    }
}

/// Kithara's side of one call, callable from any thread.
///
/// The first terminal report ([`Self::end`] or [`Self::fail`]) settles the
/// call; reports after it change nothing.
#[derive(Clone)]
pub struct HostEvents(pub(super) Arc<CallState>);

impl HostEvents {
    delegate::delegate! {
        to self.0 {
            /// Report the response status and headers.
            pub fn response(&self, status: u16, headers: Vec<(String, String)>);
            /// Hand back the buffer of the outstanding read with `len` bytes
            /// written from its start.
            pub fn read(&self, buffer: HostBuffer, len: usize);
            /// Report the end of the body in answer to the outstanding read.
            pub fn end(&self);
            /// Report the failure that ends the call for Kithara.
            pub fn fail(&self, failure: HostFailure);
        }
    }
}

impl fmt::Debug for HostEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostEvents").finish_non_exhaustive()
    }
}
