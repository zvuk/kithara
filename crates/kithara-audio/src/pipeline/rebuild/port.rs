use std::{
    io::SeekFrom,
    panic::{AssertUnwindSafe, catch_unwind},
};

use crossbeam_queue::ArrayQueue;
use kithara_platform::{
    sync::Arc,
    tokio::{runtime::Handle as RuntimeHandle, task::spawn_blocking_on},
};
use kithara_stream::{MediaInfo, StreamType, WorkerWake};
use tracing::warn;

use crate::pipeline::{
    decode::core::DecoderFactory,
    rebuild::{
        policy::classify,
        state::{
            DecoderRebuildComplete, RebuildState, RecreateCause, RecreateNext, RecreateOutcome,
            RecreateState,
        },
    },
    stream::shared::SharedStream,
};

struct JobDeps<T: StreamType> {
    completion: Arc<ArrayQueue<DecoderRebuildComplete>>,
    factory: DecoderFactory<T>,
    wake: Arc<dyn WorkerWake>,
}

impl<T: StreamType> Clone for JobDeps<T> {
    fn clone(&self) -> Self {
        Self {
            completion: self.completion.clone(),
            factory: self.factory.clone(),
            wake: self.wake.clone(),
        }
    }
}

struct PendingJob<T: StreamType> {
    deps: JobDeps<T>,
    stream: SharedStream<T>,
    media_info: MediaInfo,
    offset: u64,
    ticket: u64,
}

pub(crate) struct RebuildPort<T: StreamType> {
    deps: JobDeps<T>,
    next_ticket: u64,
    pending: Option<PendingJob<T>>,
    runtime: RuntimeHandle,
}

pub(crate) struct RebuildRuntime {
    pub(crate) handle: RuntimeHandle,
    pub(crate) wake: Arc<dyn WorkerWake>,
}

impl<T: StreamType> RebuildPort<T> {
    pub(crate) fn new(factory: DecoderFactory<T>, runtime: RebuildRuntime) -> Self {
        Self {
            deps: JobDeps {
                completion: Arc::new(ArrayQueue::new(2)),
                factory,
                wake: runtime.wake,
            },
            next_ticket: 1,
            pending: None,
            runtime: runtime.handle,
        }
    }

    pub(crate) fn prepare(
        &mut self,
        stream: &SharedStream<T>,
        recreate: RecreateState,
        started_seek_epoch: u64,
    ) -> Result<RebuildState, (RecreateState, RecreateOutcome)> {
        if recreate.cause == RecreateCause::FormatBoundary
            && matches!(recreate.next, RecreateNext::Decode)
        {
            stream.clear_variant_fence();
            if let Err(error) = stream.probe_seek(SeekFrom::Start(recreate.offset)) {
                let outcome = classify(&kithara_decode::DecodeError::from(error));
                return Err((recreate, outcome));
            }
        } else {
            stream.clear_variant_fence();
            if stream.probe_seek(SeekFrom::Start(recreate.offset)).is_err() {
                return Err((recreate, RecreateOutcome::SoftFailed));
            }
            stream.clear_variant_fence();
        }
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.pending = Some(PendingJob {
            deps: self.deps.clone(),
            stream: stream.clone(),
            media_info: recreate.media_info.clone(),
            offset: recreate.offset,
            ticket,
        });
        Ok(RebuildState {
            completion: self.completion(),
            superseded_seek: None,
            recreate,
            started_seek_epoch,
            ticket,
        })
    }

    pub(crate) fn completion(&self) -> Arc<ArrayQueue<DecoderRebuildComplete>> {
        self.deps.completion.clone()
    }

    pub(crate) fn submit(&mut self) {
        let Some(job) = self.pending.take() else {
            return;
        };
        drop(spawn_blocking_on(&self.runtime, move || run(job)));
    }

    #[cfg(test)]
    pub(crate) fn run_inline(&mut self) {
        if let Some(job) = self.pending.take() {
            run(job);
        }
    }

    #[cfg(test)]
    pub(crate) fn runtime(&self) -> &RuntimeHandle {
        &self.runtime
    }
}

fn run<T: StreamType>(job: PendingJob<T>) {
    let PendingJob {
        deps,
        stream,
        media_info,
        offset,
        ticket,
    } = job;
    let result = match catch_unwind(AssertUnwindSafe(|| {
        (deps.factory)(stream, media_info, offset)
    })) {
        Ok(result) => result.map_err(|error| classify(&error)),
        Err(payload) => {
            warn!(
                ticket,
                offset,
                panic = %panic_message(payload),
                "decoder factory panicked during rebuild; failing track"
            );
            Err(RecreateOutcome::SoftFailed)
        }
    };
    let complete = DecoderRebuildComplete { result, ticket };
    if let Err(complete) = deps.completion.push(complete) {
        let _ = deps.completion.pop();
        if deps.completion.push(complete).is_err() {
            warn!(ticket, "decoder rebuild completion queue overflowed");
        }
    }
    deps.wake.wake();
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast::<&'static str>().map_or_else(
            |_| "unknown panic payload".to_string(),
            |message| (*message).to_string(),
        ),
    }
}
