//! Boxed `DecoderInput` alias used by demuxer constructors.

use crate::traits::DecoderInput;

pub(crate) type BoxedSource = Box<dyn DecoderInput>;
