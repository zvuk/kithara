//! Compat shim: only `BoxedSource` survives.
//!
//! The legacy `Backend` trait and per-backend `dispatch::<B: Backend>`
//! were removed in the decoder unification. `Demuxer` + `FrameCodec`
//! are the new public protocol; the factory composes them directly
//! without a per-backend capability trait. `BoxedSource` is kept as
//! the boxed `DecoderInput` alias because it is the public type that
//! flows from the factory entry points into demuxer constructors.

mod source_alias;

pub(crate) use source_alias::BoxedSource;
