//! Context dump primitives used by `HangDetector` when a hang fires.
//!
//! `HangDump` is the user-facing contract: any `T: serde::Serialize`
//! implements it automatically via the blanket impl below. `NoContext`
//! is the zero-payload default used when no dump is configured.

use serde::Serialize;

/// Trait implemented by every value the detector can dump when a hang
/// fires. The default `label` returns `None`, letting the detector
/// fall back to its own constructor label.
pub trait HangDump {
    fn to_json(&self) -> String;
    fn label(&self) -> Option<&str> {
        None
    }
}

impl<T: Serialize> HangDump for T {
    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

/// Marker used when no context is provided. Serializes as JSON `null`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoContext;

impl Serialize for NoContext {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_unit_struct("NoContext")
    }
}
