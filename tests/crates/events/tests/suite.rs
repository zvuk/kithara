#![cfg(not(target_arch = "wasm32"))]

use kithara_events::{BusEvent, EventBus, EventSet};
use kithara_test_dylib as _;

#[derive(Clone, Debug, EventSet)]
enum TestEvent {
    Bus(BusEvent),
}

#[kithara::test]
fn test_event_bus_publish_subscribe() {
    let bus = EventBus::new(32);
    let mut rx = bus.subscribe();
    bus.publish(BusEvent::Overflow {
        scope: 7,
        dropped: 1,
    });

    let event = rx.try_recv().map(|env| env.event).ok();
    assert!(matches!(
        event,
        Some(TestEvent::Bus(BusEvent::Overflow {
            scope: 7,
            dropped: 1
        }))
    ));
}
