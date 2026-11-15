use std::panic::{AssertUnwindSafe, catch_unwind};

use tracing_subscriber::layer::SubscriberExt;

use super::{
    MAX_EVENTS, ProbeEvent, SCOPE, STATE, count, events, events_of, last, layer, lock, recorded,
    scope,
};

#[derive(Default)]
struct LinearRecording {
    events: Vec<ProbeEvent>,
    probes: Vec<(ProbeEvent, usize)>,
}

impl LinearRecording {
    fn push(&mut self, event: ProbeEvent) {
        let fired = if let Some((latest, fired)) = self
            .probes
            .iter_mut()
            .find(|(latest, _)| latest.probe == event.probe)
        {
            *latest = event;
            *fired += 1;
            *fired
        } else {
            self.probes.push((event, 1));
            1
        };
        if fired <= MAX_EVENTS {
            self.events.push(event);
        }
    }

    fn history(&self) -> &[ProbeEvent] {
        let overflowed = self
            .probes
            .iter()
            .filter(|(_, fired)| *fired > MAX_EVENTS)
            .map(|(latest, fired)| format!("{} x{fired}", latest.probe))
            .collect::<Vec<_>>();
        assert!(
            overflowed.is_empty(),
            "usdt scope history overflowed {MAX_EVENTS} firings of {}; \
             read the probes under test with `events_of`",
            overflowed.join(", ")
        );
        &self.events
    }

    fn history_of(&self, probe: &str) -> Vec<ProbeEvent> {
        if let Some((_, fired)) = self.probes.iter().find(|(event, _)| event.probe == probe) {
            assert!(
                *fired <= MAX_EVENTS,
                "usdt scope history overflowed {MAX_EVENTS} firings of {probe} \
                 ({fired} fired); scope the observation to the operation under test"
            );
        }
        self.events
            .iter()
            .filter(|event| event.probe == probe)
            .copied()
            .collect()
    }
}

fn fire(probe: &str, value: u64) {
    tracing::event!(
        target: "kithara_keyed_parity_probe",
        tracing::Level::TRACE,
        probe = probe,
        value = value,
        signed = 7i64,
        negative = -1i64,
        third = 3u64,
        fourth = 4u64,
        fifth = 5u64,
        beyond_cap = 6u64,
    );
}

fn emit(reference: &mut LinearRecording, probe: &str, value: u64) {
    fire(probe, value);
    let event = last(probe).expect("the emitted probe is readable");
    assert_eq!(event.probe, probe);
    assert_eq!(event.field("value"), Some(value));
    assert_eq!(event.field("negative"), None);
    assert_eq!(event.field("beyond_cap"), None);
    assert_eq!(
        event.fields().collect::<Vec<_>>(),
        [
            ("value", value),
            ("signed", 7),
            ("third", 3),
            ("fourth", 4),
            ("fifth", 5)
        ]
    );
    reference.push(event);
}

fn assert_events(actual: &[ProbeEvent], expected: &[ProbeEvent]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(
            (
                actual.probe,
                actual.target,
                actual.file,
                actual.line,
                actual.thread,
                actual.wall_time
            ),
            (
                expected.probe,
                expected.target,
                expected.file,
                expected.line,
                expected.thread,
                expected.wall_time
            )
        );
        assert_eq!(actual.fields, expected.fields);
        assert_eq!(actual.len, expected.len);
    }
}

fn assert_recording(reference: &LinearRecording) {
    let (history, truncated) = recorded();
    assert_events(&history, &reference.events);
    assert_eq!(
        truncated,
        reference
            .probes
            .iter()
            .any(|(_, fired)| *fired > MAX_EVENTS)
    );
    for (expected, fired) in &reference.probes {
        assert_eq!(count(expected.probe), *fired);
        assert_events(
            &[last(expected.probe).expect("a known probe")],
            &[*expected],
        );
    }
    let state = lock(&STATE);
    assert_eq!(state.probes.len(), reference.probes.len());
    assert_eq!(
        state
            .overflowed()
            .map(|tally| (tally.latest.probe, tally.fired))
            .collect::<Vec<_>>(),
        reference
            .probes
            .iter()
            .filter(|(_, fired)| *fired > MAX_EVENTS)
            .map(|(event, fired)| (event.probe, *fired))
            .collect::<Vec<_>>()
    );
}

fn panic_message(action: impl FnOnce()) -> String {
    let panic = catch_unwind(AssertUnwindSafe(action)).expect_err("history must report overflow");
    if let Some(message) = panic.downcast_ref::<String>() {
        return message.clone();
    }
    panic
        .downcast_ref::<&str>()
        .expect("a string panic")
        .to_string()
}

#[test]
fn keyed_high_fanout_recording_matches_the_linear_reference() {
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer()), || {
        let trace = scope();
        let mut reference = LinearRecording::default();
        let names = (0..193)
            .map(|index| format!("probe-{:03}-\u{e9}", (index * 37) % 193))
            .collect::<Vec<_>>();
        for round in 0..4u64 {
            let nested = scope();
            for (index, name) in names.iter().enumerate() {
                emit(&mut reference, name, round * 193 + index as u64);
            }
            assert_events(&nested.events(), reference.history());
        }
        assert_recording(&reference);
        assert_events(&trace.events(), reference.history());
        for name in &names {
            assert_events(&events_of(name), &reference.history_of(name));
        }
        let names_before = lock(&STATE).names.len();
        assert_eq!(count("unknown-\u{e9}"), 0);
        assert!(last("unknown-\u{e9}").is_none());
        assert!(events_of("unknown-\u{e9}").is_empty());
        assert_eq!(lock(&STATE).names.len(), names_before);
        let first = last(&names[0]).expect("an interned name").probe.as_ptr();
        drop(trace);
        let serial = lock(&SCOPE);
        assert!(events().is_empty());
        assert_eq!(count(&names[0]), 0);
        assert!(last(&names[0]).is_none());
        drop(serial);
        let _next = scope();
        emit(&mut LinearRecording::default(), &names[0], 999);
        assert_eq!(
            last(&names[0]).expect("the next scope").probe.as_ptr(),
            first
        );
        assert_eq!(count(&names[0]), 1);
    });
}

#[test]
fn keyed_cap_transitions_and_diagnostics_match_the_linear_reference() {
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer()), || {
        let _trace = scope();
        let mut reference = LinearRecording::default();
        for (index, probe) in ["z-first", "a-second", "m-quiet"].into_iter().enumerate() {
            emit(&mut reference, probe, index as u64);
        }
        for (probe, fired) in [("z-first", MAX_EVENTS), ("a-second", MAX_EVENTS - 1)] {
            lock(&STATE)
                .probes
                .get_mut(probe)
                .expect("a known probe")
                .fired = fired;
            reference
                .probes
                .iter_mut()
                .find(|(event, _)| event.probe == probe)
                .expect("the reference probe")
                .1 = fired;
        }
        for (index, probe) in ["z-first", "a-second", "a-second", "m-quiet"]
            .into_iter()
            .enumerate()
        {
            emit(&mut reference, probe, index as u64 + 10);
            assert_recording(&reference);
        }
        assert_eq!(
            panic_message(|| {
                let _ = events();
            }),
            panic_message(|| {
                let _ = reference.history();
            })
        );
        for probe in ["z-first", "a-second"] {
            assert_eq!(
                panic_message(|| {
                    let _ = events_of(probe);
                }),
                panic_message(|| {
                    let _ = reference.history_of(probe);
                })
            );
        }
        assert_events(&events_of("m-quiet"), &reference.history_of("m-quiet"));
        assert!(super::Scope::unsatisfied_wait().contains(&format!(
            "history overflowed on z-first x{}, a-second x{}",
            MAX_EVENTS + 1,
            MAX_EVENTS + 1
        )));
    });
}

#[test]
fn keyed_counter_arithmetic_matches_the_linear_reference() {
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer()), || {
        let _trace = scope();
        let mut reference = LinearRecording::default();
        emit(&mut reference, "counter", 1);
        lock(&STATE)
            .probes
            .get_mut("counter")
            .expect("a known probe")
            .fired = usize::MAX;
        reference.probes[0].1 = usize::MAX;
        let actual = catch_unwind(|| fire("counter", 2));
        let event = last("counter").expect("latest is updated before arithmetic");
        assert_eq!(event.field("value"), Some(2));
        let expected = catch_unwind(AssertUnwindSafe(|| reference.push(event)));
        assert_eq!(actual.is_err(), expected.is_err());
        assert_recording(&reference);
    });
}
