use kithara_worker::Priority;
use serde::Serialize;

/// Priority class for playback scheduling.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceClass {
    /// Not playing and not needed soon.
    #[default]
    Idle,
    /// Preloading or about to play.
    Warm,
    /// Currently audible.
    Audible,
}

impl From<ServiceClass> for Priority {
    fn from(class: ServiceClass) -> Self {
        Self::new(match class {
            ServiceClass::Idle => 0,
            ServiceClass::Warm => 1,
            ServiceClass::Audible => 2,
        })
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn service_class_order_and_worker_priority_match() {
        assert!(ServiceClass::Idle < ServiceClass::Warm);
        assert!(ServiceClass::Warm < ServiceClass::Audible);
        assert_eq!(Priority::from(ServiceClass::Idle).get(), 0);
        assert_eq!(Priority::from(ServiceClass::Warm).get(), 1);
        assert_eq!(Priority::from(ServiceClass::Audible).get(), 2);
    }
}
