use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use bytes::Bytes;
use futures::Stream;
use kithara_apple::foundation::ns::NSData;
use kithara_bufpool::BytePool;
use kithara_platform::{
    CancelToken, CancelWakerGuard,
    sync::{Arc, Mutex},
    tokio::{select, sync::oneshot},
};

use super::{
    delegate::AppleSessionEvents,
    response::{AppleDataResponse, HTTP_PARTIAL_CONTENT, StreamHead, copy_data},
    session::{AppleTask, TaskId},
};
use crate::{ByteStream, error::NetError, types::Headers};

pub(crate) struct AppleStreamResponse {
    pub(crate) headers: Headers,
    pub(crate) status: Option<u16>,
    task: AppleTask,
    body_queue: Arc<AppleBodyQueue>,
    cancel: CancelToken,
    partial: bool,
}

impl AppleStreamResponse {
    pub(crate) fn cancel(&self) {
        self.task.cancel();
    }
}

impl From<AppleStreamResponse> for ByteStream {
    fn from(response: AppleStreamResponse) -> Self {
        let AppleStreamResponse {
            headers,
            partial,
            body_queue,
            task,
            cancel,
            ..
        } = response;
        let expected_len = content_length(&headers);
        let stream = AppleBodyStream {
            body_queue,
            task,
            cancel,
            expected_len,
            cancel_wake: None,
            done: false,
            received: 0,
        };
        Self::with_partial(headers, Box::pin(stream), partial)
    }
}

pub(super) struct StartedStream {
    pub(super) task: AppleTask,
    pub(super) body_queue: Arc<AppleBodyQueue>,
    pub(super) head_receiver: oneshot::Receiver<Result<StreamHead, NetError>>,
    pub(super) task_id: TaskId,
}

struct AppleBodyStream {
    task: AppleTask,
    body_queue: Arc<AppleBodyQueue>,
    cancel: CancelToken,
    cancel_wake: Option<CancelWakerGuard>,
    expected_len: Option<u64>,
    done: bool,
    received: u64,
}

impl Stream for AppleBodyStream {
    type Item = Result<Bytes, NetError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if this.cancel.is_cancelled() {
            if this.body_complete() {
                this.cancel_wake = None;
                this.done = true;
                return Poll::Ready(None);
            }
            this.cancel_wake = None;
            this.done = true;
            this.task.cancel();
            return Poll::Ready(Some(Err(NetError::Cancelled)));
        }

        match this.body_queue.poll_next(cx) {
            Poll::Ready(None) => {
                this.cancel_wake = None;
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(item)) => {
                this.cancel_wake = None;
                match &item {
                    Ok(bytes) => this.record_received(bytes.len()),
                    Err(NetError::Cancelled) if this.body_complete() => {
                        this.done = true;
                        return Poll::Ready(None);
                    }
                    Err(_) => {}
                }
                if item.is_err() {
                    this.done = true;
                }
                Poll::Ready(Some(item))
            }
            Poll::Pending => {
                let waker = cx.waker().clone();
                this.cancel_wake = Some(this.cancel.on_cancel(move || waker.wake_by_ref()));
                if this.cancel.is_cancelled() {
                    this.cancel_wake = None;
                    this.done = true;
                    this.task.cancel();
                    return Poll::Ready(Some(Err(NetError::Cancelled)));
                }
                Poll::Pending
            }
        }
    }
}

pub(super) struct AppleBodyQueue {
    task: AppleTask,
    byte_pool: BytePool,
    inner: Mutex<AppleBodyQueueInner>,
    capacity: usize,
    resume_at: usize,
}

struct AppleBodyQueueInner {
    waker: Option<Waker>,
    items: VecDeque<Result<Bytes, NetError>>,
    closed: bool,
    suspended: bool,
}

impl AppleBodyQueue {
    pub(super) fn new(
        task: AppleTask,
        capacity: usize,
        resume_at: usize,
        byte_pool: BytePool,
    ) -> Self {
        Self {
            byte_pool,
            task,
            capacity,
            resume_at,
            inner: Mutex::new(AppleBodyQueueInner {
                closed: false,
                items: VecDeque::new(),
                suspended: false,
                waker: None,
            }),
        }
    }

    pub(super) fn close(&self, terminal: Option<NetError>) {
        let (resume, waker) = {
            let mut inner = self.inner.lock();
            if inner.closed {
                return;
            }
            if let Some(error) = terminal {
                inner.items.push_back(Err(error));
            }
            inner.closed = true;
            let resume = inner.suspended;
            inner.suspended = false;
            (resume, inner.waker.take())
        };
        if resume {
            self.task.resume();
        }
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, NetError>>> {
        let mut inner = self.inner.lock();
        if let Some(item) = inner.items.pop_front() {
            let resume = inner.suspended && inner.items.len() <= self.resume_at;
            if resume {
                inner.suspended = false;
            }
            drop(inner);
            if resume {
                self.task.resume();
            }
            return Poll::Ready(Some(item));
        }
        if inner.closed {
            return Poll::Ready(None);
        }
        inner.waker = Some(cx.waker().clone());
        Poll::Pending
    }

    pub(super) fn push(&self, bytes: Bytes) {
        let (suspend, waker) = {
            let mut inner = self.inner.lock();
            if inner.closed {
                return;
            }
            inner.items.push_back(Ok(bytes));
            let suspend =
                self.capacity > 0 && inner.items.len() >= self.capacity && !inner.suspended;
            if suspend {
                inner.suspended = true;
            }
            (suspend, inner.waker.take())
        };
        if suspend {
            self.task.suspend();
        }
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub(super) fn push_data(&self, data: &NSData) {
        match copy_data(data, &self.byte_pool) {
            Ok(bytes) => self.push(bytes),
            Err(error) => {
                self.task.cancel();
                self.close(Some(error));
            }
        }
    }
}

impl AppleBodyStream {
    fn body_complete(&self) -> bool {
        self.expected_len
            .is_some_and(|expected| self.received >= expected)
    }

    fn record_received(&mut self, len: usize) {
        let len = u64::try_from(len).unwrap_or(u64::MAX);
        self.received = self.received.saturating_add(len);
    }
}

impl Drop for AppleBodyStream {
    fn drop(&mut self) {
        if !self.done {
            self.task.cancel();
        }
    }
}

fn content_length(headers: &Headers) -> Option<u64> {
    headers
        .get("content-length")
        .or_else(|| headers.get("Content-Length"))
        .and_then(|value| value.parse().ok())
}

pub(super) async fn wait_for_data(
    receiver: oneshot::Receiver<Result<AppleDataResponse, NetError>>,
    task: AppleTask,
    cancel: CancelToken,
) -> Result<AppleDataResponse, NetError> {
    let mut guard = DataTaskGuard::new(task);
    select! {
        biased;
        () = cancel.cancelled() => {
            guard.cancel();
            Err(NetError::Cancelled)
        }
        result = receiver => {
            let result = result.unwrap_or_else(
                |_| Err(NetError::Network("NSURLSession data task closed without completion".to_string())),
            );
            guard.disarm();
            result
        }
    }
}

pub(super) async fn wait_for_stream_head(
    started: StartedStream,
    events: Arc<AppleSessionEvents>,
    cancel: CancelToken,
) -> Result<AppleStreamResponse, NetError> {
    let StartedStream {
        body_queue,
        task,
        head_receiver,
        task_id,
    } = started;
    let mut guard = StreamStartGuard::new(task, Arc::clone(&events), task_id);

    select! {
        biased;
        () = cancel.cancelled() => {
            guard.cancel();
            Err(NetError::Cancelled)
        }
        result = head_receiver => {
            match result {
                Ok(Ok(head)) => {
                    let task = guard.disarm_task();
                    let headers = head.headers;
                    let status = head.status;
                    let partial = status == Some(HTTP_PARTIAL_CONTENT);
                    Ok(AppleStreamResponse {
                        headers,
                        status,
                        task,
                        body_queue,
                        cancel,
                        partial,
                    })
                }
                Ok(Err(error)) => {
                    guard.cancel();
                    Err(error)
                }
                Err(_) => {
                    guard.cancel();
                    Err(NetError::Network("NSURLSession stream closed before response headers".to_string()))
                }
            }
        }
    }
}

struct DataTaskGuard {
    task: AppleTask,
    active: bool,
}

impl DataTaskGuard {
    fn new(task: AppleTask) -> Self {
        Self { task, active: true }
    }

    fn cancel(&mut self) {
        if self.active {
            self.task.cancel();
            self.active = false;
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for DataTaskGuard {
    fn drop(&mut self) {
        if self.active {
            self.task.cancel();
        }
    }
}

struct StreamStartGuard {
    task: AppleTask,
    events: Arc<AppleSessionEvents>,
    task_id: TaskId,
    active: bool,
}

impl StreamStartGuard {
    fn new(task: AppleTask, events: Arc<AppleSessionEvents>, task_id: TaskId) -> Self {
        Self {
            events,
            task,
            task_id,
            active: true,
        }
    }

    fn cancel(&mut self) {
        if self.active {
            self.task.cancel();
            self.events.remove_stream(self.task_id);
            self.active = false;
        }
    }

    fn disarm_task(&mut self) -> AppleTask {
        self.active = false;
        self.task.clone()
    }
}

impl Drop for StreamStartGuard {
    fn drop(&mut self) {
        if self.active {
            self.task.cancel();
            self.events.remove_stream(self.task_id);
        }
    }
}
