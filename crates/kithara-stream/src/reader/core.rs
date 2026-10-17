use std::{
    io::{Read, Seek},
    sync::atomic::{AtomicBool, Ordering},
};

use kithara_platform::{sync::Arc, time::Duration};

use super::ReaderControl;
use crate::{BoxedEventSink, ByteMap, MediaInfo, VariantTransition};

/// Per-reader switch for blocking I/O during off-RT decoder construction.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ConstructionGate {
    armed: Arc<AtomicBool>,
}

impl ConstructionGate {
    /// Enable blocking construction reads.
    pub fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    /// Restore non-blocking steady-state reads.
    pub fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
    }

    /// Keep source probes non-blocking until drop restores the previous mode.
    #[must_use = "keep the scope alive while probing source bytes"]
    pub fn nonblocking(&self) -> impl Drop + '_ {
        struct Restore<'a> {
            armed: &'a AtomicBool,
            previous: bool,
        }

        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.armed.store(self.previous, Ordering::Release);
            }
        }

        Restore {
            armed: self.armed.as_ref(),
            previous: self.armed.swap(false, Ordering::AcqRel),
        }
    }

    /// Whether construction reads should wait for source bytes.
    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }
}

/// Move-only byte input owned by one decoder session.
pub trait SessionReader: Read + Seek + Send + Sync + 'static {}

impl<T> SessionReader for T where T: Read + Seek + Send + Sync + 'static {}

/// Reader capability and byte-stream facts captured when opening a decoder.
#[non_exhaustive]
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct OpenedReader {
    input: Box<dyn SessionReader>,
    #[field(get)]
    byte_len: Option<u64>,
    byte_map: Option<Arc<dyn ByteMap>>,
    construction_gate: Option<ConstructionGate>,
    control: ReaderControl,
    event_sink: Option<BoxedEventSink>,
}

impl OpenedReader {
    /// Bundle a reader with the facts resolved by the byte-stream owner.
    #[must_use]
    pub fn new<R: SessionReader>(
        input: R,
        byte_len: Option<u64>,
        byte_map: Option<Arc<dyn ByteMap>>,
        construction_gate: Option<ConstructionGate>,
        control: ReaderControl,
        event_sink: Option<BoxedEventSink>,
    ) -> Self {
        Self {
            byte_len,
            byte_map,
            construction_gate,
            control,
            event_sink,
            input: Box::new(input),
        }
    }

    /// Segment map captured by the byte-stream owner.
    #[must_use]
    pub fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        self.byte_map.clone()
    }

    /// Construction gate supplied by the byte-stream owner.
    #[must_use]
    pub fn construction_gate(&self) -> Option<ConstructionGate> {
        self.construction_gate.clone()
    }

    /// Independent cancellation and readiness of this exact reader generation.
    #[must_use]
    pub fn control(&self) -> ReaderControl {
        self.control.clone()
    }

    /// Transfer byte input to the decoder.
    #[must_use]
    pub fn into_inner(self) -> Box<dyn SessionReader> {
        self.input
    }

    /// Transfer reader-side observation to the decoder.
    pub fn take_event_sink(&mut self) -> Option<BoxedEventSink> {
        self.event_sink.take()
    }
}

/// Target facts needed to choose a decoder and its reader requirements before
/// an incoming session is opened.
#[derive(Clone, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct VariantReaderPlan {
    #[field(get(copy))]
    landing_time: Duration,
    media_info: MediaInfo,
    #[field(get(copy))]
    transition: VariantTransition,
}

impl VariantReaderPlan {
    /// Bind target media facts and landing time to one exact transition.
    #[must_use]
    pub const fn new(
        transition: VariantTransition,
        media_info: MediaInfo,
        landing_time: Duration,
    ) -> Self {
        Self {
            landing_time,
            media_info,
            transition,
        }
    }
}

/// Move-only incoming reader bound to one exact variant reader plan.
#[non_exhaustive]
pub struct OpenedVariantReader {
    reader: OpenedReader,
    plan: VariantReaderPlan,
}

impl OpenedVariantReader {
    /// Bind byte capabilities to the exact facts used to prepare the session.
    #[must_use]
    pub const fn new(plan: VariantReaderPlan, reader: OpenedReader) -> Self {
        Self { reader, plan }
    }

    /// Split the move-only bundle for decoder construction.
    #[must_use]
    pub fn split(self) -> (VariantReaderPlan, OpenedReader) {
        (self.plan, self.reader)
    }

    delegate::delegate! {
        to self.plan {
            /// Content time where the incoming decoder must land.
            #[must_use]
            pub fn landing_time(&self) -> Duration;
            /// Target media facts captured with the reader.
            #[must_use]
            pub fn media_info(&self) -> &MediaInfo;
            /// Exact transition that owns this reader.
            #[must_use]
            pub fn transition(&self) -> VariantTransition;
        }
    }
}

/// Result of taking the reader owned by one exact variant transition.
#[non_exhaustive]
pub enum VariantReaderTake {
    /// The transition is live, but its construction bytes are not ready yet.
    Preparing,
    /// The move-only reader is ready and transferred to the caller.
    Ready(OpenedVariantReader),
    /// The exact transition is live, but its reader was already transferred.
    Taken,
    /// The transition was superseded, aborted, promoted, or never existed.
    Stale,
}
