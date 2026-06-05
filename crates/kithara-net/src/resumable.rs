//! Self-healing body stream: the [`ByteStream`] returned by streaming fetches
//! re-fetches/resumes on a stall or a transient mid-body error, so the stall
//! timeout and the retry count live in one place (the net layer) and cover the
//! BODY, not just request establishment.
//!
//! The stall detector is `select! { next, sleep(stall) }` — a quiescence
//! participant under `sim-time`, so the SAME code is bounded in virtual time
//! under simulation (collapsing to ~instant real time) and in real wall-clock
//! otherwise. No `timeout()` combinator (not uniformly sim-aware) and no
//! real-time scope: every wait routes through [`kithara_platform::time`].

use std::pin::Pin;

use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use kithara_platform::{
    CancellationToken,
    time::{Duration, sleep},
    tokio,
};
use num_traits::{AsPrimitive, ToPrimitive};

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
    refetch: Refetch,
    consumed: u64,
    /// Leading bytes of the current (post-resume) body to discard before
    /// yielding — the already-consumed prefix a non-range server re-sent.
    to_skip: u64,
    stall: Duration,
    policy: RetryPolicy,
    attempt: u32,
    cancel: CancellationToken,
    done: bool,
}

/// One loop outcome of the body race.
enum Ev {
    Chunk(Option<Result<Bytes, NetError>>),
    Stall,
    Cancel,
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
    cancel: CancellationToken,
) -> RawBody {
    let state = State {
        inner: first,
        refetch,
        consumed: 0,
        to_skip: 0,
        stall,
        policy,
        attempt: 0,
        cancel,
        done: false,
    };
    Box::pin(stream::unfold(state, |mut st| async move {
        loop {
            if st.done {
                return None;
            }
            let ev = tokio::select! {
                () = st.cancel.cancelled() => Ev::Cancel,
                c = st.inner.next() => Ev::Chunk(c),
                () = sleep(st.stall) => Ev::Stall,
            };
            match ev {
                Ev::Chunk(Some(Ok(mut bytes))) => {
                    // `usize -> u64` is a widening cast (lossless on every
                    // target), so `AsPrimitive` is infallible; `u64 -> usize`
                    // can narrow, so it goes through checked `ToPrimitive`.
                    let chunk_len: u64 = bytes.len().as_();
                    if st.to_skip > 0 {
                        // A non-range server re-sent the already-consumed prefix
                        // (`200` resume): drop it so the consumer's stream stays
                        // continuous. A skipped chunk is still progress, so the
                        // stall timer resets on the next loop pass. `min` caps
                        // the drop at the chunk length, so the narrowing always
                        // fits; the fallback (the full chunk length) is
                        // unreachable but a valid slice bound.
                        let drop_bytes = st.to_skip.min(chunk_len);
                        let drop = drop_bytes.to_usize().unwrap_or(bytes.len());
                        st.to_skip -= drop_bytes;
                        bytes = bytes.slice(drop..);
                        if bytes.is_empty() {
                            continue;
                        }
                        st.consumed = st.consumed.saturating_add(chunk_len - drop_bytes);
                        return Some((Ok(bytes), st));
                    }
                    st.consumed = st.consumed.saturating_add(chunk_len);
                    return Some((Ok(bytes), st));
                }
                Ev::Chunk(None) => return None,
                Ev::Cancel => {
                    st.done = true;
                    return Some((Err(NetError::Cancelled), st));
                }
                Ev::Chunk(Some(Err(err))) => {
                    if err.retryability() == Retryability::Fatal {
                        // Already terminal (fatal status / decode error):
                        // surface it as-is, nothing to resume.
                        st.done = true;
                        return Some((Err(err), st));
                    }
                    if st.attempt >= st.policy.max_retries {
                        // Transient, but the retry budget is spent — promote
                        // to the SAME terminal error as the stall path so
                        // downstream sees one `Fatal` exhaustion signal.
                        st.done = true;
                        return Some((Err(exhausted(st.policy.max_retries, err)), st));
                    }
                    // transient + budget remaining: fall through to resume.
                }
                Ev::Stall => {
                    if st.attempt >= st.policy.max_retries {
                        st.done = true;
                        return Some((
                            Err(exhausted(st.policy.max_retries, NetError::Timeout)),
                            st,
                        ));
                    }
                    // budget remaining: fall through to resume.
                }
            }
            // Resume: back off (virtual under sim), then re-fetch from the
            // consumed offset. A failed re-establish ends the stream.
            st.attempt += 1;
            sleep(st.policy.delay_for_attempt(st.attempt)).await;
            let fut = (st.refetch)(st.consumed);
            match fut.await {
                Ok(resumed) => {
                    st.inner = resumed.stream;
                    st.to_skip = resumed.skip;
                }
                Err(err) => {
                    st.done = true;
                    return Some((Err(err), st));
                }
            }
        }
    }))
}

/// Terminal error after the retry budget is exhausted, wrapping the cause
/// (the last transient chunk error, or `Timeout` for a pure stall). Both the
/// stall and the transient-chunk-error exhaustion paths funnel through here
/// so the consumer always sees one `Fatal` [`NetError::RetryExhausted`].
fn exhausted(max_retries: u32, source: NetError) -> NetError {
    NetError::RetryExhausted {
        max_retries,
        source: Box::new(source),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use kithara_platform::time::Instant;
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
    /// real time and under `sim-time` (virtual clock collapses the waits).
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
            CancellationToken::default(),
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
            CancellationToken::default(),
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
            CancellationToken::default(),
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
            CancellationToken::default(),
        ))
        .await
        .expect("non-range resume must complete without duplication");
        assert_eq!(out, b"abcdef", "prefix must be skipped, not duplicated");
    }
}
