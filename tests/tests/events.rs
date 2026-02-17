#![cfg(not(target_arch = "wasm32"))]
//! Integration tests for unified event bus.

use kithara_events::{Event, EventBus, HlsEvent};

#[test]
fn test_event_bus_publish_subscribe() {
    let bus = EventBus::new(32);
    let mut rx = bus.subscribe();
    bus.publish(HlsEvent::EndOfStream);

    let event = rx.try_recv().ok();
    assert!(matches!(event, Some(Event::Hls(HlsEvent::EndOfStream))));
}
