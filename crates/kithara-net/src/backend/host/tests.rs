mod kithara {
    pub(crate) use kithara_test_macros::test;
}

use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::atomic::{AtomicU32, Ordering},
};

use bytes::Bytes;
use futures::TryStreamExt;
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use url::Url;

use super::{
    AlreadyInstalled, HostBuffer, HostCall, HostEvents, HostFailure, HostMethod, HostRequest,
    HostTransport, HttpClient, call::READ_SIZE, install,
};
use crate::{
    error::NetError,
    test_pools::pools,
    types::{Headers, NetOptions, RangeSpec, RetryPolicy},
};

#[derive(Clone, Copy, Debug)]
enum OnStart {
    Respond,
    Fail,
    Refuse,
    EndBeforeResponse,
}

#[derive(Clone, Copy, Debug)]
enum OnRead {
    Serve,
    Hold,
    RespondAgain,
    ReportViolation,
    ServeThenReportAgain,
    Overfill,
}

#[derive(Debug)]
struct Plan {
    on_start: OnStart,
    on_read: OnRead,
    status: u16,
    headers: Vec<(String, String)>,
    body: Bytes,
}

impl Plan {
    fn serving(body: impl Into<Bytes>) -> Self {
        Self {
            on_start: OnStart::Respond,
            on_read: OnRead::Serve,
            status: 200,
            headers: Vec::new(),
            body: body.into(),
        }
    }
}

struct Recorded {
    method: HostMethod,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

#[derive(Default)]
struct Seen {
    starts: AtomicU32,
    cancels: AtomicU32,
    requests: Mutex<Vec<Recorded>>,
}

/// One test's plan and what its transport saw.
struct Route {
    plan: Arc<Plan>,
    seen: Arc<Seen>,
}

/// The transport the test process installs once; each test serves its plan
/// under a URL of its own, so tests sharing a process stay apart.
#[derive(Default)]
struct Router {
    routes: Mutex<HashMap<Url, Route>>,
    next: AtomicU32,
}

impl Router {
    fn installed() -> &'static Arc<Self> {
        static ROUTER: OnceLock<Arc<Router>> = OnceLock::new();
        ROUTER.get_or_init(|| {
            let router = Arc::new(Self::default());
            install(Arc::clone(&router) as Arc<dyn HostTransport>)
                .expect("BUG: the router is the test process's only transport");
            router
        })
    }

    fn route(&self, plan: Plan) -> (Url, Arc<Seen>) {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let url = Url::parse(&format!("http://127.0.0.1/probe/{id}"))
            .expect("BUG: a numbered test URL is valid");
        let seen = Arc::new(Seen::default());
        self.routes.lock().insert(
            url.clone(),
            Route {
                plan: Arc::new(plan),
                seen: Arc::clone(&seen),
            },
        );
        (url, seen)
    }
}

impl fmt::Debug for Router {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Router")
            .field("routes", &self.routes.lock().len())
            .finish_non_exhaustive()
    }
}

impl HostTransport for Router {
    fn start(
        &self,
        request: HostRequest,
        events: HostEvents,
    ) -> Result<Box<dyn HostCall>, HostFailure> {
        let (plan, seen) = self
            .routes
            .lock()
            .get(&request.url)
            .map(|route| (Arc::clone(&route.plan), Arc::clone(&route.seen)))
            .ok_or_else(|| HostFailure::Protocol(format!("no route for {}", request.url)))?;
        seen.starts.fetch_add(1, Ordering::SeqCst);
        seen.requests.lock().push(Recorded {
            method: request.method,
            headers: request.headers,
            body: request.body.map(|body| body.as_ref().to_vec()),
        });
        match plan.on_start {
            OnStart::Respond => events.response(plan.status, plan.headers.clone()),
            OnStart::Fail => events.fail(HostFailure::Transport("connection reset".to_owned())),
            OnStart::Refuse => events.fail(HostFailure::Permanent(
                "cleartext not permitted by network security policy".to_owned(),
            )),
            OnStart::EndBeforeResponse => events.end(),
        }
        Ok(Box::new(FakeCall {
            events,
            plan,
            seen,
            offset: Mutex::new(0),
            held: Mutex::new(None),
        }))
    }
}

struct FakeCall {
    events: HostEvents,
    plan: Arc<Plan>,
    seen: Arc<Seen>,
    offset: Mutex<usize>,
    held: Mutex<Option<HostBuffer>>,
}

impl FakeCall {
    fn serve(&self, mut buffer: HostBuffer) -> bool {
        let len = {
            let mut offset = self.offset.lock();
            let rest = &self.plan.body[*offset..];
            let len = rest.len().min(buffer.as_ref().len());
            buffer.as_mut()[..len].copy_from_slice(&rest[..len]);
            *offset += len;
            len
        };
        if len == 0 {
            self.events.end();
            return false;
        }
        self.events.read(buffer, len);
        true
    }
}

impl HostCall for FakeCall {
    fn read(&self, buffer: HostBuffer) {
        match self.plan.on_read {
            OnRead::Serve => {
                self.serve(buffer);
            }
            OnRead::Hold => *self.held.lock() = Some(buffer),
            OnRead::RespondAgain => self.events.response(200, Vec::new()),
            OnRead::ReportViolation => self
                .events
                .fail(HostFailure::Protocol("a read of -2 bytes".to_owned())),
            OnRead::Overfill => {
                let len = buffer.as_ref().len() + 1;
                self.events.read(buffer, len);
            }
            OnRead::ServeThenReportAgain => {
                if !self.serve(buffer) {
                    self.events.fail(HostFailure::Transport("late".to_owned()));
                    self.events.end();
                    self.events.response(500, Vec::new());
                }
            }
        }
    }

    fn cancel(&self) {
        self.seen.cancels.fetch_add(1, Ordering::SeqCst);
    }
}

fn retry_policy(max_retries: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
    }
}

fn client(plan: Plan) -> (HttpClient, Arc<Seen>, Url) {
    client_with(plan, CancelToken::never())
}

fn client_with(plan: Plan, cancel: CancelToken) -> (HttpClient, Arc<Seen>, Url) {
    let (url, seen) = Router::installed().route(plan);
    let options = NetOptions::builder().retry_policy(retry_policy(2)).build();
    (HttpClient::new(options, pools(), cancel), seen, url)
}

fn pattern(len: usize) -> Bytes {
    (0..len)
        .map(|index| u8::try_from(index % 251).expect("BUG: a residue of 251 fits a byte"))
        .collect()
}

fn sent_headers(seen: &Seen) -> HashSet<(String, String)> {
    seen.requests.lock()[0].headers.iter().cloned().collect()
}

fn identity() -> (String, String) {
    ("Accept-Encoding".to_owned(), "identity".to_owned())
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_body_larger_than_several_reads_arrives_byte_identical() {
    let body = pattern(READ_SIZE * 3 + READ_SIZE / 2);
    let (client, _, url) = client(Plan::serving(body.clone()));

    let streamed: Vec<Bytes> = client
        .stream(url.clone(), None)
        .await
        .expect("the response opens")
        .into_inner()
        .try_collect()
        .await
        .expect("the body arrives");
    let whole = client.get_bytes(url, None).await.expect("the body arrives");

    assert_eq!(Bytes::from(streamed.concat()), body);
    assert_eq!(whole, body);
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_body_before_the_response_is_a_fatal_violation() {
    let (client, seen, url) = client(Plan {
        on_start: OnStart::EndBeforeResponse,
        ..Plan::serving(Bytes::new())
    });

    let error = client
        .get_bytes(url, None)
        .await
        .expect_err("a body cannot precede its response");

    assert!(matches!(error, NetError::Protocol(_)), "got {error:?}");
    assert_eq!(
        seen.starts.load(Ordering::SeqCst),
        1,
        "a violation is not retried"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_second_response_is_a_fatal_violation() {
    let (client, seen, url) = client(Plan {
        on_read: OnRead::RespondAgain,
        ..Plan::serving(Bytes::new())
    });

    let error = client
        .get_bytes(url, None)
        .await
        .expect_err("one call has one response");

    assert!(matches!(error, NetError::Protocol(_)), "got {error:?}");
    assert_eq!(
        seen.starts.load(Ordering::SeqCst),
        1,
        "a violation is not retried"
    );
    assert_eq!(
        seen.cancels.load(Ordering::SeqCst),
        1,
        "a violating call is stopped"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_reported_violation_cancels_the_call_and_is_not_retried() {
    let (client, seen, url) = client(Plan {
        on_read: OnRead::ReportViolation,
        ..Plan::serving(Bytes::new())
    });

    let error = client
        .get_bytes(url, None)
        .await
        .expect_err("the transport declared the call broken");

    assert!(matches!(error, NetError::Protocol(_)), "got {error:?}");
    assert_eq!(seen.starts.load(Ordering::SeqCst), 1);
    assert_eq!(seen.cancels.load(Ordering::SeqCst), 1);
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_read_longer_than_the_lent_buffer_is_a_fatal_violation() {
    let (client, seen, url) = client(Plan {
        on_read: OnRead::Overfill,
        ..Plan::serving(Bytes::new())
    });

    let error = client
        .get_bytes(url, None)
        .await
        .expect_err("a read cannot outgrow its buffer");

    assert!(matches!(error, NetError::Protocol(_)), "got {error:?}");
    assert_eq!(seen.starts.load(Ordering::SeqCst), 1);
    assert_eq!(seen.cancels.load(Ordering::SeqCst), 1);
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn reports_after_the_terminal_one_change_nothing() {
    let body = pattern(READ_SIZE + 7);
    let (client, _, url) = client(Plan {
        on_read: OnRead::ServeThenReportAgain,
        ..Plan::serving(body.clone())
    });

    let whole = client
        .get_bytes(url, None)
        .await
        .expect("the first terminal report settles the call");

    assert_eq!(whole, body);
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_transport_failure_is_retried_per_policy() {
    let (client, seen, url) = client(Plan {
        on_start: OnStart::Fail,
        ..Plan::serving(Bytes::new())
    });

    let error = client
        .get_bytes(url, None)
        .await
        .expect_err("every attempt fails");

    assert!(
        matches!(&error, NetError::RetryExhausted { source, .. } if matches!(**source, NetError::Network(_))),
        "got {error:?}"
    );
    assert_eq!(
        seen.starts.load(Ordering::SeqCst),
        3,
        "one attempt and two retries"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_permanent_failure_is_attempted_once() {
    let (client, seen, url) = client(Plan {
        on_start: OnStart::Refuse,
        ..Plan::serving(Bytes::new())
    });

    let error = client
        .get_bytes(url, None)
        .await
        .expect_err("the host refused the request");

    assert!(matches!(error, NetError::Refused(_)), "got {error:?}");
    assert_eq!(seen.starts.load(Ordering::SeqCst), 1);
    assert_eq!(
        seen.cancels.load(Ordering::SeqCst),
        0,
        "the transport ended the call"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_client_error_is_not_retried() {
    let (client, seen, url) = client(Plan {
        status: 404,
        ..Plan::serving(Bytes::from_static(b"gone"))
    });

    let error = client
        .get_bytes(url, None)
        .await
        .expect_err("404 is not a body");

    assert!(
        matches!(&error, NetError::Status { status, .. } if status.get() == 404),
        "got {error:?}"
    );
    assert_eq!(seen.starts.load(Ordering::SeqCst), 1);
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn dropping_an_unfinished_body_cancels_the_call() {
    let (client, seen, url) = client(Plan {
        on_read: OnRead::Hold,
        ..Plan::serving(Bytes::new())
    });
    let mut body = client
        .stream(url, None)
        .await
        .expect("the response opens")
        .into_inner();

    let pending = futures::poll!(body.try_next());
    assert!(pending.is_pending(), "the transport holds the read");
    drop(body);

    assert_eq!(seen.cancels.load(Ordering::SeqCst), 1);
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_cancelled_and_dropped_body_cancels_the_call_once() {
    let cancel = CancelToken::root();
    let (client, seen, url) = client_with(
        Plan {
            on_read: OnRead::Hold,
            ..Plan::serving(Bytes::new())
        },
        cancel.clone(),
    );
    let mut body = client
        .stream(url, None)
        .await
        .expect("the response opens")
        .into_inner();
    let pending = futures::poll!(body.try_next());
    assert!(pending.is_pending(), "the transport holds the read");

    cancel.cancel();
    let error = body
        .try_next()
        .await
        .expect_err("a cancelled body ends in the cancel");
    drop(body);

    assert!(matches!(error, NetError::Cancelled), "got {error:?}");
    assert_eq!(seen.cancels.load(Ordering::SeqCst), 1);
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn byte_addressed_requests_ask_for_identity() {
    let (head, head_seen, head_url) = client(Plan::serving(Bytes::new()));
    head.head(head_url, None).await.expect("HEAD answers");

    let (range, range_seen, range_url) = client(Plan {
        status: 206,
        headers: vec![
            ("Content-Range".to_owned(), "bytes 2-5/10".to_owned()),
            ("Content-Length".to_owned(), "4".to_owned()),
        ],
        ..Plan::serving(Bytes::from_static(b"2345"))
    });
    range
        .get_range(range_url, RangeSpec::new(2, Some(5)), None)
        .await
        .expect("the range opens");

    let (whole, whole_seen, whole_url) = client(Plan::serving(Bytes::new()));
    whole.get_bytes(whole_url, None).await.expect("GET answers");

    assert!(sent_headers(&head_seen).contains(&identity()));
    let range_headers = sent_headers(&range_seen);
    assert!(range_headers.contains(&identity()));
    assert!(range_headers.contains(&("Range".to_owned(), "bytes=2-5".to_owned())));
    assert!(
        !sent_headers(&whole_seen).contains(&identity()),
        "a whole body leaves the coding to the host's client"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_surviving_content_encoding_is_rejected() {
    let (client, seen, url) = client(Plan {
        headers: vec![("Content-Encoding".to_owned(), "gzip".to_owned())],
        ..Plan::serving(Bytes::from_static(b"\x1f\x8b"))
    });

    let opened = client.stream(url, None).await;

    assert!(
        matches!(opened, Err(NetError::Decode(_))),
        "encoded bytes must not reach the caller"
    );
    assert_eq!(
        seen.starts.load(Ordering::SeqCst),
        1,
        "a coding is not retried"
    );
    assert_eq!(
        seen.cancels.load(Ordering::SeqCst),
        1,
        "the rejected call is stopped"
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_post_body_arrives_intact() {
    let (client, seen, url) = client(Plan::serving(Bytes::from_static(b"key")));
    let mut headers = Headers::default();
    headers.insert("X-Salt", "pepper");

    let answer = client
        .post_bytes(url, Bytes::from_static(b"request"), Some(headers))
        .await
        .expect("POST answers");

    let request = seen.requests.lock().remove(0);
    assert_eq!(answer, Bytes::from_static(b"key"));
    assert_eq!(request.method, HostMethod::Post);
    assert_eq!(request.body.as_deref(), Some(&b"request"[..]));
    assert!(
        request
            .headers
            .contains(&("X-Salt".to_owned(), "pepper".to_owned()))
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_second_transport_is_refused_and_the_first_keeps_serving() {
    let (client, seen, url) = client(Plan::serving(Bytes::from_static(b"first")));

    // An empty router: a request it served would fail for want of a route.
    let second = install(Arc::new(Router::default()) as Arc<dyn HostTransport>);
    let answer = client
        .get_bytes(url, None)
        .await
        .expect("the first transport answers");

    assert!(matches!(second, Err(AlreadyInstalled)), "got {second:?}");
    assert_eq!(answer, Bytes::from_static(b"first"));
    assert_eq!(seen.starts.load(Ordering::SeqCst), 1);
}
