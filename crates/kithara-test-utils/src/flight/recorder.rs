use std::{
    collections::VecDeque,
    fmt::{self, Write as _},
    sync::{Mutex, MutexGuard, PoisonError},
};

use tracing::{
    Event, Level, Metadata, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    filter::filter_fn,
    layer::{Context, Layer},
    registry::LookupSpan,
};

/// In-memory flight recorder: the last kithara events, whatever the fmt
/// layer's filter admits to stdout. A red test's dump (panic or hang)
/// carries this tail, so the evidence does not depend on guessing the right
/// per-test tracing filter in advance.
///
/// Two lanes so volume cannot evict signal: `#[kithara::probe]` sites fire
/// on every call and would flush rare FSM-transition DEBUG lines out of a
/// shared ring within milliseconds.
const MAX_EVENTS: usize = 256;
const MAX_EVENT_BYTES: usize = 512;
/// How far back a repeat folds into the entry already in the ring.
///
/// Steady-state volume arrives as a short cycle of a few distinct lines — a
/// fetch settling, then the two accounting lines it triggers — so a window a
/// few cycles wide collapses it where a last-line check would not, and a red
/// test's dump keeps the rare transitions that led to the failure.
const DEDUP_WINDOW: usize = 16;
/// Fields a repeat is allowed to differ in and still fold.
///
/// Every probe event carries a monotonic sequence, so comparing whole lines
/// made each firing unique and the fold above never once collapsed the probe
/// lane: a dump's tail was the last [`MAX_EVENTS`] firings of whichever probe
/// happened to be hottest, and a rare transition that led to the failure was
/// gone. Measured on one green run of `packaged_abr_switch_keeps_player_
/// continuity`: 19314 firings, 372 distinct once the counters are excluded.
///
/// The counters are dropped from the comparison only — the retained line keeps
/// the first firing's values, and the fold count carries the tempo.
const FOLD_COUNTERS: &[&str] = &["seq", "thread_seq"];

static EVENTS: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());
static PROBES: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());

/// One recorded line and how many times it repeated inside the window.
struct Entry {
    /// `line` without [`FOLD_COUNTERS`] — what a repeat is matched on.
    key: String,
    line: String,
    repeats: u32,
}

/// The rings outlive every `loom::model` execution and are read from the
/// panic hook, so they stay on the real mutex rather than the platform one:
/// a loom-modelled lock created inside one execution panics the moment
/// anything touches it after that execution ends.
///
/// Poisoning is ignored on purpose — a dump that drops the tail because some
/// other thread died mid-record loses exactly the evidence it was taken for.
fn lock(ring: &Mutex<VecDeque<Entry>>) -> MutexGuard<'_, VecDeque<Entry>> {
    ring.lock().unwrap_or_else(PoisonError::into_inner)
}

#[must_use]
pub fn layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    RingLayer.with_filter(filter_fn(|meta| classify(meta).is_some()))
}

/// Snapshot of the recorded DEBUG-event tail, oldest first.
#[must_use]
pub fn tail() -> Vec<String> {
    snapshot(&EVENTS)
}

/// Snapshot of the recorded `#[kithara::probe]` tail, oldest first.
#[must_use]
pub fn probes_tail() -> Vec<String> {
    snapshot(&PROBES)
}

fn snapshot(ring: &Mutex<VecDeque<Entry>>) -> Vec<String> {
    lock(ring)
        .iter()
        .map(|entry| {
            if entry.repeats > 1 {
                format!(
                    "{line} (x{repeats})",
                    line = entry.line,
                    repeats = entry.repeats
                )
            } else {
                entry.line.clone()
            }
        })
        .collect()
}

struct RingLayer;

enum Lane {
    Event,
    Probe,
}

/// Only kithara traffic: probe events (TRACE, `<crate>_probe` targets, one
/// per call of a probed production fn) and DEBUG-and-coarser events. The
/// recorder must keep callsites alive that the fmt filter disabled, without
/// pulling in dependency chatter or non-probe TRACE firehoses.
fn classify(meta: &Metadata<'_>) -> Option<Lane> {
    if !meta.is_event() || !meta.target().starts_with("kithara") {
        return None;
    }
    if meta.target().ends_with("_probe") {
        return Some(Lane::Probe);
    }
    (*meta.level() <= Level::DEBUG).then_some(Lane::Event)
}

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let Some(lane) = classify(meta) else {
            return;
        };
        // No timestamp: the ring is chronological by construction, and a wall
        // stamp would mix real and flash-virtual clocks depending on where the
        // event fired.
        let prefix = format!(
            "{level} {target}:",
            level = meta.level(),
            target = meta.target(),
        );
        let mut line = prefix.clone();
        let mut key = prefix;
        event.record(&mut LineVisitor {
            line: &mut line,
            key: &mut key,
        });
        let ring = match lane {
            Lane::Event => &EVENTS,
            Lane::Probe => &PROBES,
        };
        record(ring, line, key);
    }
}

fn record(ring: &Mutex<VecDeque<Entry>>, line: String, key: String) {
    let line = clamp(line);
    let key = clamp(key);
    let mut ring = lock(ring);
    let window = ring.len().saturating_sub(DEDUP_WINDOW);
    if let Some(entry) = ring.iter_mut().skip(window).find(|entry| entry.key == key) {
        entry.repeats = entry.repeats.saturating_add(1);
        return;
    }
    if ring.len() == MAX_EVENTS {
        ring.pop_front();
    }
    ring.push_back(Entry {
        line,
        key,
        repeats: 1,
    });
}

fn clamp(mut text: String) -> String {
    if text.len() <= MAX_EVENT_BYTES {
        return text;
    }
    let mut end = MAX_EVENT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push('…');
    text
}

struct LineVisitor<'a> {
    key: &'a mut String,
    line: &'a mut String,
}

impl Visit for LineVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.line, " {value:?}");
            let _ = write!(self.key, " {value:?}");
            return;
        }
        let _ = write!(self.line, " {name}={value:?}", name = field.name());
        if !FOLD_COUNTERS.contains(&field.name()) {
            let _ = write!(self.key, " {name}={value:?}", name = field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use tracing::debug;
    use tracing_subscriber::layer::SubscriberExt;

    use super::{MAX_EVENT_BYTES, MAX_EVENTS, layer, probes_tail, tail};
    use crate::kithara;

    /// The rings are process-global and the capacity tests flood them, so a
    /// concurrent marker lookup would find its entry evicted. Each test holds
    /// this lock from its emissions through its assertions.
    static FLIGHT_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn with_recorder(body: impl FnOnce()) -> MutexGuard<'static, ()> {
        let guard = FLIGHT_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, body);
        guard
    }

    #[kithara::test]
    fn debug_event_lands_in_the_tail_with_fields() {
        let _guard = with_recorder(|| {
            debug!(target: "kithara_flight_test", marker = 41, "unique recorder probe");
        });

        let line = tail()
            .into_iter()
            .rev()
            .find(|line| line.contains("unique recorder probe"))
            .expect("recorded event must be in the tail");
        assert!(line.contains("kithara_flight_test"), "{line}");
        assert!(line.contains("marker=41"), "{line}");
    }

    /// Without this the probe lane never folded at all: the sequence makes
    /// every firing a distinct line, so a dump's tail was the last
    /// `MAX_EVENTS` firings of the hottest probe and nothing else.
    #[kithara::test]
    fn a_probe_repeat_folds_across_a_changed_sequence() {
        let _guard = with_recorder(|| {
            for seq in 0..4u64 {
                tracing::trace!(
                    target: "kithara_flight_test_probe",
                    probe = "fold_across_seq_marker",
                    seq,
                    ready = true,
                );
            }
        });

        let folded = probes_tail()
            .into_iter()
            .filter(|line| line.contains("fold_across_seq_marker"))
            .collect::<Vec<_>>();

        assert_eq!(folded.len(), 1, "{folded:?}");
    }

    /// The fold count is the tempo of the probe, so it must total the firings
    /// rather than restart per line.
    #[kithara::test]
    fn a_folded_probe_counts_every_firing() {
        let _guard = with_recorder(|| {
            for seq in 0..4u64 {
                tracing::trace!(
                    target: "kithara_flight_test_probe",
                    probe = "fold_count_marker",
                    seq,
                );
            }
        });

        let line = probes_tail()
            .into_iter()
            .rev()
            .find(|line| line.contains("fold_count_marker"))
            .expect("folded probe must be in the probe tail");

        assert!(line.ends_with("(x4)"), "{line}");
    }

    /// A probe argument is the reason the site is instrumented. Folding on it
    /// would erase the transition the tail exists to show.
    #[kithara::test]
    fn a_changed_probe_argument_opens_a_new_entry() {
        let _guard = with_recorder(|| {
            for ready in [false, true] {
                tracing::trace!(
                    target: "kithara_flight_test_probe",
                    probe = "argument_split_marker",
                    seq = 1u64,
                    ready,
                );
            }
        });

        let entries = probes_tail()
            .into_iter()
            .filter(|line| line.contains("argument_split_marker"))
            .collect::<Vec<_>>();

        assert_eq!(entries.len(), 2, "{entries:?}");
    }

    /// The retained line is the first firing's, counters included: a tail that
    /// dropped them could no longer be matched against the stdout log.
    #[kithara::test]
    fn a_folded_probe_keeps_the_first_sequence_it_saw() {
        let _guard = with_recorder(|| {
            for seq in 5..8u64 {
                tracing::trace!(
                    target: "kithara_flight_test_probe",
                    probe = "kept_sequence_marker",
                    seq,
                );
            }
        });

        let line = probes_tail()
            .into_iter()
            .rev()
            .find(|line| line.contains("kept_sequence_marker"))
            .expect("folded probe must be in the probe tail");

        assert!(line.contains("seq=5"), "{line}");
    }

    #[kithara::test]
    fn non_kithara_and_trace_events_are_not_recorded() {
        let _guard = with_recorder(|| {
            debug!(target: "hyper_util::client", "foreign dependency chatter");
            tracing::trace!(target: "kithara_flight_test", "too fine for the ring");
        });

        let recorded = tail();
        assert!(
            !recorded
                .iter()
                .any(|line| line.contains("foreign dependency chatter")),
            "foreign target must not be recorded"
        );
        assert!(
            !recorded
                .iter()
                .any(|line| line.contains("too fine for the ring")),
            "non-probe TRACE must not be recorded"
        );
    }

    #[kithara::test]
    fn probe_trace_events_land_in_their_own_lane() {
        let _guard = with_recorder(|| {
            tracing::trace!(
                target: "kithara_flight_test_probe",
                probe = "flight_probe_marker",
                seq = 7u64,
                "probe firing"
            );
        });

        let line = probes_tail()
            .into_iter()
            .rev()
            .find(|line| line.contains("flight_probe_marker"))
            .expect("probe firing must be in the probe tail");
        assert!(line.contains("seq=7"), "{line}");
        assert!(
            !tail()
                .iter()
                .any(|line| line.contains("flight_probe_marker")),
            "probe volume must not occupy the event lane"
        );
    }

    #[kithara::test]
    fn probe_volume_does_not_evict_events() {
        let _guard = with_recorder(|| {
            debug!(target: "kithara_flight_test", "rare transition marker");
            for seq in 0..=MAX_EVENTS {
                tracing::trace!(
                    target: "kithara_flight_test_probe",
                    probe = "flood",
                    seq = seq as u64,
                    "probe flood"
                );
            }
        });

        assert!(
            tail()
                .iter()
                .any(|line| line.contains("rare transition marker")),
            "a probe flood must not evict the rare DEBUG event"
        );
    }

    #[kithara::test]
    fn ring_keeps_only_the_newest_events() {
        let _guard = with_recorder(|| {
            for index in 0..=MAX_EVENTS {
                debug!(target: "kithara_flight_test", index, "capacity probe");
            }
        });

        let recorded = tail();
        assert!(recorded.len() <= MAX_EVENTS);
        assert!(
            recorded.iter().any(|line| {
                line.contains("capacity probe") && line.contains(&format!("index={MAX_EVENTS}"))
            }),
            "newest event must survive"
        );
    }

    #[kithara::test]
    fn a_repeated_line_is_counted_instead_of_filling_the_ring() {
        let _guard = with_recorder(|| {
            for _ in 0..5 {
                debug!(target: "kithara_flight_test", "steady state line");
            }
        });

        let recorded: Vec<String> = tail()
            .into_iter()
            .filter(|line| line.contains("steady state line"))
            .collect();
        assert_eq!(recorded.len(), 1, "{recorded:?}");
    }

    #[kithara::test]
    fn a_repeated_line_carries_its_repeat_count() {
        let _guard = with_recorder(|| {
            for _ in 0..5 {
                debug!(target: "kithara_flight_test", "counted line");
            }
        });

        let line = tail()
            .into_iter()
            .rev()
            .find(|line| line.contains("counted line"))
            .expect("the repeated line must be in the tail");
        assert!(line.contains("(x5)"), "{line}");
    }

    /// Volume does not arrive as one line repeating: a fetch settles, and the
    /// two accounting lines it triggers follow. A last-line check would let
    /// that cycle evict everything before it.
    #[kithara::test]
    fn a_repeating_cycle_does_not_evict_the_history() {
        let _guard = with_recorder(|| {
            debug!(target: "kithara_flight_test", "rare transition before the flood");
            for _ in 0..MAX_EVENTS {
                debug!(target: "kithara_flight_test", "cycle step one");
                debug!(target: "kithara_flight_test", "cycle step two");
                debug!(target: "kithara_flight_test", "cycle step three");
            }
        });

        assert!(
            tail()
                .iter()
                .any(|line| line.contains("rare transition before the flood")),
            "a repeating cycle must not evict the transition that preceded it"
        );
    }

    #[kithara::test]
    fn oversized_event_is_truncated_at_a_char_boundary() {
        let payload = "\u{1f980}".repeat(MAX_EVENT_BYTES);
        let _guard = with_recorder(|| {
            debug!(target: "kithara_flight_test", oversized = payload.as_str(), "boundary probe");
        });

        let line = tail()
            .into_iter()
            .rev()
            .find(|line| line.contains("boundary probe"))
            .expect("truncated event must still be recorded");
        assert!(line.len() <= MAX_EVENT_BYTES + '…'.len_utf8());
        assert!(line.ends_with('…'), "{line}");
    }
}
