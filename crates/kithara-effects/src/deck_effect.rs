use std::{fmt, num::NonZeroU32};

/// A change of one parameter of a deck effect, each effect's parameters one
/// variant.
#[derive(Clone, Copy, Debug)]
pub enum FxParamChange {}

/// The effect a deck FX unit holds.
///
/// It arrives prepared off the audio thread with all the memory it uses, and
/// leaves the deck in the receipt of the command that removed it.
pub trait DeckEffect: Send {
    /// Writes the wet signal of the planar stereo `input` into `wet`, both of
    /// one length.
    fn process(&mut self, input: [&[f32]; 2], wet: [&mut [f32]; 2]);

    /// Whether this effect takes `change`: it has the parameter, and the value
    /// fits what the effect was prepared with.
    fn accepts(&self, change: &FxParamChange) -> bool;

    /// Applies `change`, one [`DeckEffect::accepts`] holds, from the next
    /// frame processed, without allocating or freeing.
    fn set(&mut self, change: FxParamChange);

    /// Returns the effect to the state it was prepared in.
    fn reset(&mut self);

    /// Whether the wet signal stays silent while the input does.
    fn tail_exhausted(&self) -> bool;

    /// Retunes all the effect derives from the sample rate to `sample_rate`,
    /// from the next frame processed, without allocating or freeing.
    fn update_sample_rate(&mut self, sample_rate: NonZeroU32);
}

impl fmt::Debug for dyn DeckEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DeckEffect")
    }
}
