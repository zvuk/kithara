use kithara_audio::{ServiceClass, SourceAudioActivity, SourceAudioReadOutcome, SourceFrameRange};
use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_platform::{
    CancelToken,
    thread::{self, JoinHandle},
    time::Duration,
};
use num_traits::cast::AsPrimitive;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Producer, Split},
};

use crate::resource::Resource;

pub(super) const PORT_CAPACITY: usize = 4;

#[derive(Clone, Copy)]
pub(super) struct ElasticSourceRequest {
    generation: u64,
    range: SourceFrameRange,
}

impl ElasticSourceRequest {
    pub(super) const fn new(generation: u64, range: SourceFrameRange) -> Self {
        Self { generation, range }
    }

    pub(super) const fn generation(self) -> u64 {
        self.generation
    }

    pub(super) const fn range(self) -> SourceFrameRange {
        self.range
    }
}

pub(super) struct ElasticSourceWindow {
    generation: u64,
    range: SourceFrameRange,
    samples: PcmBuf,
}

impl ElasticSourceWindow {
    pub(super) const fn range(&self) -> SourceFrameRange {
        self.range
    }

    pub(super) fn release_samples(self) -> PcmBuf {
        self.samples
    }
}

pub(super) enum ElasticSourceReply {
    Ready(ElasticSourceWindow),
    Eof { generation: u64 },
    Failed { generation: u64 },
}

impl ElasticSourceReply {
    pub(super) const fn generation(&self) -> u64 {
        match self {
            Self::Ready(window) => window.generation,
            Self::Eof { generation } | Self::Failed { generation } => *generation,
        }
    }
}

pub(super) struct ElasticSourcePort {
    activity: SourceAudioActivity,
    cancel: CancelToken,
    pending_service_class: Option<ServiceClass>,
    recycle_tx: HeapProd<PcmBuf>,
    reply_rx: HeapCons<ElasticSourceReply>,
    request_tx: HeapProd<ElasticSourceRequest>,
    service_tx: HeapProd<ServiceClass>,
    worker: Option<JoinHandle<()>>,
}

struct SourceWorker {
    activity: SourceAudioActivity,
    cancel: CancelToken,
    pool: PcmPool,
    recycle_rx: HeapCons<PcmBuf>,
    reply_tx: HeapProd<ElasticSourceReply>,
    request_rx: HeapCons<ElasticSourceRequest>,
    resource: Resource,
    service_rx: HeapCons<ServiceClass>,
}

#[cfg(test)]
pub(super) struct ElasticSourceTestPeer {
    reply_tx: HeapProd<ElasticSourceReply>,
    recycle_rx: HeapCons<PcmBuf>,
    request_rx: HeapCons<ElasticSourceRequest>,
    _service_rx: HeapCons<ServiceClass>,
}

#[cfg(test)]
impl ElasticSourceTestPeer {
    pub(super) fn push_ready(
        &mut self,
        generation: u64,
        range: SourceFrameRange,
        samples: PcmBuf,
    ) -> bool {
        self.reply_tx
            .try_push(ElasticSourceReply::Ready(ElasticSourceWindow {
                generation,
                range,
                samples,
            }))
            .is_ok()
    }

    pub(super) fn pop_recycled(&mut self) -> Option<PcmBuf> {
        self.recycle_rx.try_pop()
    }

    pub(super) fn pop_request(&mut self) -> Option<ElasticSourceRequest> {
        self.request_rx.try_pop()
    }
}

impl ElasticSourcePort {
    pub(super) fn request(
        &mut self,
        request: ElasticSourceRequest,
    ) -> Result<(), ElasticSourceRequest> {
        self.flush_service_class();
        let result = self.request_tx.try_push(request);
        if result.is_ok() {
            self.activity.signal();
        }
        result
    }

    pub(super) fn receive(&mut self) -> Option<ElasticSourceReply> {
        self.flush_service_class();
        let reply = self.reply_rx.try_pop();
        if reply.is_some() {
            self.activity.signal();
        }
        reply
    }

    pub(super) fn recycle(&mut self, samples: PcmBuf) -> Result<(), PcmBuf> {
        self.flush_service_class();
        let result = self.recycle_tx.try_push(samples);
        if result.is_ok() {
            self.activity.signal();
        }
        result
    }

    pub(super) fn set_service_class(&mut self, service_class: ServiceClass) {
        self.pending_service_class = Some(service_class);
        self.flush_service_class();
    }

    fn flush_service_class(&mut self) {
        let Some(service_class) = self.pending_service_class.take() else {
            return;
        };
        match self.service_tx.try_push(service_class) {
            Ok(()) => self.activity.signal(),
            Err(service_class) => self.pending_service_class = Some(service_class),
        }
    }

    #[cfg(test)]
    fn shutdown_for_test(mut self) {
        self.cancel.cancel();
        self.activity.signal();
        if let Some(worker) = self.worker.take() {
            worker.join().expect("elastic source worker exits cleanly");
        }
    }
}

impl Drop for ElasticSourcePort {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.activity.signal();
        drop(self.worker.take());
    }
}

#[cfg(test)]
pub(super) fn elastic_source_test_pair(
    cancel: CancelToken,
) -> (ElasticSourcePort, ElasticSourceTestPeer) {
    let activity = SourceAudioActivity::for_test();
    let (request_tx, request_rx) = HeapRb::<ElasticSourceRequest>::new(PORT_CAPACITY).split();
    let (reply_tx, reply_rx) = HeapRb::<ElasticSourceReply>::new(PORT_CAPACITY).split();
    let (recycle_tx, recycle_rx) = HeapRb::<PcmBuf>::new(PORT_CAPACITY).split();
    let (service_tx, service_rx) = HeapRb::<ServiceClass>::new(1).split();
    (
        ElasticSourcePort {
            activity,
            cancel,
            pending_service_class: None,
            recycle_tx,
            reply_rx,
            request_tx,
            service_tx,
            worker: None,
        },
        ElasticSourceTestPeer {
            reply_tx,
            recycle_rx,
            request_rx,
            _service_rx: service_rx,
        },
    )
}

pub(super) fn spawn_elastic_source(
    resource: Resource,
    cancel: CancelToken,
    pool: PcmPool,
    activity: SourceAudioActivity,
) -> ElasticSourcePort {
    let (request_tx, request_rx) = HeapRb::<ElasticSourceRequest>::new(PORT_CAPACITY).split();
    let (reply_tx, reply_rx) = HeapRb::<ElasticSourceReply>::new(PORT_CAPACITY).split();
    let (recycle_tx, recycle_rx) = HeapRb::<PcmBuf>::new(PORT_CAPACITY).split();
    let (service_tx, service_rx) = HeapRb::<ServiceClass>::new(1).split();
    let worker_cancel = cancel.clone();
    let worker_activity = activity.clone();
    let worker = thread::spawn_named("kithara-elastic-source", move || {
        SourceWorker {
            activity: worker_activity,
            cancel: worker_cancel,
            pool,
            recycle_rx,
            reply_tx,
            request_rx,
            resource,
            service_rx,
        }
        .run();
    });
    ElasticSourcePort {
        activity,
        cancel,
        pending_service_class: None,
        recycle_tx,
        reply_rx,
        request_tx,
        service_tx,
        worker: Some(worker),
    }
}

impl SourceWorker {
    fn run(mut self) {
        let cancel_activity = self.activity.clone();
        let _cancel_wake = self.cancel.on_cancel(move || cancel_activity.signal());
        while self.run_cycle() {}
    }

    fn run_cycle(&mut self) -> bool {
        let snapshot = self.activity.snapshot();
        self.run_cycle_after_snapshot(snapshot)
    }

    fn run_cycle_after_snapshot(&mut self, snapshot: u64) -> bool {
        if self.cancel.is_cancelled() {
            return false;
        }
        apply_service_class(&self.resource, &mut self.service_rx);
        while self.recycle_rx.try_pop().is_some() {}
        let mut request = self.request_rx.try_pop();
        while let Some(newer) = self.request_rx.try_pop() {
            request = Some(newer);
        }
        let Some(request) = request else {
            self.activity.wait(snapshot);
            return true;
        };
        let reply = fill_window(
            &mut self.resource,
            &self.pool,
            &self.cancel,
            request,
            &mut self.service_rx,
            &self.activity,
        );
        let mut pending = Some(reply);
        while let Some(reply) = pending.take() {
            let snapshot = self.activity.snapshot();
            if self.cancel.is_cancelled() {
                return false;
            }
            apply_service_class(&self.resource, &mut self.service_rx);
            if let Err(reply) = self.reply_tx.try_push(reply) {
                pending = Some(reply);
                self.activity.wait(snapshot);
            }
        }
        true
    }
}

fn fill_window(
    resource: &mut Resource,
    pool: &PcmPool,
    cancel: &CancelToken,
    request: ElasticSourceRequest,
    service_rx: &mut HeapCons<ServiceClass>,
    activity: &SourceAudioActivity,
) -> ElasticSourceReply {
    let generation = request.generation();
    let sample_rate = resource.spec().sample_rate.get();
    let start: f64 = request.range().start().as_();
    if resource
        .seek(Duration::from_secs_f64(start / f64::from(sample_rate)))
        .is_err()
    {
        return ElasticSourceReply::Failed { generation };
    }
    let Ok(Some(demand)) = resource.request_source_audio(request.range(), 0) else {
        return ElasticSourceReply::Failed { generation };
    };
    let channels = usize::from(resource.spec().channels);
    let Ok(frames) = usize::try_from(request.range().len()) else {
        return ElasticSourceReply::Failed { generation };
    };
    let Some(sample_len) = frames.checked_mul(channels) else {
        return ElasticSourceReply::Failed { generation };
    };
    let mut samples = pool.get();
    if samples.ensure_len(sample_len).is_err() {
        return ElasticSourceReply::Failed { generation };
    }
    loop {
        let snapshot = activity.snapshot();
        if cancel.is_cancelled() {
            return ElasticSourceReply::Failed { generation };
        }
        apply_service_class(resource, service_rx);
        match resource.read_source_audio(&demand, request.range(), &mut samples[..sample_len]) {
            Ok(Some(SourceAudioReadOutcome::Ready { .. })) => {
                return ElasticSourceReply::Ready(ElasticSourceWindow {
                    generation,
                    range: request.range(),
                    samples,
                });
            }
            Ok(Some(SourceAudioReadOutcome::Pending)) => {
                activity.wait(snapshot);
            }
            Ok(Some(SourceAudioReadOutcome::Eof)) => {
                return ElasticSourceReply::Eof { generation };
            }
            Ok(Some(_)) | Ok(None) | Err(_) => {
                return ElasticSourceReply::Failed { generation };
            }
        }
    }
}

fn apply_service_class(resource: &Resource, service_rx: &mut HeapCons<ServiceClass>) {
    let mut latest = None;
    while let Some(service_class) = service_rx.try_pop() {
        latest = Some(service_class);
    }
    if let Some(service_class) = latest {
        resource.set_service_class(service_class);
    }
}

#[cfg(test)]
mod tests {
    use kithara_audio::SourceAudioActivity;
    use kithara_bufpool::PcmPool;
    use kithara_platform::{CancelScope, tokio::runtime::Builder};
    use kithara_test_utils::kithara;
    use ringbuf::{HeapRb, traits::Split};

    use super::{ElasticSourceReply, ElasticSourceRequest, SourceWorker, spawn_elastic_source};
    use crate::test_support::empty_resource;

    #[kithara::test(native, flash(false))]
    fn source_worker_lifetime_is_independent_of_tokio_runtime() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime builds");
        let scope = CancelScope::new(None);

        let port = spawn_elastic_source(
            empty_resource("runtime-independent.wav"),
            scope.token(),
            PcmPool::default(),
            SourceAudioActivity::for_test(),
        );

        drop(runtime);
        port.shutdown_for_test();
    }

    #[kithara::test(native, flash(false))]
    fn cancellation_after_snapshot_exits_without_parking() {
        let scope = CancelScope::new(None);
        let activity = SourceAudioActivity::for_test();
        let (_request_tx, request_rx) = HeapRb::<ElasticSourceRequest>::new(1).split();
        let (reply_tx, _reply_rx) = HeapRb::<ElasticSourceReply>::new(1).split();
        let (_recycle_tx, recycle_rx) = HeapRb::new(1).split();
        let (_service_tx, service_rx) = HeapRb::new(1).split();
        let mut worker = SourceWorker {
            activity: activity.clone(),
            cancel: scope.token(),
            pool: PcmPool::default(),
            recycle_rx,
            reply_tx,
            request_rx,
            resource: empty_resource("cancelled-cycle.wav"),
            service_rx,
        };
        let snapshot = activity.snapshot();
        scope.cancel();

        assert!(!worker.run_cycle_after_snapshot(snapshot));
    }
}
