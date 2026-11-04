use kithara_derive::Ranged;

/// How far behind its input a [`FeedbackDelay`](super::FeedbackDelay) reads,
/// in frames; a fraction reads between two frames. Zero is no delay: the loop
/// would read the frame it is writing. The range ends at the largest history
/// any delay holds; a delay refuses a time past its own capacity.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 1.0, max = 8_388_608.0)]
pub struct DelayTime(f64);

/// Share of each echo a [`FeedbackDelay`](super::FeedbackDelay) feeds back,
/// below unity so every tail ends.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 0.0, max = 0.95)]
pub struct Feedback(f32);

/// Cutoff in Hz of the high-pass every repeat of a
/// [`FeedbackDelay`](super::FeedbackDelay) passes on its way back into the
/// history. A delay refuses a cutoff its sample rate cannot hold under
/// Nyquist.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 20.0, max = 4000.0)]
pub struct FeedbackHighPass(f32);

/// A change of one [`FeedbackDelay`](super::FeedbackDelay) parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DelayParam {
    Time(DelayTime),
    Feedback(Feedback),
    HighPass(FeedbackHighPass),
}
