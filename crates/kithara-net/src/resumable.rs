use std::pin::Pin;

use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use kithara_platform::{
    CancelToken,
    time::{Duration, sleep, timeout},
    tokio,
};
use num_traits::{AsPrimitive, ToPrimitive};

mod kithara {
    pub(crate) use kithara_test_macros::flash;
}

use crate::{
    ByteStream,
    error::{NetError, Retryability},
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
    expected_len: Option<u64>,
    stall: Duration,
    refetch: Refetch,
    policy: RetryPolicy,
    /// Resume re-fetches already performed, bounded by `policy.max_retries`.
    resumes: u32,
    consumed: u64,
    /// Leading bytes of the current (post-resume) body to discard before
    /// yielding — the already-consumed prefix a non-range server re-sent.
    to_skip: u64,
}

impl State {
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
            return Err(exhausted(self.policy.max_retries, cause));
        }
        let delay = self.policy.delay_for_attempt(self.resumes);
        self.resumes += 1;
        if !delay.is_zero() {
            sleep(delay).await;
        }
        let resumed = (self.refetch)(self.consumed).await?;
        self.inner = resumed.stream;
        self.to_skip = resumed.skip;
        Ok(())
    }

    /// Account one received chunk: drop the already-consumed prefix a
    /// non-range (`200`) resume re-sent, advance `consumed`, and return the
    /// bytes to yield — `None` when the chunk was prefix only (still
    /// progress: the stall timer re-arms on the next chunk await).
    fn take(&mut self, mut bytes: Bytes) -> Option<Bytes> {
        // `usize -> u64` is a widening cast (lossless on every target), so
        // `AsPrimitive` is infallible; `u64 -> usize` can narrow, so it goes
        // through checked `ToPrimitive` — `min` caps the skip at the chunk
        // length, so it always fits and the fallback is a valid split point.
        let len: u64 = bytes.len().as_();
        let skip = self.to_skip.min(len);
        self.to_skip -= skip;
        self.consumed = self.consumed.saturating_add(len - skip);
        let rest = bytes.split_off(skip.to_usize().unwrap_or(bytes.len()));
        (!rest.is_empty()).then_some(rest)
    }

    fn body_complete(&self) -> bool {
        self.expected_len
            .is_some_and(|expected| self.consumed >= expected)
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
                Ok(None) if st.expected_len.is_some() && !st.body_complete() => {
                    Some(Err(NetError::Network("HTTP body ended before content-length".to_string())))
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
pub(crate) fn resumable_body(
    first: ByteStream,
    refetch: Refetch,
    stall: Duration,
    policy: RetryPolicy,
    cancel: CancelToken,
) -> RawBody {
    let expected_len = content_length(&first);
    let state = State {
        refetch,
        stall,
        policy,
        cancel,
        inner: first,
        expected_len,
        consumed: 0,
        to_skip: 0,
        resumes: 0,
    };
    // `Option<State>` is the unfold's alive/finished switch: a terminal error
    // is yielded together with `None`, so the next poll ends the stream.
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
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use kithara_test_utils::kithara;

    use super::*;
    use crate::types::Headers;

    const STALL: Duration = Duration::from_millis(40);

    fn policy(max_retries: u32) -> RetryPolicy {
        RetryPolicy::builder()
            .max_retries(max_retries)
            .base_delay(Duration::from_millis(1))
            .max_delay(Duration::from_millis(5))
            .build()
    }

    fn byte_stream(chunks: Vec<Result<Bytes, NetError>>) -> ByteStream {
        ByteStream::new(Headers::default(), Box::pin(stream::iter(chunks)))
    }

    fn byte_stream_with_len(len: u64, chunks: Vec<Result<Bytes, NetError>>) -> ByteStream {
        let mut headers = Headers::default();
        headers.insert("content-length", len.to_string());
        ByteStream::new(headers, Box::pin(stream::iter(chunks)))
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

    /// Withheld body → bounded terminal `RetryExhausted` (not a hang). Works in
    /// real time and under `flash` (virtual clock collapses the waits).
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
            STALL,
            policy(2),
            CancelToken::never(),
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
            STALL,
            policy(2),
            CancelToken::never(),
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
                    byte_stream(vec![Ok(Bytes::from_static(b"def"))]),
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
            STALL,
            policy(2),
            CancelToken::never(),
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
                    byte_stream(vec![Ok(Bytes::from_static(b"def"))]),
                    0,
                ))
            })
        });
        let out = collect(resumable_body(
            byte_stream_with_len(6, vec![Ok(Bytes::from_static(b"abc"))]),
            refetch,
            STALL,
            policy(2),
            CancelToken::never(),
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
            STALL,
            policy(2),
            CancelToken::never(),
        ))
        .await
        .expect("non-range resume must complete without duplication");
        assert_eq!(out, b"abcdef", "prefix must be skipped, not duplicated");
    }
}
