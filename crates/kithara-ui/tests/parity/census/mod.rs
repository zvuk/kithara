//! Every control the document can name, mounted alone on each host: what it
//! draws, the gestures it answers, and the box it is laid out into.
//!
//! Keeping the gestures beside the paint census makes a new `ControlSpec`
//! incomplete until it names both its picture and its gestures. The census
//! enumerates kinds of control, so what a group declares over itself is
//! outside it by construction: a stepping surface has no row here and cannot
//! get one. Those are pinned as a gesture played to both hosts, in
//! `used::hand`.

mod answers;
mod boxes;
mod coverage;
mod fixture;
mod named;
mod paint;
mod table;
