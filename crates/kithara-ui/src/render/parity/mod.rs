//! What the two hosts owe each other about the boxes a document is laid out
//! into, on the documents the committed rect corpus cannot reach.
//!
//! The corpus compares the documents this crate ships, standing still. Neither
//! of the two below is in it: a menu has no box at all until a press opens it,
//! and no shipped document puts a run in a box too small for it. So each one is
//! mounted on both hosts, used the way a person would use it, and the boxes the
//! two placed are compared.

mod bars;
mod blocks;
mod extension;
mod hand;
pub(in crate::render) mod immediate;
mod press;
mod run;
pub(in crate::render) mod shared;
mod stepper;
