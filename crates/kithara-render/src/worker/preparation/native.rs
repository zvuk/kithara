use std::{
    future::Future,
    num::NonZeroUsize,
    panic::AssertUnwindSafe,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use futures::{
    FutureExt, StreamExt,
    future::{AbortHandle, Abortable, LocalBoxFuture},
    stream::FuturesUnordered,
};
use kithara_platform::{
    CancelGroup, CancelToken,
    maybe_send::MaybeSend,
    sync::{Arc, mpsc},
    tokio::{runtime::Handle, sync::oneshot},
};
use kithara_worker::{
    Dispatcher, DispatcherConfig, Task, TaskConfig, TaskError, TaskHandle, TickResult, Worker,
};

use crate::{
    LoadRefusal,
    worker::{StreamWake, core::task_refusal},
};

type Factory = Box<dyn FnOnce() -> LocalBoxFuture<'static, ()> + Send>;

pub(in crate::worker) struct Preparation {
    requests: mpsc::Sender<Factory>,
    task: Result<TaskHandle, TaskError>,
    dispatcher: Dispatcher,
}

pub(in crate::worker) fn new(worker: &Worker, id: u64, cancel: Option<CancelGroup>) -> Preparation {
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name(format!("kithara-source-prepare-{id}"))
            .capacity(NonZeroUsize::MIN)
            .wait_timeout(crate::consts::ACTIVE_WAIT_TIMEOUT)
            .maybe_cancel(cancel)
            .build(),
    );
    let (requests, incoming) = mpsc::channel();
    let wake = dispatcher.wake_handle();
    let task = dispatcher.reserve(TaskConfig::new()).and_then(|reserved| {
        reserved.start_local(move |_| Preparations {
            requests: incoming,
            openings: FuturesUnordered::new(),
            waker: Waker::from(Arc::new(StreamWake::new(wake))),
        })
    });
    Preparation {
        requests,
        task,
        dispatcher,
    }
}

pub(in crate::worker) async fn execute<F, P, R>(
    preparation: &Preparation,
    cancel: Option<CancelToken>,
    factory: F,
) -> Result<R, LoadRefusal>
where
    F: FnOnce() -> P + MaybeSend + 'static,
    P: Future<Output = Result<R, LoadRefusal>> + 'static,
    R: MaybeSend + 'static,
{
    preparation
        .task
        .as_ref()
        .map_err(|error| task_refusal(*error))?;
    let runtime = Handle::try_current().map_err(|_| LoadRefusal::NoRuntime)?;
    let (response, received) = oneshot::channel();
    let (abort, registration) = AbortHandle::new_pair();
    let request = PendingPreparation(abort);
    let dispatcher = tracing::dispatcher::get_default(Clone::clone);
    let span = tracing::Span::current();
    let request_cancel = cancel.clone();
    preparation
        .requests
        .send(Box::new(move || {
            async move {
                let opening = AssertUnwindSafe(async move {
                    let future = tracing::dispatcher::with_default(&dispatcher, || {
                        let _runtime = runtime.enter();
                        let _span = span.enter();
                        Box::pin(factory())
                    });
                    Opening {
                        future,
                        runtime,
                        dispatcher,
                        span,
                    }
                    .await
                })
                .catch_unwind();
                let cancelled = async {
                    match request_cancel.as_ref() {
                        Some(cancel) => cancel.cancelled().await,
                        None => std::future::pending().await,
                    }
                };
                let result = Abortable::new(
                    async move {
                        kithara_platform::tokio::select! {
                            biased;
                            () = cancelled => Err(LoadRefusal::Cancelled),
                            result = opening => result.unwrap_or_else(|_| {
                                tracing::error!("source preparation panicked");
                                Err(LoadRefusal::Source(kithara_audio::TrackFailureKind::Render))
                            }),
                        }
                    },
                    registration,
                )
                .await;
                if let Ok(result) = result {
                    drop(response.send(result));
                }
            }
            .boxed_local()
        }))
        .map_err(|_| {
            if preparation.dispatcher.is_cancelled() {
                LoadRefusal::Cancelled
            } else {
                LoadRefusal::Source(kithara_audio::TrackFailureKind::Render)
            }
        })?;
    preparation.dispatcher.wake_handle().wake();
    let result = received.await;
    drop(request);
    match result {
        Ok(result) => result,
        Err(_)
            if preparation.dispatcher.is_cancelled()
                || cancel.as_ref().is_some_and(CancelToken::is_cancelled) =>
        {
            Err(LoadRefusal::Cancelled)
        }
        Err(_) => Err(LoadRefusal::Source(kithara_audio::TrackFailureKind::Render)),
    }
}

struct PendingPreparation(AbortHandle);

impl Drop for PendingPreparation {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Preparations {
    requests: mpsc::Receiver<Factory>,
    openings: FuturesUnordered<LocalBoxFuture<'static, ()>>,
    waker: Waker,
}

impl Task for Preparations {
    fn tick(&mut self) -> TickResult {
        let mut progress = false;
        for factory in self.requests.try_iter() {
            self.openings.push(factory());
            progress = true;
        }
        let mut context = Context::from_waker(&self.waker);
        while let Poll::Ready(Some(())) = self.openings.poll_next_unpin(&mut context) {
            progress = true;
        }
        if progress {
            TickResult::Progress
        } else if self.openings.is_empty() {
            TickResult::Backpressured
        } else {
            TickResult::Waiting
        }
    }
}

struct Opening<P: Future> {
    future: Pin<Box<P>>,
    runtime: Handle,
    dispatcher: tracing::Dispatch,
    span: tracing::Span,
}

impl<P: Future> Future for Opening<P> {
    type Output = P::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let _runtime = this.runtime.enter();
        tracing::dispatcher::with_default(&this.dispatcher, || {
            let _span = this.span.enter();
            this.future.as_mut().poll(cx)
        })
    }
}
