use std::future::Future;

use kithara_apple::foundation::{
    ns::{
        NSOperationQueue, NSUInteger, NSURLSession, NSURLSessionConfiguration,
        NSURLSessionDataTask, NSURLSessionTask,
    },
    objc::rc::Retained,
    urlsession::{self, UrlSessionDelegate},
};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, OnceLock},
    tokio::sync::oneshot,
};

use super::{
    delegate::{AppleSessionEvents, StreamState, make_delegate},
    request::AppleRequest,
    stream::{AppleBodyQueue, AppleStreamResponse, StartedStream, wait_for_stream_head},
};
use crate::{
    backend::pooled::ByteBuffers,
    error::NetResult,
    metrics::ConnectionMetrics,
    types::{NetOptions, accept_encoding_value},
};

pub(super) type TaskId = NSUInteger;
#[derive(Clone, Debug, Eq, PartialEq, kithara_derive::Mirror)]
#[mirror(from_ref = NetOptions)]
struct SharedSessionKey {
    is_insecure: bool,
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

    fn start_stream(&self, request: AppleRequest) -> NetResult<StartedStream> {
        let (head_sender, head_receiver) = oneshot::channel();
        let accept_encoding = request.accept_encoding();
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
    use crate::NetError;

    #[kithara::test(native, flash(false))]
    fn idle_pool_capacity_leaves_the_native_connection_limit_unchanged() {
        let ephemeral_default = NSURLSessionConfiguration::ephemeralSessionConfiguration()
            .HTTPMaximumConnectionsPerHost();
        let first = shared_session(&NetOptions::builder().pool_max_idle_per_host(0).build());

        for idle in [0usize, 8, 40] {
            let options = NetOptions::builder().pool_max_idle_per_host(idle).build();
            let shared = shared_session(&options);
            let limit = shared
                .session
                .configuration()
                .HTTPMaximumConnectionsPerHost();
            assert_eq!(
                limit, ephemeral_default,
                "pool_max_idle_per_host={idle} must not change HTTPMaximumConnectionsPerHost; \
                 URLSession owns its connection pool and exposes no idle-pool bound"
            );
            assert_eq!(
                Retained::as_ptr(&shared.session),
                Retained::as_ptr(&first.session),
                "pool_max_idle_per_host={idle} must reuse the one NSURLSession of its TLS policy"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn secure_and_insecure_options_rekey_the_shared_session() {
        let secure = NetOptions::default();
        let insecure = NetOptions::builder().is_insecure(true).build();

        assert_ne!(
            SharedSessionKey::from(&secure),
            SharedSessionKey::from(&insecure),
            "is_insecure must still rekey the shared URLSession"
        );

        let secure_session = shared_session(&secure);
        let insecure_session = shared_session(&insecure);
        assert_ne!(
            Retained::as_ptr(&secure_session.session),
            Retained::as_ptr(&insecure_session.session),
            "secure and insecure options must get distinct NSURLSessions"
        );
        assert_eq!(
            Retained::as_ptr(&shared_session(&secure).session),
            Retained::as_ptr(&secure_session.session),
            "a repeated secure lookup must reuse its NSURLSession"
        );
        assert_eq!(
            Retained::as_ptr(&shared_session(&insecure).session),
            Retained::as_ptr(&insecure_session.session),
            "a repeated insecure lookup must reuse its NSURLSession"
        );
    }

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
