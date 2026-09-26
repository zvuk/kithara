use kithara::{
    abr::{AbrEvent, AbrMode},
    events::{Envelope, EventReceiver},
    platform::{
        CancelToken,
        sync::{Arc, Mutex},
        tokio,
        tokio::sync::broadcast,
    },
};
use kithara_audio::AudioEvent;

use crate::{
    core::event_set::ItemBusEvent,
    item::{ItemView, settle_failed},
    observer::ItemObserver,
    types::{FfiError, FfiItemEvent, FfiVariant},
};

/// Per-item state the event task owns: where events go, the item view they
/// settle, and what the bus has reported so far.
pub(crate) struct ItemTracker {
    duration_seconds: Option<f64>,
    observer: Arc<dyn ItemObserver>,
    state: Arc<Mutex<ItemView>>,
    variants: Vec<FfiVariant>,
}

impl ItemTracker {
    /// Milliseconds per second.
    const MS_PER_SECOND: f64 = 1000.0;

    /// 2^32 for splitting u64 into two u32 halves for lossless f64 conversion.
    const U32_MAX_PLUS_ONE: f64 = 4_294_967_296.0;

    /// Bit shift width for extracting the high 32 bits of a u64.
    const U64_HIGH_SHIFT: u32 = 32;

    /// Threshold for suppressing redundant duration/buffered updates (seconds).
    const UPDATE_THRESHOLD: f64 = 0.01;

    pub(crate) fn new(observer: Arc<dyn ItemObserver>, state: Arc<Mutex<ItemView>>) -> Self {
        Self {
            duration_seconds: None,
            observer,
            state,
            variants: Vec::new(),
        }
    }

    /// Same contract as `AudioPlayerItem::deliver`: state settles before
    /// observers see the event.
    fn deliver(&self, event: FfiItemEvent) {
        self.state.lock().absorb(&event);
        self.observer.on_event(event);
    }

    fn dispatch(&mut self, event: &ItemBusEvent) {
        if let Some(duration) = Self::duration_from_event(event)
            && self
                .duration_seconds
                .is_none_or(|current| (current - duration).abs() > Self::UPDATE_THRESHOLD)
        {
            self.duration_seconds = Some(duration);
            self.deliver(FfiItemEvent::DurationChanged { seconds: duration });
        }

        self.dispatch_variant_events(event);

        if let Ok(event) = FfiItemEvent::try_from(event) {
            self.deliver(event);
        }

        if let Ok(error) = FfiError::try_from(event) {
            settle_failed(&self.state, self.observer.as_ref(), &error.to_string());
        }
    }

    fn dispatch_variant_events(&mut self, event: &ItemBusEvent) {
        let observer = &self.observer;
        let variants = &mut self.variants;
        match event {
            ItemBusEvent::Abr(AbrEvent::VariantsRegistered {
                variants: v,
                initial,
            }) => {
                let ffi_variants: Vec<FfiVariant> = v
                    .iter()
                    .filter_map(|vi| {
                        let Ok(index) = u32::try_from(vi.variant_index.get()) else {
                            tracing::error!(
                                idx = vi.variant_index.get(),
                                "BUG: HLS variant index exceeds u32::MAX, dropped from FFI list"
                            );
                            return None;
                        };
                        Some(FfiVariant {
                            index,
                            bandwidth_bps: vi.bandwidth_bps.unwrap_or(0),
                            name: vi.name.clone(),
                        })
                    })
                    .collect();
                variants.clone_from(&ffi_variants);
                observer.on_event(FfiItemEvent::VariantsDiscovered {
                    variants: ffi_variants,
                });
                let Ok(initial_u32) = u32::try_from(initial.get()) else {
                    tracing::error!(
                        idx = initial.get(),
                        "BUG: initial HLS variant index exceeds u32::MAX, skipping initial VariantApplied"
                    );
                    return;
                };
                if let Some(initial) = variants.iter().find(|v| v.index == initial_u32) {
                    observer.on_event(FfiItemEvent::VariantApplied {
                        variant: initial.clone(),
                    });
                }
            }
            ItemBusEvent::Abr(AbrEvent::ModeChanged {
                mode: AbrMode::Manual(idx),
            }) => {
                let Ok(idx_u32) = u32::try_from(idx.get()) else {
                    tracing::error!(
                        idx = idx.get(),
                        "BUG: manual variant index exceeds u32::MAX, skipping VariantSelected"
                    );
                    return;
                };
                let variant = variants
                    .iter()
                    .find(|v| v.index == idx_u32)
                    .cloned()
                    .unwrap_or(FfiVariant {
                        index: idx_u32,
                        bandwidth_bps: 0,
                        name: None,
                    });
                observer.on_event(FfiItemEvent::VariantSelected { variant });
            }
            ItemBusEvent::Abr(AbrEvent::VariantApplied { to, .. }) => {
                let Ok(idx_u32) = u32::try_from(to.get()) else {
                    tracing::error!(
                        idx = to.get(),
                        "BUG: applied variant index exceeds u32::MAX, skipping VariantApplied"
                    );
                    return;
                };
                let variant = variants
                    .iter()
                    .find(|v| v.index == idx_u32)
                    .cloned()
                    .unwrap_or(FfiVariant {
                        index: idx_u32,
                        bandwidth_bps: 0,
                        name: None,
                    });
                observer.on_event(FfiItemEvent::VariantApplied { variant });
            }
            _ => {}
        }
    }

    fn duration_from_event(event: &ItemBusEvent) -> Option<f64> {
        match event {
            ItemBusEvent::Audio(AudioEvent::PlaybackProgress {
                total_ms: Some(total_ms),
                ..
            }) => Some(Self::u64_to_f64(*total_ms)? / Self::MS_PER_SECOND),
            _ => None,
        }
    }

    fn u64_to_f64(value: u64) -> Option<f64> {
        let hi = u32::try_from(value >> Self::U64_HIGH_SHIFT).ok()?;
        let lo = u32::try_from(value & u64::from(u32::MAX)).ok()?;
        Some(f64::from(hi).mul_add(Self::U32_MAX_PLUS_ONE, f64::from(lo)))
    }
}

pub(crate) struct ItemEventBridge {
    cancel: CancelToken,
}

impl ItemEventBridge {
    /// Spawn a task that translates resource events into item callbacks
    /// and refreshes the shared [`ItemView`] cache backing the item's
    /// synchronous getters (`duration_sec`, `is_live_stream`, …).
    pub(crate) fn spawn(
        mut rx: EventReceiver<ItemBusEvent>,
        mut tracker: ItemTracker,
        cancel: CancelToken,
    ) -> Self {
        let task_cancel = cancel.clone();
        crate::FFI_RUNTIME.spawn(async move {
            loop {
                tokio::select! {
                    () = task_cancel.cancelled() => break,
                    event = rx.recv() => {
                        match event {
                            Ok(Envelope { event, .. }) => tracker.dispatch(&event),
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
        Self { cancel }
    }
}

impl Drop for ItemEventBridge {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use kithara::{
        abr::{AbrEvent, AbrMode, AbrReason, VariantDuration, VariantIndex, VariantInfo},
        platform::sync::{Arc, Mutex},
    };
    use kithara_file::{FileError, FileEvent};

    use super::ItemTracker;
    use crate::{
        core::event_set::ItemBusEvent,
        item::{AudioPlayerItem, ItemView},
        observer::ItemObserver,
        types::{FfiError, FfiItemConfig, FfiItemEvent, FfiVariant},
    };

    #[derive(Default)]
    struct CollectingItemObserver {
        events: Mutex<Vec<FfiItemEvent>>,
    }

    impl CollectingItemObserver {
        fn take_events(&self) -> Vec<FfiItemEvent> {
            std::mem::take(&mut *self.events.lock())
        }
    }

    impl ItemObserver for CollectingItemObserver {
        fn on_event(&self, event: FfiItemEvent) {
            self.events.lock().push(event);
        }
    }

    fn item_state() -> Arc<Mutex<ItemView>> {
        Arc::clone(
            &AudioPlayerItem::new(FfiItemConfig {
                abr_mode: None,
                audio_id: None,
                headers: None,
                uuid_i64: None,
                url: "https://example.com/quiet-intro.flac".to_string(),
                is_live_stream: false,
                preferred_peak_bitrate: 0.0,
                preferred_peak_bitrate_expensive: 0.0,
            })
            .state,
        )
    }

    fn dispatch_file_error(observer: &Arc<dyn ItemObserver>, state: &Arc<Mutex<ItemView>>) {
        ItemTracker::new(Arc::clone(observer), Arc::clone(state)).dispatch(&ItemBusEvent::File(
            FileEvent::Error {
                error: FileError::Io("boom".into()),
            },
        ));
    }

    fn variant(index: usize, bandwidth_bps: Option<u64>, name: Option<&str>) -> VariantInfo {
        VariantInfo {
            bandwidth_bps,
            codecs: None,
            container: None,
            name: name.map(str::to_owned),
            duration: VariantDuration::Unknown,
            variant_index: VariantIndex::new(index),
        }
    }

    fn dispatch_variant(
        observer: &Arc<dyn ItemObserver>,
        event: AbrEvent,
        variants: &mut Vec<FfiVariant>,
    ) {
        let mut tracker = ItemTracker::new(Arc::clone(observer), item_state());
        tracker.variants = std::mem::take(variants);
        tracker.dispatch_variant_events(&ItemBusEvent::Abr(event));
        *variants = tracker.variants;
    }

    #[kithara::test]
    fn file_error_maps_to_item_failed() {
        let event = ItemBusEvent::File(FileEvent::Error {
            error: FileError::Io("boom".into()),
        });
        let error = FfiError::try_from(&event).ok();
        assert!(matches!(
            error,
            Some(FfiError::ItemFailed { reason }) if reason == "io: boom"
        ));
    }

    /// The queue settles a failed track too, and reaches the same observer.
    /// Whichever source gets there first owns the pair; a protocol error
    /// arriving after it must not repeat what the item already reported.
    #[kithara::test]
    fn a_protocol_error_after_settlement_does_not_repeat_the_pair() {
        let observer_impl = Arc::new(CollectingItemObserver::default());
        let observer: Arc<dyn ItemObserver> = observer_impl.clone();
        let state = item_state();
        assert!(
            state.lock().mark_failed("test failure"),
            "the item settles first"
        );

        dispatch_file_error(&observer, &state);

        assert!(
            observer_impl.take_events().is_empty(),
            "a settled item must report its terminal pair once"
        );
    }

    #[kithara::test]
    fn a_protocol_error_on_a_live_item_emits_the_pair() {
        let observer_impl = Arc::new(CollectingItemObserver::default());
        let observer: Arc<dyn ItemObserver> = observer_impl.clone();
        let state = item_state();

        dispatch_file_error(&observer, &state);

        assert_eq!(observer_impl.take_events().len(), 2);
    }

    #[kithara::test]
    fn registered_variants_preserve_metadata_and_apply_initial() {
        let observer_impl = Arc::new(CollectingItemObserver::default());
        let observer: Arc<dyn ItemObserver> = observer_impl.clone();
        let mut variants = Vec::new();

        dispatch_variant(
            &observer,
            AbrEvent::VariantsRegistered {
                variants: vec![
                    variant(0, Some(128_000), Some("low")),
                    variant(1, None, Some("high")),
                ],
                initial: VariantIndex::new(1),
            },
            &mut variants,
        );

        assert_eq!(variants.len(), 2);
        assert!(matches!(
            observer_impl.take_events().as_slice(),
            [
                FfiItemEvent::VariantsDiscovered { variants },
                FfiItemEvent::VariantApplied { variant },
            ] if variants.len() == 2
                && variants[0].index == 0
                && variants[0].bandwidth_bps == 128_000
                && variants[0].name.as_deref() == Some("low")
                && variants[1].index == 1
                && variants[1].bandwidth_bps == 0
                && variants[1].name.as_deref() == Some("high")
                && variant.index == 1
                && variant.name.as_deref() == Some("high")
        ));

        dispatch_variant(
            &observer,
            AbrEvent::VariantsRegistered {
                variants: Vec::new(),
                initial: VariantIndex::new(9),
            },
            &mut variants,
        );
        assert!(variants.is_empty());
        assert!(matches!(
            observer_impl.take_events().as_slice(),
            [FfiItemEvent::VariantsDiscovered { variants }] if variants.is_empty()
        ));
    }

    #[kithara::test]
    fn selected_and_applied_variants_use_known_metadata_or_fallback() {
        let observer_impl = Arc::new(CollectingItemObserver::default());
        let observer: Arc<dyn ItemObserver> = observer_impl.clone();
        let mut variants = vec![FfiVariant {
            index: 1,
            bandwidth_bps: 256_000,
            name: Some("known".into()),
        }];

        for event in [
            AbrEvent::ModeChanged {
                mode: AbrMode::Manual(VariantIndex::new(1)),
            },
            AbrEvent::ModeChanged {
                mode: AbrMode::Manual(VariantIndex::new(2)),
            },
            AbrEvent::VariantApplied {
                from: VariantIndex::new(0),
                to: VariantIndex::new(1),
                reason: AbrReason::ManualOverride,
            },
            AbrEvent::VariantApplied {
                from: VariantIndex::new(1),
                to: VariantIndex::new(3),
                reason: AbrReason::UpSwitch,
            },
            AbrEvent::ModeChanged {
                mode: AbrMode::Auto(None),
            },
        ] {
            dispatch_variant(&observer, event, &mut variants);
        }

        assert!(matches!(
            observer_impl.take_events().as_slice(),
            [
                FfiItemEvent::VariantSelected { variant: selected_known },
                FfiItemEvent::VariantSelected { variant: selected_fallback },
                FfiItemEvent::VariantApplied { variant: applied_known },
                FfiItemEvent::VariantApplied { variant: applied_fallback },
            ] if selected_known.index == 1
                && selected_known.bandwidth_bps == 256_000
                && selected_known.name.as_deref() == Some("known")
                && selected_fallback.index == 2
                && selected_fallback.bandwidth_bps == 0
                && selected_fallback.name.is_none()
                && applied_known.index == 1
                && applied_known.bandwidth_bps == 256_000
                && applied_known.name.as_deref() == Some("known")
                && applied_fallback.index == 3
                && applied_fallback.bandwidth_bps == 0
                && applied_fallback.name.is_none()
        ));
    }

    #[cfg(target_pointer_width = "64")]
    #[kithara::test]
    fn overflowing_variant_indices_are_filtered_or_skipped() {
        let overflow = usize::try_from(u64::from(u32::MAX) + 1)
            .expect("64-bit hosts can represent indices above u32::MAX");
        let observer_impl = Arc::new(CollectingItemObserver::default());
        let observer: Arc<dyn ItemObserver> = observer_impl.clone();
        let mut variants = Vec::new();

        dispatch_variant(
            &observer,
            AbrEvent::VariantsRegistered {
                variants: vec![
                    variant(0, Some(128_000), Some("valid")),
                    variant(overflow, None, None),
                ],
                initial: VariantIndex::new(overflow),
            },
            &mut variants,
        );
        dispatch_variant(
            &observer,
            AbrEvent::ModeChanged {
                mode: AbrMode::Manual(VariantIndex::new(overflow)),
            },
            &mut variants,
        );
        dispatch_variant(
            &observer,
            AbrEvent::VariantApplied {
                from: VariantIndex::new(0),
                to: VariantIndex::new(overflow),
                reason: AbrReason::UpSwitch,
            },
            &mut variants,
        );

        assert_eq!(variants.len(), 1);
        assert!(matches!(
            observer_impl.take_events().as_slice(),
            [FfiItemEvent::VariantsDiscovered { variants }]
                if variants.len() == 1 && variants[0].index == 0
        ));
    }
}
