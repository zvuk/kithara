use std::{
    io::SeekFrom,
    panic::{AssertUnwindSafe, catch_unwind},
};

use crossbeam_queue::ArrayQueue;
use kithara_decode::{DecoderSeekOutcome, DropChunks, GaplessMode};
use kithara_platform::{
    sync::Arc,
    time::Duration,
    tokio::{runtime::Handle as RuntimeHandle, task::spawn_blocking_on},
};
use kithara_stream::{
    ByteMap, MediaInfo, OpenedReader, OpenedVariantReader, ReaderProfile, StreamType,
    VariantTransition, WorkerWake,
};
use tracing::warn;

use crate::pipeline::{
    decode::{core::DecoderFactory, generation::DecoderGeneration},
    rebuild::{
        policy::classify,
        state::{
            BuildId, DecoderBuildComplete, DecoderBuildPurpose, RebuildState, RecreateCause,
            RecreateNext, RecreateOutcome, RecreateState,
        },
    },
    seek::{ResumeState, SeekContext},
    stream::shared::SharedStream,
};

struct JobDeps {
    incoming_completion: Arc<ArrayQueue<DecoderBuildComplete>>,
    replacement_completion: Arc<ArrayQueue<DecoderBuildComplete>>,
    wake: Arc<dyn WorkerWake>,
    factory: DecoderFactory,
    gapless_mode: GaplessMode,
}

impl Clone for JobDeps {
    fn clone(&self) -> Self {
        Self {
            incoming_completion: self.incoming_completion.clone(),
            factory: self.factory.clone(),
            gapless_mode: self.gapless_mode,
            replacement_completion: self.replacement_completion.clone(),
            wake: self.wake.clone(),
        }
    }
}

enum PendingInput<T: StreamType> {
    Opened(OpenedReader),
    Shared {
        stream: SharedStream<T>,
        offset: u64,
    },
}

struct PendingJob<T: StreamType> {
    build: BuildId,
    purpose: DecoderBuildPurpose,
    deps: JobDeps,
    media_info: MediaInfo,
    landing: Option<Duration>,
    input: PendingInput<T>,
    offset: u64,
    seek_epoch: u64,
}

pub(crate) struct RebuildPort<T: StreamType> {
    deps: JobDeps,
    pending: Option<PendingJob<T>>,
    ready_replacement: Option<DecoderBuildComplete>,
    runtime: RuntimeHandle,
    next_build: u64,
}

pub(crate) struct RebuildRuntime {
    pub(crate) wake: Arc<dyn WorkerWake>,
    pub(crate) handle: RuntimeHandle,
}

impl<T: StreamType> RebuildPort<T> {
    const COMPLETION_CAPACITY: usize = 4;

    pub(crate) fn new(
        factory: DecoderFactory,
        gapless_mode: GaplessMode,
        runtime: RebuildRuntime,
    ) -> Self {
        Self {
            deps: JobDeps {
                factory,
                gapless_mode,
                incoming_completion: Arc::new(ArrayQueue::new(Self::COMPLETION_CAPACITY)),
                replacement_completion: Arc::new(ArrayQueue::new(Self::COMPLETION_CAPACITY)),
                wake: runtime.wake,
            },
            next_build: 1,
            pending: None,
            ready_replacement: None,
            runtime: runtime.handle,
        }
    }

    pub(crate) fn cache_replacement(
        &mut self,
        complete: DecoderBuildComplete,
    ) -> Option<DecoderBuildComplete> {
        self.ready_replacement.replace(complete)
    }

    pub(crate) const fn can_prepare(&self) -> bool {
        self.pending.is_none()
    }

    delegate::delegate! {
        to self.deps.replacement_completion {
            #[cfg(test)]
            #[call(clone)]
            pub(crate) fn completion(&self) -> Arc<ArrayQueue<DecoderBuildComplete>>;
            #[call(pop)]
            pub(crate) fn pop_replacement_completion(&self) -> Option<DecoderBuildComplete>;
        }
    }

    fn next_build(&mut self) -> BuildId {
        let build = BuildId::new(self.next_build);
        self.next_build = self.next_build.wrapping_add(1);
        build
    }

    pub(crate) fn pop_incoming_completion(&self) -> Option<DecoderBuildComplete> {
        self.deps.incoming_completion.pop()
    }

    pub(crate) fn prepare(
        &mut self,
        stream: &SharedStream<T>,
        recreate: RecreateState,
        started_seek_epoch: u64,
    ) -> Result<RebuildState, (RecreateState, RecreateOutcome)> {
        if self.pending.is_some() {
            return Err((recreate, RecreateOutcome::SoftFailed));
        }
        if recreate.cause == RecreateCause::FormatBoundary
            && matches!(recreate.next, RecreateNext::Decode)
        {
            if let Err(error) = stream.probe_seek(SeekFrom::Start(recreate.offset)) {
                let outcome = classify(&kithara_decode::DecodeError::from(error));
                return Err((recreate, outcome));
            }
        } else if stream.probe_seek(SeekFrom::Start(recreate.offset)).is_err() {
            return Err((recreate, RecreateOutcome::SoftFailed));
        }
        let build = self.next_build();
        self.pending = Some(PendingJob {
            build,
            deps: self.deps.clone(),
            input: PendingInput::Shared {
                stream: stream.clone(),
                offset: recreate.offset,
            },
            landing: None,
            media_info: recreate.media_info.clone(),
            offset: recreate.offset,
            purpose: DecoderBuildPurpose::Replacement,
            seek_epoch: started_seek_epoch,
        });
        Ok(RebuildState {
            build,
            recreate,
            started_seek_epoch,
            superseded_seek: None,
        })
    }

    pub(crate) fn prepare_incoming(
        &mut self,
        opened: OpenedVariantReader,
    ) -> Option<(VariantTransition, BuildId)> {
        if self.pending.is_some() {
            return None;
        }
        let transition = opened.transition();
        let landing = opened.landing_time();
        let media_info = opened.media_info().clone();
        let seek_epoch = transition.id().seek_epoch();
        let (_plan, reader) = opened.split();
        let build = self.next_build();
        self.pending = Some(PendingJob {
            build,
            media_info,
            seek_epoch,
            deps: self.deps.clone(),
            input: PendingInput::Opened(reader),
            landing: Some(landing),
            offset: 0,
            purpose: DecoderBuildPurpose::Incoming(transition),
        });
        Some((transition, build))
    }

    pub(crate) fn reader_profile(
        &self,
        media_info: &MediaInfo,
        byte_map: Option<&dyn ByteMap>,
    ) -> ReaderProfile {
        self.deps.factory.reader_profile(media_info, byte_map)
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

    pub(crate) fn submit(&mut self) {
        let Some(job) = self.pending.take() else {
            return;
        };
        drop(spawn_blocking_on(&self.runtime, move || run(job)));
    }

    pub(crate) fn take_replacement(&mut self, build: BuildId) -> Option<DecoderBuildComplete> {
        if self
            .ready_replacement
            .as_ref()
            .is_some_and(|complete| complete.build == build)
        {
            self.ready_replacement.take()
        } else {
            None
        }
    }

    pub(crate) fn wake(&self) {
        self.deps.wake.wake();
    }
}

fn run<T: StreamType>(job: PendingJob<T>) {
    let PendingJob {
        build,
        deps,
        input,
        landing,
        media_info,
        offset,
        purpose,
        seek_epoch,
    } = job;
    let reader = match input {
        PendingInput::Opened(reader) => reader,
        PendingInput::Shared { stream, offset } => stream.open_rebuild_reader(offset),
    };
    let construction_gate = reader.construction_gate();
    if let Some(gate) = &construction_gate {
        gate.arm();
    }
    let built = catch_unwind(AssertUnwindSafe(|| {
        let mut decoder = deps.factory.create(reader, media_info.clone())?;
        let pending_head_skip = match landing {
            Some(landing) => match decoder.seek(landing)? {
                DecoderSeekOutcome::Landed { .. } => Some(ResumeState {
                    trim_head: true,
                    seek: SeekContext {
                        target: landing,
                        epoch: seek_epoch,
                    },
                    ..Default::default()
                }),
                DecoderSeekOutcome::PastEof { .. } => None,
            },
            None => None,
        };
        let mut generation = DecoderGeneration::new(
            decoder,
            Some(media_info),
            offset,
            seek_epoch,
            pending_head_skip,
            deps.gapless_mode,
        );
        // A generation built here has nothing buffered yet, and this runs on
        // the off-RT builder, so the sink has nothing to carry.
        if landing.is_some_and(|landing| !landing.is_zero()) {
            generation.notify_seek(&DropChunks);
        }
        Ok(generation)
    }));
    if let Some(gate) = &construction_gate {
        gate.disarm();
    }
    let result = match built {
        Ok(result) => result.map_err(|error| classify(&error)),
        Err(payload) => {
            warn!(
                build = build.get(),
                offset,
                panic = %panic_message(payload),
                "decoder factory panicked during rebuild; failing track"
            );
            Err(RecreateOutcome::SoftFailed)
        }
    };
    let complete = DecoderBuildComplete {
        build,
        purpose,
        result,
    };
    let completion = match purpose {
        DecoderBuildPurpose::Replacement => &deps.replacement_completion,
        DecoderBuildPurpose::Incoming(_) => &deps.incoming_completion,
    };
    if let Err(complete) = completion.push(complete) {
        let _ = completion.pop();
        if completion.push(complete).is_err() {
            warn!(
                build = build.get(),
                "decoder build completion queue overflowed"
            );
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
