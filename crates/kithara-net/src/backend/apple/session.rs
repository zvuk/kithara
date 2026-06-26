#![allow(unsafe_code)]

use std::{future::Future, sync::Arc};

use block2::RcBlock;
use kithara_platform::{
    CancelToken,
    sync::{Mutex, OnceLock},
    time::Duration,
    tokio::sync::oneshot,
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
    stream::{
        AppleBodyQueue, AppleStreamResponse, StartedStream, wait_for_data, wait_for_stream_head,
    },
};
use crate::{
    error::{NetError, NetResult},
    metrics::ConnectionMetrics,
    types::NetOptions,
};

pub(super) type TaskId = NSUInteger;
type DataStart = (
    oneshot::Receiver<Result<AppleDataResponse, NetError>>,
    AppleTask,
);

#[derive(Clone, Debug, Eq, PartialEq)]
struct SharedSessionKey {
    total_timeout: Option<Duration>,
    max_connections_per_host: usize,
    is_insecure: bool,
}

#[derive(Clone)]
struct SharedSession {
    key: SharedSessionKey,
    delegate: Retained<AppleSessionDelegate>,
    session: Retained<NSURLSession>,
}

/// SAFETY: Apple documents `NSURLSession` as thread-safe
/// (<https://developer.apple.com/documentation/foundation/nsurlsession>);
/// moving `Retained<NSURLSession>` across Tokio worker threads is sound because
/// the retained Objective-C object is itself thread-safe.
#[derive(Clone)]
pub(crate) struct AppleSession {
    accept_encoding: String,
    body_queue_capacity: usize,
    body_queue_resume_at: usize,
    connection_metrics: ConnectionMetrics,
    shared: SharedSession,
}

impl AppleSession {
    pub(crate) fn new(options: &NetOptions, connection_metrics: ConnectionMetrics) -> Self {
        let shared = shared_session(options);
        let accept_encoding = accept_encoding_value(options.compression);

        Self {
            accept_encoding,
            body_queue_capacity: options.body_queue_capacity,
            body_queue_resume_at: options.body_queue_resume_at,
            connection_metrics,
            shared,
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
        // SAFETY: The block captures only an Arc<platform Mutex<Option<Sender>>>,
        // which is Send + Sync. NSURLSession copies the completion block before
        // returning the task, so the local RcBlock can be dropped before await.
        let data_task = unsafe {
            self.shared
                .session
                .dataTaskWithRequest_completionHandler(&request, &completion)
        };
        let task: AppleTask = data_task.into();
        self.shared
            .delegate
            .register_metrics(task.id(), self.connection_metrics.clone());
        task.resume();
        Ok((receiver, task))
    }

    fn start_stream(&self, request: AppleRequest) -> NetResult<StartedStream> {
        let (head_sender, head_receiver) = oneshot::channel();
        let request = request.into_ns_request(&self.accept_encoding)?;
        let data_task = self.shared.session.dataTaskWithRequest(&request);
        let task: AppleTask = data_task.into();
        let body_queue = Arc::new(AppleBodyQueue::new(
            task.clone(),
            self.body_queue_capacity,
            self.body_queue_resume_at,
        ));
        let task_id = task.id();
        self.shared
            .delegate
            .register_metrics(task_id, self.connection_metrics.clone());
        self.shared.delegate.register_stream(
            task_id,
            StreamState {
                body_queue: Some(Arc::clone(&body_queue)),
                head_sender: Some(head_sender),
            },
        );
        task.resume();

        Ok(StartedStream {
            body_queue,
            task,
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
        let delegate = self.shared.delegate.clone();
        async move {
            let started = started?;
            wait_for_stream_head(started, delegate, cancel).await
        }
    }
}

impl From<&NetOptions> for SharedSessionKey {
    fn from(options: &NetOptions) -> Self {
        Self {
            total_timeout: options.total_timeout,
            max_connections_per_host: options.pool_max_idle_per_host,
            is_insecure: options.is_insecure,
        }
    }
}

impl SharedSession {
    fn new(key: SharedSessionKey) -> Self {
        let delegate = AppleSessionDelegate::new(key.is_insecure);
        let configuration = NSURLSessionConfiguration::ephemeralSessionConfiguration();
        if let Some(total_timeout) = key.total_timeout {
            configuration.setTimeoutIntervalForResource(total_timeout.as_secs_f64());
        }
        if let Ok(max_connections) = NSInteger::try_from(key.max_connections_per_host)
            && max_connections > 0
        {
            configuration.setHTTPMaximumConnectionsPerHost(max_connections);
        }

        let queue = NSOperationQueue::new();
        queue.setMaxConcurrentOperationCount(1);
        let delegate_obj: &AppleSessionDelegate = &delegate;
        let delegate_ref: &ProtocolObject<dyn NSURLSessionDelegate> =
            ProtocolObject::from_ref(delegate_obj);
        // SAFETY: `AppleSessionDelegate` implements `NSURLSessionDelegate` and
        // stores only Send/Sync Rust state. The delegate queue is retained by
        // the session and may invoke callbacks off the main thread.
        let session = unsafe {
            NSURLSession::sessionWithConfiguration_delegate_delegateQueue(
                &configuration,
                Some(delegate_ref),
                Some(&queue),
            )
        };

        Self {
            key,
            delegate,
            session,
        }
    }
}

fn shared_session(options: &NetOptions) -> SharedSession {
    static SESSIONS: OnceLock<Mutex<Vec<SharedSession>>> = OnceLock::new();

    let key = SharedSessionKey::from(options);
    let sessions = SESSIONS.get_or_init(Mutex::default);
    let mut sessions = sessions.lock();
    if let Some(session) = sessions.iter().find(|session| session.key == key) {
        return session.clone();
    }

    let session = SharedSession::new(key);
    sessions.push(session.clone());
    session
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

    pub(super) fn resume(&self) {
        self.task.resume();
    }

    pub(super) fn suspend(&self) {
        self.task.suspend();
    }
}
