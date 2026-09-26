use std::pin::Pin;

use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use kithara_platform::{
    CancelToken,
    time::{Duration, sleep, timeout},
    tokio,
};
use num_traits::{AsPrimitive, ToPrimitive};
use url::Url;

mod kithara {
    pub(crate) use kithara_test_macros::flash;
}

use crate::{
    ByteStream,
    error::{NetError, Retryability},
    observe::Observer,
    range_response::representation_total,
    types::RetryPolicy,
};

/// A boxed body stream. Mirrors [`RawByteStream`](crate::traits) `Send` cfg:
/// `Send` on native, unconstrained on wasm (browser futures are `!Send`).
#[cfg(not(target_arch = "wasm32"))]
type RawBody = Pin<Box<dyn Stream<Item = Result<Bytes, NetError>> + Send>>;
#[cfg(target_arch = "wasm32")]
type RawBody = Pin<Box<dyn Stream<Item = Result<Bytes, NetError>>>>;

/// Outcome of a resume re-fetch: a fresh body plus how many leading bytes to
/// DROP before yielding. `skip = 0` when the server honoured the range (`206`,
/// body already starts at the resume point); `skip = base_start + consumed`
/// when it ignored the range (`200`, body restarts at zero) so the consumer
/// still sees one continuous, non-duplicated byte stream — see [`resumable_body`].
pub(crate) struct Resumed {
    pub(crate) stream: ByteStream,
    pub(crate) skip: u64,
}

/// Re-issue the fetch resuming from `consumed` absolute body bytes (the caller
/// closes over the url/range/headers and the inner `Net`, mapping `consumed`
/// onto `RangeSpec { start: orig_start + consumed, .. }`, and computes `skip`
/// from the response status).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type Refetch = Box<
    dyn Fn(u64) -> futures::future::BoxFuture<'static, Result<Resumed, NetError>> + Send + Sync,
>;
#[cfg(target_arch = "wasm32")]
pub(crate) type Refetch =
    Box<dyn Fn(u64) -> futures::future::LocalBoxFuture<'static, Result<Resumed, NetError>>>;

/// Per-stream state threaded through the `unfold`.
struct State {
    inner: ByteStream,
    cancel: CancelToken,
    stall: Duration,
    expected_len: Option<u64>,
    observer: Option<Observer>,
    representation_total: Option<u64>,
    refetch: Refetch,
    policy: RetryPolicy,
    resource: Url,
    partial: bool,
    /// Resume re-fetches already performed, bounded by `policy.max_retries`.
    resumes: u32,
    consumed: u64,
    /// Leading bytes of the current (post-resume) body to discard before
    /// yielding — the already-consumed prefix a non-range server re-sent.
    to_skip: u64,
}

impl State {
    fn body_complete(&self) -> bool {
        self.expected_len
            .is_some_and(|expected| self.consumed >= expected)
    }

    fn body_incomplete_at_eof(&self) -> bool {
        self.to_skip > 0
            || self
                .expected_len
                .map_or(self.partial, |_| !self.body_complete())
    }

    /// Heal one transient failure: spend a unit of retry budget, back off
    /// (virtual under flash), and re-establish from the consumed offset.
    ///
    /// # Errors
    ///
    /// Returns the terminal error to yield: the cause itself when it is fatal
    /// (`Cancelled`, fatal status, decode), [`NetError::RetryExhausted`] when
    /// the budget is spent, or the re-establish failure.
    async fn resume(&mut self, cause: NetError) -> Result<(), NetError> {
        if cause.retryability() == Retryability::Fatal {
            return Err(cause);
        }
        if self.resumes >= self.policy.max_retries {
            if let Some(observer) = self.observer.as_ref() {
                observer
                    .0
                    .retry_exhausted(self.policy.max_retries, self.consumed, &cause);
            }
            return Err(exhausted(self.policy.max_retries, cause));
        }
        if let NetError::Timeout = cause
            && let Some(observer) = self.observer.as_ref()
        {
            observer
                .0
                .body_stalled(self.consumed, self.expected_len, self.stall);
        }
        let delay = self.policy.delay_for_attempt(self.resumes);
        self.resumes += 1;
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Err(NetError::Cancelled),
            () = sleep(delay) => {}
        }
        let resumed = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Err(NetError::Cancelled),
            resumed = (self.refetch)(self.consumed) => resumed?,
        };
        let resumed_partial = resumed.stream.is_partial();
        let resumed_total = representation_total(resumed_partial, &resumed.stream.headers);
        if resumed_total.is_some_and(|total| total < resumed.skip) {
            return Err(NetError::Decode(format!(
                "resumed response for {} declared a representation shorter than the consumed prefix {}",
                self.resource, resumed.skip
            )));
        }
        if let Some(expected) = self.representation_total {
            match resumed_total {
                Some(actual) if expected == actual => {}
                Some(actual) => {
                    return Err(NetError::Decode(format!(
                        "resumed response for {} changed representation total from {expected} to {actual}",
                        self.resource
                    )));
                }
                None => {
                    return Err(NetError::Decode(format!(
                        "resumed response for {} dropped the known representation total {expected}",
                        self.resource
                    )));
                }
            }
        }
        if self.representation_total.is_none() {
            self.representation_total = resumed_total;
            if self.expected_len.is_none() {
                self.expected_len = resumed_total;
            }
        }
        if let Some(observer) = self.observer.as_ref() {
            observer
                .0
                .body_resumed(self.resumes, self.consumed, resumed.skip == 0);
        }
        self.partial = resumed_partial;
        self.inner = resumed.stream;
        self.to_skip = resumed.skip;
        Ok(())
    }

    /// Account one received chunk: drop the already-consumed prefix a
    /// non-range (`200`) resume re-sent, advance `consumed`, and return the
    /// bytes to yield — `None` when the chunk was prefix only (still
    /// progress: the stall timer re-arms on the next chunk await).
    ///
    /// `usize -> u64` widens losslessly; `u64 -> usize` narrows and goes through checked
    /// conversion, with `min` capping the skip at the chunk length so the fallback split point is
    /// always valid.
    fn take(&mut self, mut bytes: Bytes) -> Option<Bytes> {
        let received: u64 = bytes.len().as_();
        let skip = self.to_skip.min(received);
        self.to_skip -= skip;
        let mut rest = bytes.split_off(skip.to_usize().unwrap_or(bytes.len()));
        let available: u64 = rest.len().as_();
        let accepted = self.expected_len.map_or(available, |expected| {
            expected.saturating_sub(self.consumed).min(available)
        });
        rest.truncate(accepted.to_usize().unwrap_or(rest.len()));
        self.consumed = self.consumed.saturating_add(accepted);
        (!rest.is_empty()).then_some(rest)
    }
}

/// Await the next body chunk — a real socket read, so the fn is one
/// `flash(io)` bracket: the virtual clock is paced to real time while the
/// chunk is in flight, and the pace drops the moment the await resolves, so
/// pauses between consumer pulls stay fully virtual. A stall maps to
/// `Timeout` (transient) and cancellation to `Cancelled` (fatal), so every
/// failure funnels through [`State::resume`]'s single classification.
#[kithara::flash(io)]
async fn next_chunk(st: &mut State) -> Option<Result<Bytes, NetError>> {
    if st.body_complete() {
        return None;
    }
    tokio::select! {
        biased;
        res = timeout(st.stall, st.inner.next()) => {
            match res {
                Ok(None) if st.body_incomplete_at_eof() => {
                    Some(Err(NetError::Network(
                        "HTTP body ended before representation completion".to_string(),
                    )))
                }
                Ok(item) => item,
                Err(_) => Some(Err(NetError::Timeout)),
            }
        },
        () = st.cancel.cancelled() => {
            if st.body_complete() {
                None
            } else {
                Some(Err(NetError::Cancelled))
            }
        },
    }
}

/// Wrap a freshly-established body in the self-healing stream. On a stall
/// (no chunk within `stall`) or a transient chunk error, it re-fetches from the
/// consumed offset up to `policy.max_retries` times (with `policy` backoff),
/// then yields a terminal [`NetError::RetryExhausted`]. A clean EOF, a
/// non-transient error, or cancellation end the stream immediately.
///
/// `Option<State>` doubles as the unfold's alive/finished switch: a terminal error is yielded
/// together with `None`, so the next poll ends the stream.
pub(crate) fn resumable_body(
    first: ByteStream,
    refetch: Refetch,
    resource: Url,
    stall: Duration,
    policy: RetryPolicy,
    cancel: CancelToken,
    observer: Option<Observer>,
) -> RawBody {
    let expected_len = content_length(&first);
    let partial = first.is_partial();
    let response_total = representation_total(partial, &first.headers);
    let state = State {
        refetch,
        stall,
        policy,
        cancel,
        expected_len,
        observer,
        partial,
        resource,
        inner: first,
        representation_total: response_total,
        consumed: 0,
        to_skip: 0,
        resumes: 0,
    };
    Box::pin(stream::unfold(Some(state), |st| async move {
        let mut st = st?;
        loop {
            let cause = match next_chunk(&mut st).await {
                Some(Ok(bytes)) => match st.take(bytes) {
                    Some(out) => return Some((Ok(out), Some(st))),
                    None => continue,
                },
                None => return None,
                Some(Err(cause)) => cause,
            };
            if let Err(terminal) = st.resume(cause).await {
                return Some((Err(terminal), None));
            }
        }
    }))
}

/// Terminal error after the retry budget is exhausted, wrapping the cause
/// (the last transient chunk error, or `Timeout` for a pure stall). Both the
/// stall and the transient-chunk-error exhaustion paths funnel through here
/// so the consumer always sees one `Fatal` [`NetError::RetryExhausted`].
pub(crate) fn exhausted(max_retries: u32, source: NetError) -> NetError {
    NetError::RetryExhausted {
        max_retries,
        source: Box::new(source),
    }
}

fn content_length(stream: &ByteStream) -> Option<u64> {
    stream
        .headers
        .get("content-length")
        .or_else(|| stream.headers.get("Content-Length"))
        .and_then(|value| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use kithara_platform::sync::{Arc, Mutex};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{observe::NetObserver, types::Headers};

    mod consts {
        use super::*;

        pub(super) const STALL: Duration = Duration::from_millis(40);
    }

    /// Fires once, on the `at`-th stall. Which stall matters: only the second
    /// one onwards is followed by a real backoff wait.
    struct StallSignal {
        seen: AtomicU64,
        signal: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        at: u64,
    }

    impl StallSignal {
        fn nth(at: u64, signal: tokio::sync::oneshot::Sender<()>) -> Self {
            Self {
                at,
                seen: AtomicU64::new(0),
                signal: Mutex::new(Some(signal)),
            }
        }
    }

    impl NetObserver for StallSignal {
        fn body_stalled(&self, _consumed: u64, _expected: Option<u64>, _stall: Duration) {
            if self.seen.fetch_add(1, Ordering::SeqCst) + 1 != self.at {
                return;
            }
            if let Some(sender) = self.signal.lock().take() {
                sender.send(()).ok();
            }
        }
    }

    fn policy(max_retries: u32) -> RetryPolicy {
        RetryPolicy::builder()
            .max_retries(max_retries)
            .base_delay(Duration::from_millis(1))
            .max_delay(Duration::from_millis(5))
            .build()
    }

    fn test_url() -> Url {
        Url::parse("https://example.com/audio.bin").expect("test URL")
    }

    fn byte_stream(chunks: Vec<Result<Bytes, NetError>>) -> ByteStream {
        ByteStream::new(Headers::default(), Box::pin(stream::iter(chunks)))
    }

    fn byte_stream_with_len(len: u64, chunks: Vec<Result<Bytes, NetError>>) -> ByteStream {
        let mut headers = Headers::default();
        headers.insert("content-length", len.to_string());
        ByteStream::new(headers, Box::pin(stream::iter(chunks)))
    }

    fn partial_byte_stream(
        start: u64,
        end: u64,
        total: u64,
        inner: impl Stream<Item = Result<Bytes, NetError>> + Send + 'static,
    ) -> ByteStream {
        let mut headers = Headers::default();
        headers.insert(
            "content-length",
            end.saturating_sub(start).saturating_add(1).to_string(),
        );
        headers.insert("content-range", format!("bytes {start}-{end}/{total}"));
        ByteStream::with_partial(headers, Box::pin(inner), true)
    }

    fn partial_byte_stream_unknown(
        start: u64,
        end: u64,
        inner: impl Stream<Item = Result<Bytes, NetError>> + Send + 'static,
    ) -> ByteStream {
        let mut headers = Headers::default();
        headers.insert(
            "content-length",
            end.saturating_sub(start).saturating_add(1).to_string(),
        );
        headers.insert("content-range", format!("bytes {start}-{end}/*"));
        ByteStream::with_partial(headers, Box::pin(inner), true)
    }

    /// A body that never yields (server holds the connection open, no bytes).
    fn withheld() -> ByteStream {
        ByteStream::new(Headers::default(), Box::pin(stream::pending()))
    }

    async fn collect(body: RawBody) -> Result<Vec<u8>, NetError> {
        let mut out = Vec::new();
        let mut body = body;
        while let Some(item) = body.next().await {
            out.extend_from_slice(item?.as_ref());
        }
        Ok(out)
    }

    fn resumed(stream: ByteStream, skip: u64) -> Resumed {
        Resumed { stream, skip }
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn withheld_body_exhausts_bounded() {
        let refetch: Refetch = Box::new(|_off| Box::pin(async { Ok(resumed(withheld(), 0)) }));
        let started = Instant::now();
        let result = collect(resumable_body(
            withheld(),
            refetch,
            test_url(),
            consts::STALL,
            policy(2),
            CancelToken::never(),
            None,
        ))
        .await;
        let elapsed = started.elapsed();
        assert!(
            matches!(result, Err(NetError::RetryExhausted { .. })),
            "withheld body must terminate with RetryExhausted, got {result:?}"
        );
        // 3 stalls (initial + 2 retries) at 40ms = ~120ms virtual; never hangs.
        assert!(
            elapsed < Duration::from_secs(5),
            "must be bounded ({elapsed:?})"
        );
    }

    /// Slow-but-live body (chunks arrive within the stall window) → all bytes,
    /// no retry, no error.
    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn live_body_passes_through() {
        let refetch: Refetch =
            Box::new(|_off| Box::pin(async { Ok(resumed(byte_stream(vec![]), 0)) }));
        let body = byte_stream(vec![
            Ok(Bytes::from_static(b"abc")),
            Ok(Bytes::from_static(b"def")),
        ]);
        let out = collect(resumable_body(
            body,
            refetch,
            test_url(),
            consts::STALL,
            policy(2),
            CancelToken::never(),
            None,
        ))
        .await
        .expect("live body must pass through");
        assert_eq!(out, b"abcdef");
    }

    /// First attempt yields one chunk then stalls; resume (from the consumed
    /// offset) yields the rest → full body, and `refetch` is called with the
    /// correct resume offset.
    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn resumes_from_consumed_offset() {
        let resume_off = Arc::new(AtomicU64::new(u64::MAX));
        let seen = Arc::clone(&resume_off);
        // 206-style resume: server honoured the range, body starts at offset 3.
        let refetch: Refetch = Box::new(move |off| {
            seen.store(off, Ordering::SeqCst);
            Box::pin(async move {
                Ok(resumed(
                    partial_byte_stream(
                        3,
                        5,
                        6,
                        stream::once(async { Ok(Bytes::from_static(b"def")) }),
                    ),
                    0,
                ))
            })
        });
        // First yields "abc" then stalls (pending) → resume from offset 3.
        let first = ByteStream::new(
            Headers::default(),
            Box::pin(
                stream::once(async { Ok(Bytes::from_static(b"abc")) }).chain(stream::pending()),
            ),
        );
        let out = collect(resumable_body(
            first,
            refetch,
            test_url(),
            consts::STALL,
            policy(2),
            CancelToken::never(),
            None,
        ))
        .await
        .expect("resume must complete the body");
        assert_eq!(out, b"abcdef");
        assert_eq!(
            resume_off.load(Ordering::SeqCst),
            3,
            "resume from consumed offset"
        );
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn resume_rejects_conflicting_representation_total() {
        let refetch: Refetch = Box::new(|_off| {
            Box::pin(async {
                Ok(resumed(
                    partial_byte_stream(
                        3,
                        5,
                        9,
                        stream::once(async { Ok(Bytes::from_static(b"def")) }),
                    ),
                    0,
                ))
            })
        });
        let first = partial_byte_stream(
            0,
            5,
            6,
            stream::once(async { Ok(Bytes::from_static(b"abc")) }).chain(stream::pending()),
        );

        let result = collect(resumable_body(
            first,
            refetch,
            test_url(),
            consts::STALL,
            policy(1),
            CancelToken::never(),
            None,
        ))
        .await;

        assert!(
            matches!(result, Err(NetError::Decode(detail)) if detail.contains("6") && detail.contains("9"))
        );
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn resume_rejects_unknown_total_after_known_total() {
        let refetch: Refetch = Box::new(|_off| {
            Box::pin(async {
                Ok(resumed(
                    partial_byte_stream_unknown(
                        3,
                        5,
                        stream::once(async { Ok(Bytes::from_static(b"def")) }),
                    ),
                    0,
                ))
            })
        });
        let first = partial_byte_stream(
            0,
            5,
            6,
            stream::once(async { Ok(Bytes::from_static(b"abc")) }).chain(stream::pending()),
        );

        let result = collect(resumable_body(
            first,
            refetch,
            test_url(),
            consts::STALL,
            policy(1),
            CancelToken::never(),
            None,
        ))
        .await;

        assert!(
            matches!(result, Err(NetError::Decode(detail)) if detail.contains("known representation total"))
        );
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn resume_adopts_discovered_total_and_continues_after_capped_body() {
        let refetch: Refetch = Box::new(|off| {
            Box::pin(async move {
                let stream = match off {
                    3 => partial_byte_stream(
                        3,
                        4,
                        6,
                        stream::once(async { Ok(Bytes::from_static(b"de")) }),
                    ),
                    5 => partial_byte_stream(
                        5,
                        5,
                        6,
                        stream::once(async { Ok(Bytes::from_static(b"f")) }),
                    ),
                    _ => panic!("unexpected resume offset {off}"),
                };
                Ok(resumed(stream, 0))
            })
        });
        let first = ByteStream::new(
            Headers::default(),
            Box::pin(
                stream::once(async { Ok(Bytes::from_static(b"abc")) }).chain(stream::pending()),
            ),
        );

        let out = collect(resumable_body(
            first,
            refetch,
            test_url(),
            consts::STALL,
            policy(2),
            CancelToken::never(),
            None,
        ))
        .await
        .expect("discovered total must drive the remaining resume");

        assert_eq!(out, b"abcdef");
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn resumed_partial_unknown_total_never_proves_completion() {
        let refetch: Refetch = Box::new(|off| {
            Box::pin(async move {
                Ok(resumed(
                    partial_byte_stream_unknown(
                        off,
                        off.saturating_add(1),
                        stream::once(async { Ok(Bytes::from_static(b"de")) }),
                    ),
                    0,
                ))
            })
        });
        let first = ByteStream::new(
            Headers::default(),
            Box::pin(
                stream::once(async { Ok(Bytes::from_static(b"abc")) }).chain(stream::pending()),
            ),
        );

        let result = collect(resumable_body(
            first,
            refetch,
            test_url(),
            consts::STALL,
            policy(1),
            CancelToken::never(),
            None,
        ))
        .await;

        assert!(matches!(result, Err(NetError::RetryExhausted { .. })));
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn resumed_full_body_cannot_shrink_below_consumed_prefix() {
        let refetch: Refetch = Box::new(|off| {
            Box::pin(async move {
                Ok(resumed(
                    byte_stream_with_len(3, vec![Ok(Bytes::from_static(b"abc"))]),
                    off,
                ))
            })
        });
        let first = byte_stream(vec![
            Ok(Bytes::from_static(b"abcdef")),
            Err(NetError::Network("reset".to_string())),
        ]);

        let result = collect(resumable_body(
            first,
            refetch,
            test_url(),
            consts::STALL,
            policy(1),
            CancelToken::never(),
            None,
        ))
        .await;

        assert!(
            matches!(result, Err(NetError::Decode(detail)) if detail.contains("consumed prefix"))
        );
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn resumed_unknown_full_body_shorter_than_prefix_never_proves_completion() {
        let refetch: Refetch = Box::new(|off| {
            Box::pin(async move {
                Ok(resumed(
                    byte_stream(vec![Ok(Bytes::from_static(b"abc"))]),
                    off,
                ))
            })
        });
        let first = byte_stream(vec![
            Ok(Bytes::from_static(b"abcdef")),
            Err(NetError::Network("reset".to_string())),
        ]);

        let result = collect(resumable_body(
            first,
            refetch,
            test_url(),
            consts::STALL,
            policy(1),
            CancelToken::never(),
            None,
        ))
        .await;

        assert!(matches!(result, Err(NetError::RetryExhausted { .. })));
    }

    #[kithara::test(tokio, multi_thread, flash(false), timeout(Duration::from_secs(1)))]
    async fn cancellation_interrupts_an_inflight_refetch() {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let entered = Arc::new(Mutex::new(Some(entered_tx)));
        let refetch: Refetch = Box::new(move |_off| {
            let entered = Arc::clone(&entered);
            Box::pin(async move {
                if let Some(sender) = entered.lock().take() {
                    sender.send(()).ok();
                }
                futures::future::pending::<Result<Resumed, NetError>>().await
            })
        });
        let cancel = CancelToken::root();
        let trigger = cancel.clone();
        tokio::task::spawn(async move {
            entered_rx.await.expect("refetch must start");
            trigger.cancel();
        });
        let first = byte_stream(vec![Err(NetError::Network("reset".to_string()))]);

        let result = collect(resumable_body(
            first,
            refetch,
            test_url(),
            consts::STALL,
            policy(1),
            cancel,
            None,
        ))
        .await;

        assert!(matches!(result, Err(NetError::Cancelled)));
    }

    #[kithara::test(tokio, multi_thread, flash(false), timeout(Duration::from_secs(1)))]
    async fn cancellation_interrupts_retry_backoff() {
        // `delay_for_attempt(0)` is ZERO — the first retry is immediate, so a
        // cancel aimed at it races a sleep of no length and proves nothing about
        // a backoff. The SECOND stall is the first one followed by a real wait,
        // so that is the one this test cancels: the 60s the cancel must cut
        // short is longer than any scheduling slack, which is what makes the
        // outcome independent of who wins the race to run.
        let (stalled_tx, stalled_rx) = tokio::sync::oneshot::channel();
        let observer = Observer(Arc::new(StallSignal::nth(2, stalled_tx)));
        let cancel = CancelToken::root();
        let trigger = cancel.clone();
        tokio::task::spawn(async move {
            stalled_rx.await.expect("body must stall twice");
            trigger.cancel();
        });
        let refetches = Arc::new(AtomicU64::new(0));
        let refetch: Refetch = Box::new(move |_off| {
            // Counted on POLL, not on call: `tokio::select!` builds every
            // branch's future before it polls the biased cancel arm, so the
            // closure runs even for a re-fetch that is never awaited.
            let refetches = Arc::clone(&refetches);
            Box::pin(async move {
                assert_eq!(
                    refetches.fetch_add(1, Ordering::SeqCst),
                    0,
                    "cancelled backoff must not re-fetch"
                );
                Ok(resumed(withheld(), 0))
            })
        });
        let long_backoff = RetryPolicy::builder()
            .max_retries(2)
            .base_delay(Duration::from_secs(60))
            .max_delay(Duration::from_secs(60))
            .build();

        let result = collect(resumable_body(
            withheld(),
            refetch,
            test_url(),
            consts::STALL,
            long_backoff,
            cancel,
            Some(observer),
        ))
        .await;

        assert!(matches!(result, Err(NetError::Cancelled)));
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn resumed_partial_is_capped_to_original_response_span() {
        let refetch: Refetch = Box::new(|_off| {
            Box::pin(async {
                Ok(resumed(
                    partial_byte_stream(
                        3,
                        15,
                        100,
                        stream::once(async { Ok(Bytes::from_static(b"defghijklmnop")) }),
                    ),
                    0,
                ))
            })
        });
        let first = partial_byte_stream(
            0,
            5,
            100,
            stream::iter([
                Ok(Bytes::from_static(b"abc")),
                Err(NetError::Network("reset".to_string())),
            ]),
        );

        let out = collect(resumable_body(
            first,
            refetch,
            test_url(),
            consts::STALL,
            policy(1),
            CancelToken::never(),
            None,
        ))
        .await
        .expect("resume must complete only the original response span");

        assert_eq!(out, b"abcdef");
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn ignored_resume_is_capped_to_original_response_length() {
        let refetch: Refetch = Box::new(|off| {
            Box::pin(async move {
                Ok(resumed(
                    byte_stream_with_len(6, vec![Ok(Bytes::from_static(b"abcdefghi"))]),
                    off,
                ))
            })
        });
        let first = byte_stream_with_len(
            6,
            vec![
                Ok(Bytes::from_static(b"abc")),
                Err(NetError::Network("reset".to_string())),
            ],
        );

        let out = collect(resumable_body(
            first,
            refetch,
            test_url(),
            consts::STALL,
            policy(1),
            CancelToken::never(),
            None,
        ))
        .await
        .expect("ignored resume must complete only the original response length");

        assert_eq!(out, b"abcdef");
    }

    /// A clean EOF before the promised content-length is a broken transfer, not
    /// a complete body. Treat it like a transient body failure and resume from
    /// the consumed offset.
    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn early_eof_before_content_length_resumes() {
        let resume_off = Arc::new(AtomicU64::new(u64::MAX));
        let seen = Arc::clone(&resume_off);
        let refetch: Refetch = Box::new(move |off| {
            seen.store(off, Ordering::SeqCst);
            Box::pin(async move {
                Ok(resumed(
                    partial_byte_stream(
                        3,
                        5,
                        6,
                        stream::once(async { Ok(Bytes::from_static(b"def")) }),
                    ),
                    0,
                ))
            })
        });
        let out = collect(resumable_body(
            byte_stream_with_len(6, vec![Ok(Bytes::from_static(b"abc"))]),
            refetch,
            test_url(),
            consts::STALL,
            policy(2),
            CancelToken::never(),
            None,
        ))
        .await
        .expect("early EOF before content-length must resume");

        assert_eq!(out, b"abcdef");
        assert_eq!(resume_off.load(Ordering::SeqCst), 3);
    }

    /// Non-range server: the resume re-fetch returns the FULL body from zero
    /// (`200`, `skip = consumed`). The already-consumed prefix is dropped, so
    /// the consumer sees one continuous, non-duplicated stream.
    #[kithara::test(tokio, timeout(Duration::from_secs(10)))]
    async fn non_range_server_skips_prefix_no_duplication() {
        // Resume yields the WHOLE body "abcdef" again, with skip = 3 (consumed).
        let refetch: Refetch = Box::new(|off| {
            Box::pin(async move {
                Ok(resumed(
                    byte_stream(vec![Ok(Bytes::from_static(b"abcdef"))]),
                    off,
                ))
            })
        });
        // First yields "abc" then stalls → resume; the full re-stream's "abc"
        // prefix is skipped, leaving "def".
        let first = ByteStream::new(
            Headers::default(),
            Box::pin(
                stream::once(async { Ok(Bytes::from_static(b"abc")) }).chain(stream::pending()),
            ),
        );
        let out = collect(resumable_body(
            first,
            refetch,
            test_url(),
            consts::STALL,
            policy(2),
            CancelToken::never(),
            None,
        ))
        .await
        .expect("non-range resume must complete without duplication");
        assert_eq!(out, b"abcdef", "prefix must be skipped, not duplicated");
    }
}
