/// Producer progress toward an installed source discontinuity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduledActivationProgress {
    AwaitingActivation,
    ProducingOldPcm,
    Ready,
}
