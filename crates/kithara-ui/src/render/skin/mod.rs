#[cfg(feature = "iced")]
mod iced;
mod neutral;

#[cfg(feature = "iced")]
pub(crate) use iced::IcedSkin;
// Its production reader is `Skin::text_role` inside `neutral`; the only
// caller outside it is the iced tree's test-only frame join.
#[cfg(test)]
pub(crate) use neutral::active_tone;
pub use neutral::{CrossfaderLabels, Skin};
