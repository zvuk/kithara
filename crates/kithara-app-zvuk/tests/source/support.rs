use std::{
    any::Any,
    collections::{HashMap, VecDeque},
    fmt, io,
    num::NonZeroU16,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use async_trait::async_trait;
use bytes::Bytes;
use keyring_core::{Entry, Error, api::CredentialStoreApi, mock};
use kithara_app_library::{KeyAccess, LibrarySource, OpenUrlError, PageStatus, Secrets};
use kithara_app_zvuk::{Client, Config, Source};
use kithara_net::{ByteStream, Headers, Net, NetError, RangeSpec};
use kithara_platform::{
    CancelToken,
    time::{self, Duration, Instant},
    tokio::runtime::Handle,
};
use kithara_test_utils::wait_until;
use kithara_ui::{
    module::IconName,
    render::{ReadValue, TableValue, WriteValue},
};
use serde_json::{Value, json};
use unimock::Unimock;
use url::Url;

pub(super) fn search_reply() -> Bytes {
    Bytes::from_static(include_bytes!("../fixtures/search.json"))
}

pub(super) fn stream_reply() -> Bytes {
    Bytes::from_static(include_bytes!("../fixtures/streams.json"))
}

/// A liked page of the search fixture's tracks, each marked liked.
pub(super) fn liked_reply() -> Bytes {
    let mut tracks = search_tracks();
    for track in tracks
        .as_array_mut()
        .expect("the search fixture lists tracks")
    {
        track["collectionItemData"]["itemStatus"] = "liked".into();
    }
    Bytes::from(
        json!({"data": {
            "collectionCount": {"tracks": 153},
            "paginatedCollection": {"tracks": {"items": tracks, "page": {"hasNextPage": true}}},
        }})
        .to_string(),
    )
}

/// The first page of playlist 1023, the search fixture's tracks of 215.
pub(super) fn playlist_reply() -> Bytes {
    Bytes::from(
        json!({"data": {"playlists": [{"id": "1023", "trackCount": 215, "tracks": search_tracks()}]}})
            .to_string(),
    )
}

fn search_tracks() -> Value {
    let mut search: Value =
        serde_json::from_slice(&search_reply()).expect("the search fixture parses");
    search["data"]["search"]["tracks"]["items"].take()
}

pub(super) const TOKEN: &str = "synthetic-account-token";
pub(super) const AUTH_HEADER: &str = "X-Auth-Token";
pub(super) const PAGE: &str = "https://id.example.com/?user=SYN123";
pub(super) const LABEL: &str = "Synthetic Listener";

/// The account's request paths.
pub(super) mod paths {
    pub(crate) const SESSION: &str = "/api/tiny/login/qr/session";
    pub(crate) const TOKEN: &str = "/api/tiny/login/qr/token";
    pub(crate) const PROFILE: &str = "/api/v2/tiny/profile";
    pub(crate) const LOGOUT: &str = "/api/tiny/logout";
}

/// The failures the account row shows.
pub(super) mod faults {
    pub(crate) const SESSION: &str = "Zvuk refused the sign-in request";
    pub(crate) const BROWSER: &str = "the browser did not open the sign-in page";
    pub(crate) const STORE: &str = "the secret store failed";
    pub(crate) const REJECTED: &str = "Zvuk rejected the authentication token";
}

pub(super) fn session(expires_in: u64) -> Bytes {
    Bytes::from(
        json!({"result": {
            "device_code": "synthetic-device-code",
            "user_code": "SYN123",
            "expires_in": expires_in,
            "qr_authorization_url": PAGE,
        }})
        .to_string(),
    )
}

/// A poll reply: Zvuk confirmed the code with `token`, or still waits.
pub(super) fn poll(token: Option<&str>) -> Bytes {
    let status = if token.is_some() {
        "success"
    } else {
        "pending"
    };
    Bytes::from(
        json!({"result": {"status": status, "access_token": token, "refresh_token": token}})
            .to_string(),
    )
}

pub(super) fn status(code: u16) -> NetError {
    NetError::Status {
        status: NonZeroU16::new(code).expect("a status code is not zero"),
        url: None,
        body: None,
    }
}

pub(super) fn memory() -> Secrets {
    Secrets::new(mock::Store::new())
}

/// A store that failed to open.
pub(super) fn refusing() -> Secrets {
    Secrets::new(Err::<Arc<mock::Store>, _>(Error::NoStorageAccess(
        Box::new(io::Error::other("synthetic store refusal")),
    )))
}

fn built<N: Net + Clone + 'static>(
    net: N,
    secrets: Secrets,
    open: impl Fn(&Url) -> Result<(), OpenUrlError> + Send + Sync + 'static,
    cancel: &CancelToken,
) -> (Box<dyn LibrarySource>, KeyAccess) {
    let config: Config = serde_json::from_value(json!({"user_agent": "synthetic-agent"}))
        .expect("the entry names the client identity");
    let registration = Source::registered()
        .client(Client::with_transports(net.clone(), net, &config))
        .secrets(secrets)
        .open(Arc::new(open))
        .runtime(&Handle::current())
        .cancel(cancel.clone())
        .call()
        .expect("the source documents parse");
    let grant = registration
        .granted()
        .cloned()
        .expect("the source grants key access");
    let text = registration.page().texts[0];
    let words =
        kithara_ui::text::parse_text(text.text, &kithara_ui::ids::SourceUri(text.path.to_owned()))
            .expect("the source's captions parse");
    let source = registration
        .build(&words)
        .expect("the source's captions word every label");
    (source, grant)
}

/// A source whose page follows the stored token [`TOKEN`].
pub(super) async fn registered(net: DelayedNet, cancel: &CancelToken) -> Box<dyn LibrarySource> {
    let secrets = memory();
    secrets.set("zvuk", TOKEN).expect("the store writes");
    let mut source = built(net, secrets, |_: &Url| Ok(()), cancel).0;
    until(&mut *source, PageStatus::Empty).await;
    source
}

pub(super) async fn source(net: DelayedNet) -> Box<dyn LibrarySource> {
    registered(net, &kithara_test_utils::cancel_token()).await
}

/// Ticks the source until its page reaches `status`.
pub(super) async fn until(source: &mut dyn LibrarySource, status: PageStatus<'_>) {
    wait_until(
        Duration::from_secs(2),
        "the page reaches its status",
        || {
            source.tick();
            source.status() == status
        },
    )
    .await
    .expect("the source must reach the awaited page status");
}

/// A source showing the `needle` search's fixture rows.
pub(super) async fn loaded_search(net: DelayedNet) -> Box<dyn LibrarySource> {
    let mut source = source(net).await;
    source.write("query", &WriteValue::Text("needle".into()));
    time::sleep(Duration::from_millis(300)).await;
    until(&mut *source, PageStatus::Ready).await;
    source
}

/// Starts a search and leaves its catalogue completion queued for the next tick.
pub(super) async fn queue_catalogue(
    source: &mut dyn LibrarySource,
    net: &DelayedNet,
    query: &str,
    events: usize,
) {
    source.write("query", &WriteValue::Text(query.into()));
    time::sleep(Duration::from_millis(300)).await;
    source.tick();
    net.reaches_events(events).await;
}

/// Drives the catalogue-to-stream handoff, leaving the last completion queued.
pub(super) async fn queue_completed(
    source: &mut dyn LibrarySource,
    net: &DelayedNet,
    query: &str,
    events: usize,
) {
    source.write("query", &WriteValue::Text(query.into()));
    time::sleep(Duration::from_millis(300)).await;
    source.tick();
    wait_until(Duration::from_secs(2), "the stream completes", || {
        if net.events().len() < events {
            source.tick();
        }
        net.events().len() == events
    })
    .await
    .expect("the catalogue and stream must complete");
}

/// Waits until the source's tasks released their clones of `log`, so the
/// log is final and the mock verifies on the test thread.
async fn released<T>(log: &Arc<T>) {
    wait_until(
        Duration::from_secs(2),
        "the source releases its transport",
        || Arc::strong_count(log) == 1,
    )
    .await
    .expect("the source tasks must end with their cancellation");
}

/// Drops the source and waits until its tasks release the transport, so the
/// event log is final and the mock verifies on the test thread.
pub(super) async fn release(source: Box<dyn LibrarySource>, net: &DelayedNet) {
    drop(source);
    released(&net.events).await;
}

/// The reaction state of every row of track `id`.
pub(super) fn reactions(source: &dyn LibrarySource, id: &str) -> Vec<bool> {
    source
        .rows(None)
        .iter()
        .flat_map(|row| row.cells().iter().filter(|cell| cell.action() == Some(id)))
        .map(|cell| {
            let TableValue::Icon { icon, active, .. } = cell.value() else {
                panic!("a reaction cell shows an icon");
            };
            assert_eq!(
                *icon,
                if *active {
                    IconName::HeartFilled
                } else {
                    IconName::Heart
                }
            );
            *active
        })
        .collect()
}

#[derive(Clone, Debug)]
pub(super) struct Request {
    pub(super) path: String,
    pub(super) token: Option<String>,
    pub(super) device_id: Option<String>,
    pub(super) at: Instant,
}

/// Logs each catalogue POST's start and end and holds a `blocked` search or
/// one started under [`DelayedNet::hold`]; account requests skip the mock.
#[derive(Clone)]
pub(super) struct DelayedNet {
    net: Unimock,
    events: Arc<Mutex<Vec<String>>>,
    requests: Arc<Mutex<Vec<Request>>>,
    sessions: Arc<Mutex<VecDeque<Result<Bytes, u16>>>>,
    polls: Arc<Mutex<VecDeque<Bytes>>>,
    autocomplete: Arc<Mutex<VecDeque<Result<Bytes, u16>>>>,
    holding: Arc<AtomicBool>,
    hold_streams: Arc<AtomicBool>,
    stream_waiting: Arc<AtomicBool>,
    hold_reactions: Arc<AtomicBool>,
    reaction_waiting: Arc<AtomicBool>,
    parked: Arc<AtomicBool>,
    logout_held: Arc<AtomicBool>,
}

impl DelayedNet {
    pub(super) fn new(net: Unimock) -> Self {
        Self {
            net,
            events: Arc::default(),
            requests: Arc::default(),
            sessions: Arc::default(),
            polls: Arc::default(),
            autocomplete: Arc::default(),
            holding: Arc::default(),
            hold_streams: Arc::default(),
            stream_waiting: Arc::default(),
            hold_reactions: Arc::default(),
            reaction_waiting: Arc::default(),
            parked: Arc::default(),
            logout_held: Arc::default(),
        }
    }

    /// Replies to the next sign-in requests; then a 300 s session.
    pub(super) fn sessions(&self, replies: impl IntoIterator<Item = Result<Bytes, u16>>) {
        self.sessions
            .lock()
            .expect("the script is intact")
            .extend(replies);
    }

    /// Replies to the next polls; then the code stays pending.
    pub(super) fn polls(&self, replies: impl IntoIterator<Item = Bytes>) {
        self.polls
            .lock()
            .expect("the script is intact")
            .extend(replies);
    }

    pub(super) fn autocomplete(&self, replies: impl IntoIterator<Item = Result<Bytes, u16>>) {
        self.autocomplete
            .lock()
            .expect("the autocomplete script is intact")
            .extend(replies);
    }

    pub(super) fn hold(&self, on: bool) {
        self.holding.store(on, Ordering::SeqCst);
    }

    pub(super) fn hold_streams(&self, on: bool) {
        self.hold_streams.store(on, Ordering::SeqCst);
    }

    pub(super) fn stream_waiting(&self) -> bool {
        self.stream_waiting.load(Ordering::SeqCst)
    }

    pub(super) fn hold_reactions(&self, on: bool) {
        self.hold_reactions.store(on, Ordering::SeqCst);
    }

    pub(super) fn reaction_waiting(&self) -> bool {
        self.reaction_waiting.load(Ordering::SeqCst)
    }

    /// While on, every catalogue request but a playlist listing waits,
    /// unlogged.
    pub(super) fn park(&self, on: bool) {
        self.parked.store(on, Ordering::SeqCst);
    }

    /// Leaves every logout unanswered.
    pub(super) fn hold_logout(&self) {
        self.logout_held.store(true, Ordering::SeqCst);
    }

    pub(super) fn events(&self) -> Vec<String> {
        self.events
            .lock()
            .expect("request event recording must not panic")
            .clone()
    }

    pub(super) fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .expect("the request log is intact")
            .clone()
    }

    pub(super) fn paths(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|request| request.path)
            .collect()
    }

    pub(super) async fn reaches(&self, count: usize) {
        wait_until(
            Duration::from_secs(6),
            "the account sends the awaited requests",
            || self.paths().len() >= count,
        )
        .await
        .expect("the account sends the awaited requests in time");
    }

    pub(super) async fn reaches_events(&self, count: usize) {
        wait_until(
            Duration::from_secs(2),
            "the catalogue events arrive",
            || self.events().len() == count,
        )
        .await
        .expect("the catalogue logs the awaited events in time");
    }

    fn record(&self, url: &Url, headers: Option<&Headers>, body: Option<&[u8]>) {
        let device_id = body
            .and_then(|body| serde_json::from_slice::<Value>(body).ok())
            .and_then(|body| body["device_id"].as_str().map(str::to_owned));
        self.requests
            .lock()
            .expect("the request log is intact")
            .push(Request {
                path: url.path().to_owned(),
                token: headers
                    .and_then(|headers| headers.get(AUTH_HEADER))
                    .map(str::to_owned),
                device_id,
                at: Instant::now(),
            });
    }
}

struct Active {
    events: Arc<Mutex<Vec<String>>>,
    query: String,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.events
            .lock()
            .expect("request completion recording must retain the event log")
            .push(format!("end:{}", self.query));
    }
}

#[async_trait]
impl Net for DelayedNet {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        self.record(&url, headers.as_ref(), None);
        match url.path() {
            paths::PROFILE => Ok(Bytes::from(
                json!({"result": {"profile": {"name": LABEL}}}).to_string(),
            )),
            _ => Ok(self
                .polls
                .lock()
                .expect("the script is intact")
                .pop_front()
                .unwrap_or_else(|| poll(None))),
        }
    }

    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        self.net.get_range(url, range, headers).await
    }

    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        self.net.head(url, headers).await
    }

    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError> {
        self.net.stream(url, headers).await
    }

    async fn post_bytes(
        &self,
        url: Url,
        body: Bytes,
        headers: Option<Headers>,
    ) -> Result<Bytes, NetError> {
        match url.path() {
            paths::SESSION => {
                self.record(&url, headers.as_ref(), Some(&body));
                let next = self
                    .sessions
                    .lock()
                    .expect("the script is intact")
                    .pop_front();
                return next.unwrap_or_else(|| Ok(session(300))).map_err(status);
            }
            paths::LOGOUT => {
                self.record(&url, headers.as_ref(), None);
                if self.logout_held.load(Ordering::SeqCst) {
                    std::future::pending::<()>().await;
                }
                return Ok(Bytes::from_static(br#"{"result": null}"#));
            }
            _ => {}
        }
        while self.parked.load(Ordering::SeqCst)
            && !String::from_utf8_lossy(&body).contains("KitharaPlaylists")
        {
            time::sleep(Duration::from_millis(10)).await;
        }
        let value: Value =
            serde_json::from_slice(&body).expect("the client must encode a JSON GraphQL request");
        let autocomplete = value["query"]
            .as_str()
            .is_some_and(|query| query.contains("KitharaAutocomplete"));
        let scripted = autocomplete
            .then(|| {
                self.autocomplete
                    .lock()
                    .expect("the autocomplete script is intact")
                    .pop_front()
            })
            .flatten();
        if autocomplete && scripted.is_none() {
            return Ok(Bytes::from_static(br#"{"data":{"searchAutocomplete":[]}}"#));
        }
        let query = value["variables"]["query"]
            .as_str()
            .unwrap_or("node")
            .to_owned();
        self.events
            .lock()
            .expect("request start recording must retain the event log")
            .push(format!("start:{query}"));
        let _active = Active {
            events: self.events.clone(),
            query: query.clone(),
        };
        if query == "blocked" || self.holding.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        while self.hold_streams.load(Ordering::SeqCst)
            && value["query"]
                .as_str()
                .is_some_and(|query| query.contains("KitharaStreams"))
        {
            self.stream_waiting.store(true, Ordering::SeqCst);
            time::sleep(Duration::from_millis(10)).await;
        }
        let result = if let Some(result) = scripted {
            result.map_err(status)
        } else {
            self.net.post_bytes(url, body, headers).await
        };
        while self.hold_reactions.load(Ordering::SeqCst)
            && value["query"]
                .as_str()
                .is_some_and(|query| query.contains("KitharaReaction"))
        {
            self.reaction_waiting.store(true, Ordering::SeqCst);
            time::sleep(Duration::from_millis(10)).await;
        }
        result
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum Shown {
    SignedOut,
    Waiting,
    Connected(Option<String>),
}

#[derive(Debug)]
pub(super) struct Row {
    pub(super) state: Shown,
    pub(super) fault: Option<String>,
}

/// The Zvuk source with its key grant, store and browser log.
pub(super) struct Linked {
    pub(super) source: Box<dyn LibrarySource>,
    pub(super) net: DelayedNet,
    pub(super) cancel: CancelToken,
    pub(super) grant: KeyAccess,
    secrets: Secrets,
    opened: Arc<Mutex<Vec<String>>>,
}

impl Linked {
    pub(super) fn new(net: DelayedNet, secrets: Secrets, fails: bool) -> Self {
        let cancel = kithara_test_utils::cancel_token();
        let opened: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = Arc::clone(&opened);
        let open = move |url: &Url| {
            log.lock()
                .expect("the URL log is intact")
                .push(url.to_string());
            if fails {
                Err(OpenUrlError::from(io::Error::other(
                    "synthetic browser refusal",
                )))
            } else {
                Ok(())
            }
        };
        let (source, grant) = built(net.clone(), secrets.clone(), open, &cancel);
        Self {
            source,
            net,
            cancel,
            grant,
            secrets,
            opened,
        }
    }

    pub(super) fn token(&self) -> Option<String> {
        self.grant.token()
    }

    pub(super) fn stored(&self) -> Option<String> {
        self.secrets.get("zvuk").expect("the store reads")
    }

    pub(super) fn opened(&self) -> Vec<String> {
        self.opened.lock().expect("the URL log is intact").clone()
    }

    pub(super) fn press(&mut self, endpoint: &str) {
        self.source.write(endpoint, &WriteValue::Trigger);
    }

    pub(super) fn row(&self) -> Row {
        let text = |endpoint| match self.source.read(endpoint) {
            Some(ReadValue::Text(text)) => text.to_owned(),
            other => panic!("the source reads `{endpoint}` as text: {other:?}"),
        };
        let shown = |endpoint| matches!(self.source.read(endpoint), Some(ReadValue::Bool(false)));
        let label = shown("account_label_hidden").then(|| text("account_label"));
        let state = if shown("account_connect_hidden") {
            Shown::SignedOut
        } else if shown("account_awaiting_hidden") {
            Shown::Waiting
        } else if shown("account_disconnect_hidden") {
            Shown::Connected(label)
        } else {
            panic!("the row offers no action");
        };
        Row {
            state,
            fault: shown("account_fault_hidden").then(|| text("account_fault")),
        }
    }

    pub(super) async fn shows(&mut self, ready: impl Fn(&Row) -> bool) {
        wait_until(
            Duration::from_secs(6),
            "the row reaches the awaited state",
            || {
                self.source.tick();
                ready(&self.row())
            },
        )
        .await
        .expect("the row reaches the awaited state in time");
    }

    pub(super) async fn sign_out(&mut self) {
        self.press("account_disconnect");
        self.granted(None).await;
    }

    /// Connects and has Zvuk confirm `token` at the first poll.
    pub(super) async fn sign_in(&mut self, token: &str) {
        self.net.polls([poll(Some(token))]);
        self.press("account_connect");
        self.granted(Some(token)).await;
    }

    async fn granted(&self, token: Option<&str>) {
        wait_until(
            Duration::from_secs(6),
            "the account grants the token",
            || self.token().as_deref() == token,
        )
        .await
        .expect("the account must reach the awaited token");
    }

    /// Cancels the plugin and returns its transport once its tasks end.
    pub(super) async fn release(self) -> DelayedNet {
        let Self {
            source,
            net,
            cancel,
            ..
        } = self;
        cancel.cancel();
        drop(source);
        released(&net.events).await;
        net
    }
}

/// The source over a store holding `stored`, once the account took it.
pub(super) async fn linked(net: DelayedNet, stored: Option<&str>) -> Linked {
    let secrets = memory();
    if let Some(token) = stored {
        secrets.set("zvuk", token).expect("the store writes");
    }
    let linked = Linked::new(net, secrets, false);
    linked.granted(stored).await;
    linked
}

/// A store whose every access waits until the test lets it go, then reaches
/// the store behind it.
struct Blocking {
    entered: Mutex<mpsc::Sender<()>>,
    gate: Mutex<mpsc::Receiver<()>>,
    inner: Arc<mock::Store>,
}

/// `entered` signals a store access; dropping `release` fails it.
pub(super) struct Gate {
    entered: mpsc::Receiver<()>,
    pub(super) release: mpsc::Sender<()>,
}

impl Gate {
    /// Waits until the next store access starts.
    pub(super) async fn reached(&self) {
        wait_until(Duration::from_secs(2), "a store access starts", || {
            self.entered.try_recv().is_ok()
        })
        .await
        .expect("the account reaches the store in time");
    }
}

/// A blocking store holding `token`, if any.
pub(super) fn blocking(token: Option<&str>) -> (Secrets, Gate) {
    let inner = mock::Store::new().expect("the mock store opens");
    if let Some(token) = token {
        Secrets::new(Ok(Arc::clone(&inner)))
            .set("zvuk", token)
            .expect("the store writes");
    }
    let (entered, entering) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let store = Blocking {
        entered: Mutex::new(entered),
        gate: Mutex::new(gate),
        inner,
    };
    (
        Secrets::new(Ok(Arc::new(store))),
        Gate {
            entered: entering,
            release,
        },
    )
}

impl CredentialStoreApi for Blocking {
    fn vendor(&self) -> String {
        "blocking".to_owned()
    }

    fn id(&self) -> String {
        "blocking".to_owned()
    }

    fn build(
        &self,
        service: &str,
        user: &str,
        modifiers: Option<&HashMap<&str, &str>>,
    ) -> keyring_core::Result<Entry> {
        let _ = self.entered.lock().expect("the signal is intact").send(());
        match self.gate.lock().expect("the gate is intact").recv() {
            Ok(()) => self.inner.build(service, user, modifiers),
            Err(_) => Err(Error::NoStorageAccess(Box::new(io::Error::other(
                "synthetic store release",
            )))),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn debug_fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Blocking")
    }
}
