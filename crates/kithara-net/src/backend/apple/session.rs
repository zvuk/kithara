#![allow(unsafe_code)]

use std::{
    future::Future,
    sync::{Arc, Mutex},
};

use block2::RcBlock;
use kithara_platform::{
    CancelToken,
    tokio::sync::{mpsc, oneshot},
};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::{
    NSData, NSError, NSInteger, NSOperationQueue, NSUInteger, NSURLResponse, NSURLSession,
    NSURLSessionConfiguration, NSURLSessionDataTask, NSURLSessionDelegate, NSURLSessionTask,
};

use super::{
    delegate::{AppleSessionDelegate, StreamState},
    request::{AppleRequest, accept_encoding_value},
    response::{AppleDataResponse, completion_result, send_once},
    stream::{AppleStreamResponse, StartedStream, wait_for_data, wait_for_stream_head},
};
use crate::{
    error::{NetError, NetResult},
    types::NetOptions,
};

const STREAM_BUFFER_CHUNKS: usize = 32;

pub(super) type TaskId = NSUInteger;
type DataStart = (
    oneshot::Receiver<Result<AppleDataResponse, NetError>>,
    AppleTask,
);

/// SAFETY: Apple documents `NSURLSession` as thread-safe
/// (<https://developer.apple.com/documentation/foundation/nsurlsession>);
/// moving `Retained<NSURLSession>` across Tokio worker threads is sound because
/// the retained Objective-C object is itself thread-safe.
#[derive(Clone)]
pub(crate) struct AppleSession {
    delegate: Retained<AppleSessionDelegate>,
    session: Retained<NSURLSession>,
    accept_encoding: String,
}

impl AppleSession {
    pub(crate) fn new(options: &NetOptions) -> Self {
        let delegate = AppleSessionDelegate::new(options.is_insecure);
        let configuration = NSURLSessionConfiguration::ephemeralSessionConfiguration();
        configuration.setTimeoutIntervalForRequest(options.inactivity_timeout.as_secs_f64());
        if let Some(total_timeout) = options.total_timeout {
            configuration.setTimeoutIntervalForResource(total_timeout.as_secs_f64());
        }
        if let Ok(limit) = NSInteger::try_from(options.pool_max_idle_per_host) {
            configuration.setHTTPMaximumConnectionsPerHost(limit);
        }
        let accept_encoding = accept_encoding_value(options.compression);

        let queue = NSOperationQueue::new();
        queue.setMaxConcurrentOperationCount(1);
        let delegate_obj: &AppleSessionDelegate = &delegate;
        let delegate_ref: &ProtocolObject<dyn NSURLSessionDelegate> =
            ProtocolObject::from_ref(delegate_obj);
        // SAFETY: `AppleSessionDelegate` implements `NSURLSessionDelegate` and
        // stores only Send/Sync Rust state. The delegate queue
        // is retained by the session and may invoke callbacks off the main thread.
        let session = unsafe {
            NSURLSession::sessionWithConfiguration_delegate_delegateQueue(
                &configuration,
                Some(delegate_ref),
                Some(&queue),
            )
        };

        Self {
            delegate,
            session,
            accept_encoding,
        }
    }

    pub(crate) fn data(
        &self,
        request: AppleRequest,
        cancel: CancelToken,
    ) -> impl Future<Output = NetResult<AppleDataResponse>> + Send {
        let started = self.start_data(request);
        async move {
            let (receiver, task) = started?;
            wait_for_data(receiver, task, cancel).await
        }
    }

    fn start_data(&self, request: AppleRequest) -> NetResult<DataStart> {
        let (sender, receiver) = oneshot::channel();
        let sender = Arc::new(Mutex::new(Some(sender)));
        let block_sender = Arc::clone(&sender);
        let completion = RcBlock::new(
            move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
                let result = completion_result(data, response, error);
                send_once(&block_sender, result);
            },
        );

        let request = request.into_ns_request(&self.accept_encoding)?;
        // SAFETY: The block captures only an Arc<Mutex<Option<oneshot::Sender<_>>>>,
        // which is Send + Sync. NSURLSession copies the completion block before
        // returning the task, so the local RcBlock can be dropped before await.
        let data_task = unsafe {
            self.session
                .dataTaskWithRequest_completionHandler(&request, &completion)
        };
        let task: AppleTask = data_task.into();
        task.resume();
        Ok((receiver, task))
    }

    fn start_stream(&self, request: AppleRequest) -> NetResult<StartedStream> {
        let (head_sender, head_receiver) = oneshot::channel();
        let (body_sender, body_receiver) = mpsc::channel(STREAM_BUFFER_CHUNKS);
        let terminal = Arc::new(Mutex::new(None));
        let request = request.into_ns_request(&self.accept_encoding)?;
        let data_task = self.session.dataTaskWithRequest(&request);
        let task: AppleTask = data_task.into();
        let task_id = task.id();
        self.delegate.register_stream(
            task_id,
            StreamState {
                terminal: Arc::clone(&terminal),
                body_sender: Some(body_sender),
                head_sender: Some(head_sender),
            },
        );
        task.resume();

        Ok(StartedStream {
            task,
            terminal,
            body_receiver,
            head_receiver,
            task_id,
        })
    }

    pub(crate) fn stream(
        &self,
        request: AppleRequest,
        cancel: CancelToken,
    ) -> impl Future<Output = NetResult<AppleStreamResponse>> + Send {
        let started = self.start_stream(request);
        let delegate = self.delegate.clone();
        async move {
            let started = started?;
            wait_for_stream_head(started, delegate, cancel).await
        }
    }
}

/// SAFETY: `AppleTask` wraps an `NSURLSessionDataTask` through its
/// `NSURLSessionTask` superclass. Apple documents URL session tasks as safe to
/// use from multiple threads; moving the retained task across await points is
/// sound under that Foundation contract.
#[derive(Clone)]
pub(super) struct AppleTask {
    task: Retained<NSURLSessionTask>,
}

impl From<Retained<NSURLSessionDataTask>> for AppleTask {
    fn from(task: Retained<NSURLSessionDataTask>) -> Self {
        Self {
            task: task.into_super(),
        }
    }
}

impl AppleTask {
    pub(super) fn cancel(&self) {
        self.task.cancel();
    }

    pub(super) fn id(&self) -> TaskId {
        self.task.taskIdentifier()
    }

    fn resume(&self) {
        self.task.resume();
    }
}
