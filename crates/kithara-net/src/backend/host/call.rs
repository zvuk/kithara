use std::{
    pin::Pin,
    task::{Context, Poll, Waker},
};

use bytes::Bytes;
use futures::Stream;
use kithara_platform::{
    CancelToken, CancelWakerGuard,
    sync::{Arc, Mutex},
};

use super::transport::{HostBuffer, HostCall, HostFailure};
use crate::{backend::pooled::ByteBuffers, error::NetError};

pub(super) const READ_SIZE: usize = 64 * 1024;

pub(super) struct Response {
    pub(super) status: u16,
    pub(super) headers: Vec<(String, String)>,
}

// What the transport reported and Kithara has not taken yet.
#[derive(Default)]
pub(super) struct CallState {
    reports: Mutex<Reports>,
}

#[derive(Default)]
struct Reports {
    phase: Phase,
    response: Option<Response>,
    chunk: Option<Bytes>,
    outcome: Option<Result<(), NetError>>,
    transport_ended: bool,
    waker: Option<Waker>,
}

#[derive(Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Opening,
    Body,
    Settled,
}

impl CallState {
    pub(super) fn response(&self, status: u16, headers: Vec<(String, String)>) {
        self.update(|reports| match reports.phase {
            Phase::Opening => {
                reports.phase = Phase::Body;
                reports.response = Some(Response { status, headers });
            }
            Phase::Body => reports.violate(format!("a second response, HTTP {status}")),
            Phase::Settled => {}
        });
    }

    pub(super) fn read(&self, buffer: HostBuffer, len: usize) {
        self.update(|reports| match reports.phase {
            Phase::Opening => reports.violate("a read before the response".to_owned()),
            Phase::Body if len > buffer.as_ref().len() => reports.violate(format!(
                "a read of {len} bytes into a buffer of {}",
                buffer.as_ref().len()
            )),
            Phase::Body if reports.chunk.is_some() => {
                reports.violate("a read nobody asked for".to_owned());
            }
            Phase::Body => reports.chunk = Some(buffer.into_bytes(len)),
            Phase::Settled => {}
        });
    }

    pub(super) fn end(&self) {
        self.update(|reports| {
            reports.transport_ended = true;
            reports.settle(Ok(()));
        });
    }

    pub(super) fn fail(&self, failure: HostFailure) {
        self.update(|reports| {
            reports.transport_ended |= !matches!(failure, HostFailure::Protocol(_));
            reports.settle(Err(failure.into()));
        });
    }

    pub(super) fn transport_ended(&self) -> bool {
        self.reports.lock().transport_ended
    }

    pub(super) fn poll_response(&self, cx: &mut Context<'_>) -> Poll<Result<Response, NetError>> {
        let mut reports = self.reports.lock();
        if let Some(response) = reports.response.take() {
            return Poll::Ready(Ok(response));
        }
        match reports.outcome.take() {
            Some(Ok(())) => Poll::Ready(Err(NetError::Protocol(
                "the body ended before the response".to_owned(),
            ))),
            Some(Err(error)) => Poll::Ready(Err(error)),
            None => {
                reports.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    fn poll_chunk(&self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, NetError>>> {
        let mut reports = self.reports.lock();
        if let Some(chunk) = reports.chunk.take() {
            return Poll::Ready(Some(Ok(chunk)));
        }
        match reports.outcome.take() {
            Some(Ok(())) => Poll::Ready(None),
            Some(Err(error)) => Poll::Ready(Some(Err(error))),
            None => {
                reports.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    fn update(&self, report: impl FnOnce(&mut Reports)) {
        let waker = {
            let mut reports = self.reports.lock();
            report(&mut reports);
            reports.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl Reports {
    fn settle(&mut self, outcome: Result<(), NetError>) {
        if self.phase != Phase::Settled {
            self.phase = Phase::Settled;
            self.outcome = Some(outcome);
        }
    }

    fn violate(&mut self, message: String) {
        self.settle(Err(NetError::Protocol(message)));
    }
}

/// A started call; stopping or dropping it before the transport ended the
/// call cancels it once.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct Call {
    call: Box<dyn HostCall>,
    #[field(get, vis = "pub(super)")]
    state: Arc<CallState>,
    stopped: bool,
}

impl Call {
    pub(super) fn new(call: Box<dyn HostCall>, state: Arc<CallState>) -> Self {
        Self {
            call,
            state,
            stopped: false,
        }
    }

    pub(super) fn read(&self, buffer: HostBuffer) {
        self.call.read(buffer);
    }

    pub(super) fn stop(&mut self) {
        if !self.stopped && !self.state.transport_ended() {
            self.call.cancel();
        }
        self.stopped = true;
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(super) struct HostBodyStream {
    call: Call,
    buffers: ByteBuffers,
    cancel: CancelToken,
    cancel_wake: Option<CancelWakerGuard>,
    done: bool,
    outstanding: bool,
}

impl HostBodyStream {
    pub(super) fn new(call: Call, buffers: ByteBuffers, cancel: CancelToken) -> Self {
        Self {
            call,
            buffers,
            cancel,
            cancel_wake: None,
            done: false,
            outstanding: false,
        }
    }

    fn stop(&mut self, error: NetError) -> Poll<Option<Result<Bytes, NetError>>> {
        self.cancel_wake = None;
        self.done = true;
        self.call.stop();
        Poll::Ready(Some(Err(error)))
    }
}

impl Stream for HostBodyStream {
    type Item = Result<Bytes, NetError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if this.cancel.is_cancelled() {
            return this.stop(NetError::Cancelled);
        }
        if !this.outstanding {
            match HostBuffer::with_len(&this.buffers, READ_SIZE) {
                Ok(buffer) => this.call.read(buffer),
                Err(error) => return this.stop(error),
            }
            this.outstanding = true;
        }

        match this.call.state().poll_chunk(cx) {
            Poll::Pending => {
                let waker = cx.waker().clone();
                this.cancel_wake = Some(this.cancel.on_cancel(move || waker.wake_by_ref()));
                if this.cancel.is_cancelled() {
                    return this.stop(NetError::Cancelled);
                }
                Poll::Pending
            }
            Poll::Ready(Some(Ok(chunk))) => {
                this.cancel_wake = None;
                this.outstanding = false;
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(error))) => this.stop(error),
            Poll::Ready(None) => {
                this.cancel_wake = None;
                this.done = true;
                Poll::Ready(None)
            }
        }
    }
}
