use std::future::Future;

use kithara_apple::foundation::{
    ns::{
        NSInteger, NSOperationQueue, NSUInteger, NSURLSession, NSURLSessionConfiguration,
        NSURLSessionDataTask, NSURLSessionTask,
    },
    objc::rc::Retained,
    urlsession::{self, DataCompletion, UrlSessionDelegate},
};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, OnceLock},
    tokio::sync::oneshot,
};
use url::Url;

use super::{
    delegate::{AppleSessionEvents, StreamState, make_delegate},
    request::AppleRequest,
    response::{AppleDataResponse, completion_result, send_once},
    stream::{
        AppleBodyQueue, AppleStreamResponse, StartedStream, wait_for_data, wait_for_stream_head,
    },
};
use crate::{
    backend::pooled::ByteBuffers,
    error::{NetError, NetResult},
    metrics::ConnectionMetrics,
    types::{AcceptEncodingPolicy, NetOptions, accept_encoding_value},
};

pub(super) type TaskId = NSUInteger;
type DataResult = Result<AppleDataResponse, NetError>;
type DataSender = Arc<Mutex<Option<oneshot::Sender<DataResult>>>>;
type DataStart = (oneshot::Receiver<DataResult>, AppleTask);

struct DataCompletionSink {
    accept_encoding: AcceptEncodingPolicy,
    buffers: ByteBuffers,
    sender: DataSender,
    url: Url,
}

impl urlsession::DataTaskCompletion for DataCompletionSink {
    fn complete(&self, completion: DataCompletion<'_>) {
        let result = completion_result(&completion, &self.buffers, self.accept_encoding, &self.url);
        send_once(&self.sender, result);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, kithara_derive::Mirror)]
#[mirror(from_ref = NetOptions)]
struct SharedSessionKey {
    is_insecure: bool,
    #[mirror(rename = pool_max_idle_per_host)]
    max_connections_per_host: usize,
}

#[derive(Clone)]
struct SharedSession {
    events: Arc<AppleSessionEvents>,
    _delegate: Retained<UrlSessionDelegate>,
    session: Retained<NSURLSession>,
    key: SharedSessionKey,
}

#[derive(Clone)]
pub(crate) struct AppleSession {
    buffers: ByteBuffers,
    connection_metrics: ConnectionMetrics,
    shared: SharedSession,
    accept_encoding: String,
    body_queue_capacity: usize,
    body_queue_resume_at: usize,
}

impl AppleSession {
    pub(crate) fn new<S>(
        options: &NetOptions,
        pools: PoolRegion<S>,
        connection_metrics: ConnectionMetrics,
    ) -> Self
    where
        S: HasPool<u8> + Send + Sync + 'static,
    {
        let shared = shared_session(options);
        let accept_encoding = accept_encoding_value(options.compression);

        Self {
            accept_encoding,
            connection_metrics,
            shared,
            buffers: ByteBuffers::new(pools),
            body_queue_capacity: options.body_queue_capacity,
            body_queue_resume_at: options.body_queue_resume_at,
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
        let accept_encoding = request.accept_encoding();
        let url = request.url().clone();
        let completion: Arc<dyn urlsession::DataTaskCompletion> = Arc::new(DataCompletionSink {
            accept_encoding,
            url,
            buffers: self.buffers.clone(),
            sender: Arc::clone(&sender),
        });

        let request = request.into_ns_request(&self.accept_encoding)?;
        let data_task =
            urlsession::data_task_with_completion(&self.shared.session, &request, completion);
        let task: AppleTask = data_task.into();
        self.shared
            .events
            .register_metrics(task.id(), self.connection_metrics.clone());
        task.resume();
        Ok((receiver, task))
    }

    fn start_stream(&self, request: AppleRequest) -> NetResult<StartedStream> {
        let (head_sender, head_receiver) = oneshot::channel();
        let accept_encoding = request.accept_encoding();
        let url = request.url().clone();
        let request = request.into_ns_request(&self.accept_encoding)?;
        let data_task = self.shared.session.dataTaskWithRequest(&request);
        let task: AppleTask = data_task.into();
        let body_queue = Arc::new(AppleBodyQueue::new(
            task.clone(),
            self.body_queue_capacity,
            self.body_queue_resume_at,
            self.buffers.clone(),
        ));
        let task_id = task.id();
        self.shared
            .events
            .register_metrics(task_id, self.connection_metrics.clone());
        self.shared.events.register_stream(
            task_id,
            StreamState {
                accept_encoding,
                url,
                body_queue: Some(Arc::clone(&body_queue)),
                head_sender: Some(head_sender),
            },
        );
        task.resume();

        Ok(StartedStream {
            task,
            body_queue,
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
        let events = Arc::clone(&self.shared.events);
        async move {
            let started = started?;
            wait_for_stream_head(started, events, cancel).await
        }
    }
}

impl SharedSession {
    fn new(key: SharedSessionKey) -> Self {
        let events = AppleSessionEvents::new(key.is_insecure);
        let delegate = make_delegate(&events);
        let configuration = NSURLSessionConfiguration::ephemeralSessionConfiguration();
        if let Ok(max_connections) = NSInteger::try_from(key.max_connections_per_host)
            && max_connections > 0
        {
            configuration.setHTTPMaximumConnectionsPerHost(max_connections);
        }

        let queue = NSOperationQueue::new();
        queue.setMaxConcurrentOperationCount(1);
        let session = urlsession::session_with_delegate(&configuration, &delegate, &queue);

        Self {
            events,
            session,
            key,
            _delegate: delegate,
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
    delegate::delegate! {
        to self.task {
            pub(super) fn cancel(&self);
            #[call(taskIdentifier)]
            pub(super) fn id(&self) -> TaskId;
            pub(super) fn resume(&self);
            pub(super) fn suspend(&self);
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_apple::foundation::ns::NSData;
    use kithara_test_utils::kithara;

    use super::{super::response::copy_data, *};

    #[kithara::test(native, flash(false))]
    fn configured_byte_pool_reaches_response_copy_path() {
        let pools = crate::test_pools::pools_with_budget(64);
        let options = NetOptions::default();
        let session = AppleSession::new(&options, pools.clone(), ConnectionMetrics::default());
        let data = NSData::with_bytes(b"abc");

        let bytes = copy_data(&data, &session.buffers).expect("copy into configured pool");
        assert_eq!(&bytes[..], b"abc");
        assert!(pools.stats().allocated_bytes > 0);
        let ptr = bytes.as_ptr();
        drop(bytes);

        let bytes = copy_data(&data, &session.buffers).expect("reuse configured pool");
        assert_eq!(&bytes[..], b"abc");
        assert_eq!(bytes.as_ptr(), ptr);
    }

    #[kithara::test(native, flash(false))]
    fn response_copy_reports_byte_budget_exhaustion() {
        let buffers = ByteBuffers::new(crate::test_pools::pools_with_budget(1));
        let data = NSData::with_bytes(b"ab");

        let error = copy_data(&data, &buffers).expect_err("budget must reject two-byte body");
        assert!(matches!(error, NetError::Network(message) if message.contains("budget")));
    }
}
