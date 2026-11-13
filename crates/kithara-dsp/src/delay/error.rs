/// Why a read position or a span was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DelayError {
    #[error("delay is not finite, shorter than one frame, or longer than the prepared capacity")]
    OutOfRange,
    #[error("channel, plane count, plane coverage or span length does not match the delay line")]
    Shape,
}
