use arc_swap::ArcSwapOption;
use kithara_platform::sync::Arc;
use num_traits::ToPrimitive;
use portable_atomic::{AtomicF64, Ordering};

use crate::{
    musical::{SessionBeat, SourceSchedule},
    tempo::streaming::StretchControls,
};

/// Returns whether the selected exact-span engine accepts this continuous
/// source-frame advance, or `None` when this build has no such engine.
#[must_use]
pub fn bound_rate_supported(source_frames_per_output: f64) -> Option<bool> {
    #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
    {
        super::bound::rate_supported(source_frames_per_output)
    }
    #[cfg(not(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith")))]
    {
        let _ = source_frames_per_output;
        None
    }
}

/// Output frames in one exact-span planning request for the selected engine.
#[must_use]
pub fn bound_render_span_frames() -> Option<u64> {
    #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
    {
        Some(super::bound::render_span_frames())
    }
    #[cfg(not(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith")))]
    {
        None
    }
}

/// A tempo slot cannot be built as configured.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TempoSlotError {
    /// A deck was bound to the session grid on a build with no exact-span
    /// engine. Rendering it through the streaming slot would silently place its
    /// beats somewhere else, so the binding is refused instead.
    #[error("binding a deck to the session grid needs a compiled exact-span engine")]
    BoundEngineMissing,
}

/// The live binding phase of one resident tempo stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TempoState {
    /// The deck follows its live speed controls.
    Free,
    /// The exact-span renderer has a target but has not emitted it in phase.
    Converging,
    /// The deck follows the session grid.
    Bound,
}

pub(crate) struct TempoBinding {
    pub(crate) schedule: Arc<SourceSchedule>,
    pub(crate) session_origin: SessionBeat,
    bound_rate: AtomicF64,
    rendered: portable_atomic::AtomicBool,
}

impl TempoBinding {
    fn new(schedule: Arc<SourceSchedule>, session_origin: SessionBeat, initial_rate: f64) -> Self {
        Self {
            schedule,
            session_origin,
            bound_rate: AtomicF64::new(initial_rate),
            rendered: portable_atomic::AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_renderer(
        schedule: Arc<SourceSchedule>,
        session_origin: SessionBeat,
    ) -> Self {
        Self::new(schedule, session_origin, 1.0)
    }

    pub(crate) fn mark_rendered(&self) {
        self.rendered.store(true, Ordering::Release);
    }

    pub(crate) fn set_rate(&self, rate: f64) {
        self.bound_rate.store(rate, Ordering::Relaxed);
    }
}

struct TempoShared {
    binding: ArcSwapOption<TempoBinding>,
    controls: Arc<StretchControls>,
}

/// Shared control handle for one resident duration-changing stage.
///
/// Every resource built by a deck receives a clone of this handle. Binding is
/// changed through the handle after construction; the effect chain keeps the
/// same stage and observes the new target on its next source chunk.
#[derive(Clone)]
pub struct TempoSlot {
    shared: Arc<TempoShared>,
}

impl From<Arc<StretchControls>> for TempoSlot {
    fn from(controls: Arc<StretchControls>) -> Self {
        Self {
            shared: Arc::new(TempoShared {
                binding: ArcSwapOption::const_empty(),
                controls,
            }),
        }
    }
}

impl TempoSlot {
    /// Installs a binding target without replacing the resident stage.
    pub fn bind(&self, schedule: Arc<SourceSchedule>, session_origin: SessionBeat) {
        let initial_rate = f64::from(self.shared.controls.speed());
        self.shared.binding.store(Some(Arc::new(TempoBinding::new(
            schedule,
            session_origin,
            initial_rate,
        ))));
    }

    /// Returns the shared live controls used while the stage is free.
    #[must_use]
    pub fn controls(&self) -> &Arc<StretchControls> {
        &self.shared.controls
    }

    /// Returns the current binding phase.
    #[must_use]
    pub fn state(&self) -> TempoState {
        self.shared
            .binding
            .load()
            .as_ref()
            .map_or(TempoState::Free, |binding| {
                if binding.rendered.load(Ordering::Acquire) {
                    TempoState::Bound
                } else {
                    TempoState::Converging
                }
            })
    }

    /// Withdraws the binding and carries its last rendered tempo into the free
    /// controls.
    #[must_use]
    pub fn unbind(&self) -> Option<f32> {
        let binding = self.shared.binding.swap(None)?;
        let rate = binding.bound_rate.load(Ordering::Acquire);
        if rate.is_finite() && rate > 0.0 {
            let rate = rate.to_f32()?;
            self.shared.controls.set_speed(rate);
            Some(rate)
        } else {
            None
        }
    }

    pub(crate) fn binding(&self) -> Option<Arc<TempoBinding>> {
        self.shared.binding.load_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unbound_slot_is_free() {
        let slot = TempoSlot::from(StretchControls::new(1.0));

        assert_eq!(slot.state(), TempoState::Free);
    }
}
