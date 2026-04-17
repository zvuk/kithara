//! Node and scheduling types.

/// Priority class for worker scheduling.
///
/// Nodes with higher service class are served first when the scheduler
/// selects which node to process next.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ServiceClass {
    /// Not playing, not needed soon. Lowest priority.
    #[default]
    Idle,
    /// Preloading or about to play. Medium priority.
    Warm,
    /// Currently audible. Highest priority.
    Audible,
}

/// Result of a single node tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TickResult {
    /// Node made progress (produced or consumed data, applied internal state change).
    Progress,
    /// Node is alive but waiting (backpressure, source not ready yet).
    Waiting,
    /// Node has finished its work (EOF, failed, terminal).
    Done,
}

/// A component that can be executed by the scheduler.
pub trait Node: Send + 'static {
    /// Perform one quantum of work.
    fn tick(&mut self) -> TickResult;

    /// Return the current service class (priority) of this node.
    fn service_class(&self) -> ServiceClass {
        ServiceClass::Audible
    }

    /// Called when the scheduler is cancelled or the node is unregistered.
    fn on_cancel(&mut self) {}
}

impl Node for Box<dyn Node> {
    fn tick(&mut self) -> TickResult {
        (**self).tick()
    }

    fn service_class(&self) -> ServiceClass {
        (**self).service_class()
    }

    fn on_cancel(&mut self) {
        (**self).on_cancel();
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn service_class_ordering() {
        assert!(ServiceClass::Idle < ServiceClass::Warm);
        assert!(ServiceClass::Warm < ServiceClass::Audible);
    }

    #[kithara::test]
    fn service_class_default_is_idle() {
        assert_eq!(ServiceClass::default(), ServiceClass::Idle);
    }
}
