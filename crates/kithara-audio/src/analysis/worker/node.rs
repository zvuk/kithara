use kithara_platform::sync::mpsc::{Receiver, TryRecvError};
use kithara_resampler::ResamplerBackend;

use super::AnalysisTask;
pub(crate) use super::task::Job;
use crate::{
    analysis::analyzer::{AnalyzerBuilder, Detector},
    runtime::{Node, RtPolicy, ServiceClass, TickResult},
};

pub(crate) struct AnalysisNode<B>
where
    B: ResamplerBackend,
{
    builder: AnalyzerBuilder<B>,
    current: Option<AnalysisTask<B>>,
    detector: Option<Detector>,
    jobs: Receiver<Job>,
}

impl<B> AnalysisNode<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn new(mut builder: AnalyzerBuilder<B>, jobs: Receiver<Job>) -> Self {
        let detector = builder.take_detector();
        Self {
            builder,
            current: None,
            detector,
            jobs,
        }
    }

    fn tick_current(&mut self) -> TickResult {
        let Some(current) = &mut self.current else {
            return TickResult::UpstreamPending;
        };
        let result = current.tick(&self.builder, self.detector.as_mut());
        if current.is_done() {
            self.current = None;
        }
        result
    }
}

impl<B> Node for AnalysisNode<B>
where
    B: ResamplerBackend,
{
    fn on_cancel(&mut self) {
        self.current = None;
    }

    fn rt_policy(&self) -> RtPolicy {
        RtPolicy::Heavy
    }

    fn service_class(&self) -> ServiceClass {
        ServiceClass::Idle
    }

    fn tick(&mut self) -> TickResult {
        if self.current.is_some() {
            return self.tick_current();
        }

        match self.jobs.try_recv() {
            Ok(job) => {
                self.current = Some(AnalysisTask::new(job));
                self.tick_current()
            }
            Err(TryRecvError::Empty) => TickResult::UpstreamPending,
            Err(TryRecvError::Disconnected) => TickResult::Done,
        }
    }
}
