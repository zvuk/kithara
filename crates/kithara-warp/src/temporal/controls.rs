use std::sync::atomic::Ordering;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use std::sync::atomic::{AtomicBool, AtomicU8};

use arc_swap::ArcSwapOption;
use kithara_platform::sync::Arc;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_stretch::{BackendCapabilities, StretchKind};
use portable_atomic::AtomicU64;

use super::{RateTarget, RegionPlan};

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[derive(Debug)]
struct EngineControls {
    keylock: AtomicBool,
    backend: AtomicU8,
}

/// Requested temporal target shared by the caller and the Warp effect chain.
///
/// The playback layer publishes the rate a concrete resource actually applies;
/// this control stores intent, not an effective deck-speed observation.
#[derive(Debug)]
#[non_exhaustive]
pub struct StretchControls {
    target: AtomicU64,
    region_plan: ArcSwapOption<RegionPlan>,
    #[cfg(any(
        feature = "stretch-signalsmith",
        feature = "stretch-bungee",
        feature = "stretch-glide"
    ))]
    engine: EngineControls,
}

impl StretchControls {
    /// Lowest supported media seconds consumed per output second.
    ///
    /// This already asks the backend for a 20x stretch; lower values collapse
    /// quality without providing a useful playback mode.
    pub const MIN_SPEED: f32 = 0.05;

    #[must_use]
    pub fn new(speed: f32) -> Arc<Self> {
        Arc::new(Self {
            target: AtomicU64::new(RateTarget::pack(speed.max(Self::MIN_SPEED), 0)),
            region_plan: ArcSwapOption::const_empty(),
            #[cfg(any(
                feature = "stretch-signalsmith",
                feature = "stretch-bungee",
                feature = "stretch-glide"
            ))]
            engine: EngineControls {
                keylock: AtomicBool::new(false),
                backend: AtomicU8::new(u8::from(StretchKind::default())),
            },
        })
    }

    pub(crate) fn rate_target(&self) -> RateTarget {
        RateTarget::unpack(self.target.load(Ordering::Acquire))
    }

    pub fn set_speed(&self, speed: f32) -> u64 {
        let speed = speed.max(Self::MIN_SPEED);
        let mut revision = 0;
        let _ = self
            .target
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                revision = RateTarget::revision_from(current).wrapping_add(1);
                if revision == 0 {
                    revision = 1;
                }
                Some(RateTarget::pack(speed, revision))
            });
        u64::from(revision)
    }

    #[must_use]
    pub fn speed(&self) -> f32 {
        self.rate_target().speed()
    }

    delegate::delegate! {
        to self.region_plan {
            /// The active region-stretch plan, if any.
            #[must_use]
            #[call(load_full)]
            pub fn region_plan(&self) -> Option<Arc<RegionPlan>>;
            /// Install or clear the region-stretch plan; picked up on the next chunk.
            #[call(store)]
            pub fn set_region_plan(&self, plan: Option<Arc<RegionPlan>>);
        }
    }
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
mod backend {
    use super::*;

    impl StretchControls {
        #[must_use]
        pub fn backend(&self) -> StretchKind {
            StretchKind::from(self.engine.backend.load(Ordering::Relaxed))
        }

        #[must_use]
        pub fn keylock(&self) -> bool {
            self.engine.keylock.load(Ordering::Relaxed)
                && self.capabilities().contains(BackendCapabilities::KEYLOCK)
        }

        #[must_use]
        pub fn capabilities(&self) -> BackendCapabilities {
            self.backend().capabilities()
        }

        pub fn set_backend(&self, backend: StretchKind) {
            self.engine
                .backend
                .store(u8::from(backend), Ordering::Relaxed);
            if !backend
                .capabilities()
                .contains(BackendCapabilities::KEYLOCK)
            {
                self.engine.keylock.store(false, Ordering::Relaxed);
            }
        }

        pub fn set_keylock(&self, on: bool) {
            self.engine.keylock.store(
                on && self.capabilities().contains(BackendCapabilities::KEYLOCK),
                Ordering::Relaxed,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[cfg(feature = "stretch-glide")]
    fn glide_drops_keylock_intent() {
        let controls = StretchControls::new(1.0);
        let native = StretchKind::all()
            .iter()
            .copied()
            .find(|kind| kind.capabilities().contains(BackendCapabilities::KEYLOCK));
        if let Some(native) = native {
            controls.set_backend(native);
            controls.set_keylock(true);
            assert!(controls.keylock());
        }
        controls.set_backend(StretchKind::Glide);
        assert!(!controls.keylock());
        controls.set_keylock(true);
        assert!(!controls.keylock());
        if let Some(native) = native {
            controls.set_backend(native);
            assert!(!controls.keylock());
        }
    }

    #[kithara::test]
    #[case(0.0)]
    #[case(-1.0)]
    #[case(f32::NAN)]
    fn speed_target_is_clamped_at_construction_and_update(#[case] input: f32) {
        let controls = StretchControls::new(input);
        assert!((controls.speed() - StretchControls::MIN_SPEED).abs() < f32::EPSILON);

        controls.set_speed(1.0);
        controls.set_speed(input);
        assert!((controls.speed() - StretchControls::MIN_SPEED).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn concurrent_rate_publications_keep_one_coherent_revision_order() {
        let controls = StretchControls::new(1.0);
        let published = Mutex::new(Vec::new());

        std::thread::scope(|scope| {
            for writer in 0..4_u16 {
                let controls = &controls;
                let published = &published;
                scope.spawn(move || {
                    for step in 0..64_u16 {
                        let speed = f32::from(writer * 64 + step + 1) / 100.0;
                        let revision = controls.set_speed(speed);
                        published
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push((revision, speed.max(StretchControls::MIN_SPEED)));
                    }
                });
            }
        });

        let mut published = published
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        published.sort_unstable_by_key(|(revision, _)| *revision);
        assert_eq!(published.len(), 256);
        assert!(
            published
                .windows(2)
                .all(|pair| pair[0].0.checked_add(1) == Some(pair[1].0))
        );
        let latest = published.last().copied().expect("fixture publishes rates");
        assert_eq!(controls.rate_target().speed(), latest.1);
    }
}
