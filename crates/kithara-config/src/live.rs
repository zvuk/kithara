use std::sync::atomic::Ordering;

use portable_atomic::{AtomicBool, AtomicF32};

/// Atomic retained boolean setting. Cloning creates an independent snapshot.
#[derive(Debug)]
pub struct LiveBool(AtomicBool);

impl LiveBool {
    #[must_use]
    pub const fn new(value: bool) -> Self {
        Self(AtomicBool::new(value))
    }

    #[must_use]
    pub fn load(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn store(&self, value: bool) {
        self.0.store(value, Ordering::Relaxed);
    }
}

impl Clone for LiveBool {
    fn clone(&self) -> Self {
        Self::new(self.load())
    }
}

/// Atomic retained floating-point setting. Cloning creates an independent snapshot.
#[derive(Debug)]
pub struct LiveF32(AtomicF32);

impl LiveF32 {
    #[must_use]
    pub const fn new(value: f32) -> Self {
        Self(AtomicF32::new(value))
    }

    #[must_use]
    pub fn load(&self) -> f32 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn store(&self, value: f32) {
        self.0.store(value, Ordering::Relaxed);
    }
}

impl Clone for LiveF32 {
    fn clone(&self) -> Self {
        Self::new(self.load())
    }
}
