use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::Stream;
use kithara_platform::{
    CancelToken, CancelWakerGuard,
    tokio::{
        select,
        sync::{mpsc, oneshot},
    },
};
use objc2::rc::Retained;

use super::{
    delegate::AppleSessionDelegate,
    response::{AppleDataResponse, StreamHead, take_terminal},
    session::{AppleTask, TaskId},
};
use crate::{ByteStream, error::NetError, types::Headers};

pub(crate) struct AppleStreamResponse {
    pub(crate) headers: Headers,
    pub(crate) status: Option<u16>,
    task: AppleTask,
    terminal: Arc<Mutex<Option<NetError>>>,
    cancel: CancelToken,
    receiver: mpsc::Receiver<Result<Bytes, NetError>>,
}

impl AppleStreamResponse {
    pub(crate) fn cancel(&self) {
        self.task.cancel();
    }
}

impl From<AppleStreamResponse> for ByteStream {
    fn from(response: AppleStreamResponse) -> Self {
        let stream = AppleBodyStream {
            task: response.task,
            terminal: response.terminal,
            cancel: response.cancel,
            cancel_wake: None,
            receiver: response.receiver,
            done: false,
        };
        Self::new(response.headers, Box::pin(stream))
    }
}

pub(super) struct StartedStream {
    pub(super) task: AppleTask,
    pub(super) terminal: Arc<Mutex<Option<NetError>>>,
    pub(super) body_receiver: mpsc::Receiver<Result<Bytes, NetError>>,
    pub(super) head_receiver: oneshot::Receiver<Result<StreamHead, NetError>>,
    pub(super) task_id: TaskId,
}

struct AppleBodyStream {
    task: AppleTask,
    terminal: Arc<Mutex<Option<NetError>>>,
    cancel: CancelToken,
    cancel_wake: Option<CancelWakerGuard>,
    receiver: mpsc::Receiver<Result<Bytes, NetError>>,
    done: bool,
}

impl Stream for AppleBodyStream {
    type Item = Result<Bytes, NetError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if this.cancel.is_cancelled() {
            this.cancel_wake = None;
            this.done = true;
            this.task.cancel();
            return Poll::Ready(Some(Err(NetError::Cancelled)));
        }
        if let Some(error) = take_terminal(&this.terminal) {
            this.cancel_wake = None;
            this.done = true;
            this.task.cancel();
            return Poll::Ready(Some(Err(error)));
        }

        match Pin::new(&mut this.receiver).poll_recv(cx) {
            Poll::Ready(None) => {
                this.cancel_wake = None;
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(item)) => {
                this.cancel_wake = None;
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

impl Drop for AppleBodyStream {
    fn drop(&mut self) {
        if !self.done {
            self.task.cancel();
        }
    }
}

pub(super) async fn wait_for_data(
    receiver: oneshot::Receiver<Result<AppleDataResponse, NetError>>,
    task: AppleTask,
    cancel: CancelToken,
) -> Result<AppleDataResponse, NetError> {
    let mut task = DataTaskGuard::new(task);
    select! {
        biased;
        () = cancel.cancelled() => {
            task.cancel();
            Err(NetError::Cancelled)
        }
        result = receiver => {
            let result = result.unwrap_or_else(
                |_| Err(NetError::Network("NSURLSession data task closed without completion".to_string())),
            );
            task.disarm();
            result
        }
    }
}

pub(super) async fn wait_for_stream_head(
    started: StartedStream,
    delegate: Retained<AppleSessionDelegate>,
    cancel: CancelToken,
) -> Result<AppleStreamResponse, NetError> {
    let StartedStream {
        task,
        terminal,
        body_receiver,
        head_receiver,
        task_id,
    } = started;
    let mut guard = StreamStartGuard::new(task, delegate, task_id);

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
                    let receiver = body_receiver;
                    Ok(AppleStreamResponse {
                        headers,
                        status,
                        task,
                        terminal,
                        cancel,
                        receiver,
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
    delegate: Retained<AppleSessionDelegate>,
    task_id: TaskId,
    active: bool,
}

impl StreamStartGuard {
    fn new(task: AppleTask, delegate: Retained<AppleSessionDelegate>, task_id: TaskId) -> Self {
        Self {
            delegate,
            task,
            task_id,
            active: true,
        }
    }

    fn cancel(&mut self) {
        if self.active {
            self.task.cancel();
            self.delegate.remove_stream(self.task_id);
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
            self.delegate.remove_stream(self.task_id);
        }
    }
}
