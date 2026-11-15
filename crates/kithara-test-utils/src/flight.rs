use std::{
    collections::VecDeque,
    fmt::{self, Write as _},
    hash::{DefaultHasher, Hash as _, Hasher as _},
    sync::{Mutex, MutexGuard, OnceLock, PoisonError},
};

use arrayvec::ArrayString;
use kithara_platform::time::WallInstant;
use tracing::{
    Event, Level, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};
use tracing_subscriber::{
    field::RecordFields,
    filter::filter_fn,
    fmt::{
        FormattedFields,
        format::{DefaultFields, FormatFields, Writer},
    },
    layer::{Context, Layer},
    registry::LookupSpan,
};

mod consts {
    /// In-memory flight recorder: the last kithara events, whatever the fmt
    /// layer's filter admits to stdout. A red test's dump (panic or hang)
    /// carries this tail, so the evidence does not depend on guessing the right
    /// per-test tracing filter in advance.
    ///
    /// Two lanes so volume cannot evict signal: `#[kithara::probe]` sites fire
    /// on every call and would flush rare FSM-transition DEBUG lines out of a
    /// shared ring within milliseconds.
    pub(super) const MAX_EVENTS: usize = 256;

    pub(super) const MAX_EVENT_BYTES: usize = 512;

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
    pub(super) const FOLD_COUNTERS: &[&str] = &["seq", "thread_seq"];
}

static EVENTS: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());
static PROBES: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());
static STARTED: OnceLock<WallInstant> = OnceLock::new();

/// One recorded line and how many times it repeated inside the window.
struct Entry {
    first_ns: u128,
    /// Hash of the counter-filtered thread, span, and event fields.
    key: u64,
    last_ns: u128,
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
    RingLayer {
        started: *STARTED.get_or_init(WallInstant::now),
    }
    .with_filter(filter_fn(|meta| classify(meta).is_some()))
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
            let line = clamp(format!(
                "mono_ns={:039}..{:039} {}",
                entry.first_ns, entry.last_ns, entry.line
            ));
            if entry.repeats > 1 {
                format!("{line} (x{repeats})", repeats = entry.repeats)
            } else {
                line
            }
        })
        .collect()
}

struct RingLayer {
    started: WallInstant,
}

enum Lane {
    Event,
    Probe,
}

/// Only kithara traffic: probe events (TRACE, `<crate>_probe` targets, one
/// per call of a probed production fn) and DEBUG-and-coarser events. The
/// recorder keeps their span context even when the fmt filter disables it,
/// without dependency chatter or non-probe TRACE firehoses.
fn classify(meta: &Metadata<'_>) -> Option<Lane> {
    if !meta.target().starts_with("kithara") {
        return None;
    }
    if meta.target().ends_with("_probe") {
        return Some(Lane::Probe);
    }
    (*meta.level() <= Level::DEBUG).then_some(Lane::Event)
}

impl<'writer> FormatFields<'writer> for RingLayer {
    fn format_fields<R: RecordFields>(&self, writer: Writer<'writer>, fields: R) -> fmt::Result {
        DefaultFields::new().format_fields(writer, fields)
    }
}

struct SpanFields {
    fields: FormattedFields<RingLayer>,
    key: DefaultHasher,
}

impl<S> Layer<S> for RingLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut fields = FormattedFields::<Self>::new(String::new());
        if self.format_fields(fields.as_writer(), attrs).is_ok() {
            let mut key = DefaultHasher::new();
            fields.fields.hash(&mut key);
            fields.fields = clamp(fields.fields);
            span.extensions_mut().insert(SpanFields { fields, key });
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        if let Some(span) = extensions.get_mut::<SpanFields>() {
            let _ = self.add_fields(&mut span.fields, values);
            span.fields.fields.hash(&mut span.key);
            span.fields.fields = clamp(std::mem::take(&mut span.fields.fields));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        let Some(lane) = classify(meta) else {
            return;
        };
        let at_ns = self.started.elapsed().as_nanos();
        let thread = std::thread::current();
        let mut text = EventText::default();
        let _ = write!(
            text,
            "{level} {target}: thread={id:?} thread_name={name:?}",
            level = meta.level(),
            target = meta.target(),
            id = thread.id(),
            name = thread.name(),
        );
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                let extensions = span.extensions();
                if let Some(fields) = extensions.get::<SpanFields>() {
                    let _ = write!(
                        LineOnly(&mut text),
                        " {name}{{{fields}}}",
                        name = span.name(),
                        fields = fields.fields,
                    );
                    text.key.write(span.name().as_bytes());
                    text.key.write_u64(fields.key.finish());
                }
            }
        }
        event.record(&mut FieldVisitor(&mut text));
        let ring = match lane {
            Lane::Event => &EVENTS,
            Lane::Probe => &PROBES,
        };
        let (line, key) = text.finish();
        record(ring, &line, key, at_ns);
    }
}

/// One event written once: the line keeps the first
/// [`consts::MAX_EVENT_BYTES`] and the fold key hashes every byte, so a field's
/// `Debug` runs a single time however long its output is.
#[derive(Default)]
struct EventText {
    key: DefaultHasher,
    line: ArrayString<{ consts::MAX_EVENT_BYTES + '\u{2026}'.len_utf8() }>,
    truncated: bool,
}

impl EventText {
    fn push_line(&mut self, text: &str) {
        if self.truncated {
            return;
        }
        let room = consts::MAX_EVENT_BYTES - self.line.len();
        if text.len() <= room {
            self.line.push_str(text);
            return;
        }
        let mut end = room;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.line.push_str(&text[..end]);
        self.truncated = true;
    }

    fn finish(
        mut self,
    ) -> (
        ArrayString<{ consts::MAX_EVENT_BYTES + '\u{2026}'.len_utf8() }>,
        u64,
    ) {
        if self.truncated {
            self.line.push('\u{2026}');
        }
        (self.line, self.key.finish())
    }
}

impl fmt::Write for EventText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.push_line(text);
        self.key.write(text.as_bytes());
        Ok(())
    }
}

/// Text that belongs to the line but not to the fold key.
struct LineOnly<'a>(&'a mut EventText);

impl fmt::Write for LineOnly<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0.push_line(text);
        Ok(())
    }
}

fn record(ring: &Mutex<VecDeque<Entry>>, line: &str, key: u64, at_ns: u128) {
    /// How far back a repeat folds into the entry already in the ring.
    ///
    /// Steady-state volume arrives as a short cycle of a few distinct lines — a
    /// fetch settling, then the two accounting lines it triggers — so a window a
    /// few cycles wide collapses it where a last-line check would not, and a red
    /// test's dump keeps the rare transitions that led to the failure.
    const DEDUP_WINDOW: usize = 16;

    let mut ring = lock(ring);
    let window = ring.len().saturating_sub(DEDUP_WINDOW);
    if let Some(entry) = ring.iter_mut().skip(window).find(|entry| entry.key == key) {
        entry.repeats = entry.repeats.saturating_add(1);
        entry.first_ns = entry.first_ns.min(at_ns);
        entry.last_ns = entry.last_ns.max(at_ns);
        return;
    }
    if ring.len() == consts::MAX_EVENTS {
        ring.pop_front();
    }
    ring.push_back(Entry {
        first_ns: at_ns,
        key,
        last_ns: at_ns,
        line: line.to_owned(),
        repeats: 1,
    });
}

fn clamp(mut text: String) -> String {
    if text.len() <= consts::MAX_EVENT_BYTES {
        return text;
    }
    let mut end = consts::MAX_EVENT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push('\u{2026}');
    text.into_boxed_str().into_string()
}

struct FieldVisitor<'a>(&'a mut EventText);

impl FieldVisitor<'_> {
    fn write_field(
        writer: &mut impl fmt::Write,
        field: &Field,
        value: &dyn fmt::Debug,
    ) -> fmt::Result {
        writer.write_str(" ")?;
        if field.name() != "message" {
            writer.write_str(field.name())?;
            writer.write_str("=")?;
        }
        write!(writer, "{value:?}")
    }
}

impl Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if consts::FOLD_COUNTERS.contains(&field.name()) {
            let _ = Self::write_field(&mut LineOnly(self.0), field, value);
        } else {
            let _ = Self::write_field(self.0, field, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        collections::VecDeque,
        fmt::{self, Write as _},
        hash::{DefaultHasher, Hasher as _},
        sync::{Mutex, MutexGuard, PoisonError},
        thread,
    };

    use tracing::{callsite::Callsite as _, debug, field::Visit as _};
    use tracing_subscriber::layer::SubscriberExt;

    use super::{
        EventText, FieldVisitor, LineOnly,
        consts::{MAX_EVENT_BYTES, MAX_EVENTS},
        layer, lock, probes_tail, record, snapshot, tail,
    };
    use crate::kithara;

    /// The rings are process-global and the capacity tests flood them, so a
    /// concurrent marker lookup would find its entry evicted. Each test holds
    /// this lock from its emissions through its assertions.
    static FLIGHT_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn with_recorder(body: impl FnOnce()) -> MutexGuard<'static, ()> {
        let guard = FLIGHT_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_test_writer())
            .with(layer());
        tracing::subscriber::with_default(subscriber, body);
        guard
    }

    fn without_timestamps(line: &str) -> &str {
        line.split_once(' ').expect("monotonic timestamp prefix").1
    }

    #[derive(Default)]
    struct OwnedEventText {
        key: DefaultHasher,
        line: String,
        truncated: bool,
    }

    impl OwnedEventText {
        fn push_line(&mut self, text: &str) {
            if self.truncated {
                return;
            }
            let room = MAX_EVENT_BYTES - self.line.len();
            if text.len() <= room {
                self.line.push_str(text);
                return;
            }
            let mut end = room;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            self.line.push_str(&text[..end]);
            self.truncated = true;
        }

        fn finish(mut self) -> (String, u64) {
            if self.truncated {
                self.line.push('\u{2026}');
            }
            (self.line, self.key.finish())
        }
    }

    impl fmt::Write for OwnedEventText {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            self.push_line(text);
            self.key.write(text.as_bytes());
            Ok(())
        }
    }

    #[kithara::test]
    fn inline_text_matches_owned_text_at_every_utf8_boundary() {
        for prefix_bytes in 0..=MAX_EVENT_BYTES {
            let prefix = "x".repeat(prefix_bytes);
            for ending in ["", "a", "\u{e9}", "\u{20ac}", "\u{1f980}"] {
                for tail in ["", "after the retained line"] {
                    let mut inline = EventText::default();
                    let mut owned = OwnedEventText::default();
                    for chunk in [prefix.as_str(), "", ending, tail] {
                        inline.write_str(chunk).unwrap();
                        owned.write_str(chunk).unwrap();
                    }
                    let (line, key) = inline.finish();
                    let (expected_line, expected_key) = owned.finish();
                    assert_eq!(line.as_str(), expected_line);
                    assert_eq!(key, expected_key);
                }
            }
        }
    }

    #[kithara::test]
    fn inline_text_preserves_thread_span_and_signed_counter_keys() {
        let callsite = tracing::callsite! {
            name: "flight key parity",
            kind: tracing::metadata::Kind::EVENT,
            fields: message, seq, thread_seq, ready,
        };
        let fields = callsite.metadata().fields();
        let mut context_keys = Vec::new();
        for thread in ["ThreadId(7)", "ThreadId(8)"] {
            for span_key in [11, 12] {
                let mut keys = Vec::new();
                for (seq, thread_seq) in [(-7i64, -9i64), (8, 10)] {
                    let mut inline = EventText::default();
                    let mut owned = OwnedEventText::default();
                    let header = format!(
                        "TRACE kithara_flight_test_probe: thread={thread} thread_name=Some(\"worker\")"
                    );
                    inline.write_str(&header).unwrap();
                    owned.write_str(&header).unwrap();
                    LineOnly(&mut inline).write_str(" worker{lane=11}").unwrap();
                    owned.push_line(" worker{lane=11}");
                    inline.key.write(b"worker");
                    inline.key.write_u64(span_key);
                    owned.key.write(b"worker");
                    owned.key.write_u64(span_key);
                    let mut visitor = FieldVisitor(&mut inline);
                    visitor.record_debug(&fields.field("message").unwrap(), &"parity");
                    visitor.record_debug(&fields.field("seq").unwrap(), &seq);
                    visitor.record_debug(&fields.field("thread_seq").unwrap(), &thread_seq);
                    visitor.record_debug(&fields.field("ready").unwrap(), &true);
                    owned.write_str(" ").unwrap();
                    write!(owned, "{:?}", "parity").unwrap();
                    owned.push_line(&format!(" seq={seq} thread_seq={thread_seq}"));
                    for chunk in [" ", "ready", "="] {
                        owned.write_str(chunk).unwrap();
                    }
                    write!(owned, "{:?}", true).unwrap();
                    let (line, key) = inline.finish();
                    let (expected_line, expected_key) = owned.finish();
                    assert_eq!(line.as_str(), expected_line);
                    assert_eq!(key, expected_key);
                    keys.push(key);
                }
                assert_eq!(keys[0], keys[1]);
                context_keys.push(keys[0]);
            }
        }
        context_keys.sort_unstable();
        context_keys.dedup();
        assert_eq!(context_keys.len(), 4);
    }

    #[kithara::test]
    fn long_debug_runs_once_per_firing_and_hashes_the_unretained_suffix() {
        struct LongDebug<'a> {
            calls: &'a Cell<usize>,
            ending: &'a str,
        }

        impl fmt::Debug for LongDebug<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.calls.set(self.calls.get() + 1);
                for _ in 0..MAX_EVENT_BYTES {
                    f.write_str("\u{1f980}")?;
                }
                f.write_str(self.ending)
            }
        }

        let callsite = tracing::callsite! {
            name: "long flight field",
            kind: tracing::metadata::Kind::EVENT,
            fields: payload,
        };
        let field = callsite.metadata().fields().field("payload").unwrap();
        let calls = Cell::new(0);
        let reference_calls = Cell::new(0);
        let ring = Mutex::new(VecDeque::new());
        for (index, ending) in ["left", "left", "right"].into_iter().enumerate() {
            let mut inline = EventText::default();
            let mut owned = OwnedEventText::default();
            let value = LongDebug {
                calls: &calls,
                ending,
            };
            FieldVisitor(&mut inline).record_debug(&field, &value);
            for chunk in [" ", "payload", "="] {
                owned.write_str(chunk).unwrap();
            }
            write!(
                owned,
                "{:?}",
                LongDebug {
                    calls: &reference_calls,
                    ending,
                }
            )
            .unwrap();
            let (line, key) = inline.finish();
            let (expected_line, expected_key) = owned.finish();
            assert_eq!(line.as_str(), expected_line);
            assert_eq!(key, expected_key);
            record(&ring, &line, key, index as u128);
        }
        assert_eq!(calls.get(), 3);
        assert_eq!(reference_calls.get(), 3);
        let entries = lock(&ring);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].repeats, 2);
        assert_eq!((entries[0].first_ns, entries[0].last_ns), (0, 1));
        assert_eq!(entries[1].repeats, 1);
        assert_eq!(entries[0].line, entries[1].line);
        assert_eq!(
            entries[0].line,
            format!(" payload={}\u{2026}", "\u{1f980}".repeat(125))
        );
        assert_ne!(entries[0].key, entries[1].key);
    }

    #[kithara::test]
    fn a_debug_error_keeps_partial_text_and_following_fields() {
        struct PartialDebug<'a>(&'a Cell<usize>);

        impl fmt::Debug for PartialDebug<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.set(self.0.get() + 1);
                f.write_str("partial")?;
                Err(fmt::Error)
            }
        }

        let callsite = tracing::callsite! {
            name: "fallible flight field",
            kind: tracing::metadata::Kind::EVENT,
            fields: payload, ready,
        };
        let fields = callsite.metadata().fields();
        let calls = Cell::new(0);
        let mut inline = EventText::default();
        let mut visitor = FieldVisitor(&mut inline);
        visitor.record_debug(&fields.field("payload").unwrap(), &PartialDebug(&calls));
        visitor.record_debug(&fields.field("ready").unwrap(), &true);
        let mut owned = OwnedEventText::default();
        for chunk in [" ", "payload", "=", "partial", " ", "ready", "=", "true"] {
            owned.write_str(chunk).unwrap();
        }
        let (line, key) = inline.finish();
        let (expected_line, expected_key) = owned.finish();
        assert_eq!(calls.get(), 1);
        assert_eq!(line.as_str(), " payload=partial ready=true");
        assert_eq!(line.as_str(), expected_line);
        assert_eq!(key, expected_key);
    }

    #[kithara::test]
    fn fold_window_keeps_the_first_line_order_and_saturating_count() {
        for intervening in [15u64, 16] {
            let ring = Mutex::new(VecDeque::new());
            record(&ring, "first sequence", 0, 17);
            for key in 1..=intervening {
                record(&ring, "intervening", key, 23);
            }
            record(&ring, "changed sequence", 0, 7);
            let entries = lock(&ring);
            assert_eq!(entries[0].line, "first sequence");
            assert_eq!(entries[1].key, 1);
            if intervening == 15 {
                assert_eq!(entries.len(), 16);
                assert_eq!(entries[0].repeats, 2);
                assert_eq!((entries[0].first_ns, entries[0].last_ns), (7, 17));
                assert_eq!(entries.back().unwrap().key, 15);
            } else {
                assert_eq!(entries.len(), 18);
                assert_eq!(entries[0].repeats, 1);
                assert_eq!((entries[0].first_ns, entries[0].last_ns), (17, 17));
                assert_eq!(entries.back().unwrap().line, "changed sequence");
            }
        }
        let ring = Mutex::new(VecDeque::new());
        record(&ring, "saturated", 7, 17);
        lock(&ring)[0].repeats = u32::MAX;
        record(&ring, "discarded", 7, 31);
        assert_eq!(
            snapshot(&ring),
            [format!(
                "mono_ns={:039}..{:039} saturated (x{})",
                17,
                31,
                u32::MAX
            )]
        );
    }

    #[kithara::test]
    fn ring_capacity_preserves_exact_insertion_order() {
        let ring = Mutex::new(VecDeque::new());
        for key in 0..=MAX_EVENTS as u64 {
            record(&ring, "unique event", key, u128::from(key));
        }
        let entries = lock(&ring);
        assert_eq!(entries.len(), MAX_EVENTS);
        for (index, entry) in entries.iter().enumerate() {
            assert_eq!(entry.key, (index + 1) as u64);
            assert_eq!(entry.repeats, 1);
            assert_eq!(
                (entry.first_ns, entry.last_ns),
                (entry.key.into(), entry.key.into())
            );
        }
    }

    #[kithara::test]
    fn repeated_events_keep_their_monotonic_interval() {
        let ring = Mutex::new(VecDeque::new());
        record(&ring, "same event", 7, 17);
        record(&ring, "same event", 7, 31);
        record(&ring, "same event", 7, 23);

        assert_eq!(
            snapshot(&ring),
            [format!("mono_ns={:039}..{:039} same event (x3)", 17, 31)]
        );
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
            let _span = tracing::debug_span!(
                target: "kithara_flight_test",
                "worker_lane",
                lane = 7u64,
            )
            .entered();
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
        assert!(folded[0].contains("worker_lane{lane=7}"), "{folded:?}");
        assert!(folded[0].ends_with("(x4)"), "{folded:?}");
    }

    #[kithara::test]
    fn identical_probes_in_different_lanes_do_not_fold() {
        let _guard = with_recorder(|| {
            for lane in [11u64, 12] {
                let _span = tracing::debug_span!(
                    target: "kithara_flight_test",
                    "worker_lane",
                    lane,
                )
                .entered();
                tracing::trace!(
                    target: "kithara_flight_test_probe",
                    probe = "lane_split_marker",
                    seq = 1u64,
                );
            }
        });

        let entries = probes_tail()
            .into_iter()
            .filter(|line| line.contains("lane_split_marker"))
            .collect::<Vec<_>>();

        assert_eq!(entries.len(), 2, "{entries:?}");
        for lane in [11, 12] {
            assert!(
                entries
                    .iter()
                    .any(|line| line.contains(&format!("worker_lane{{lane={lane}}}"))),
                "{entries:?}"
            );
        }
    }

    #[kithara::test(native)]
    fn identical_probes_from_different_workers_do_not_fold() {
        let mut ids = Vec::new();
        let _guard = with_recorder(|| {
            let dispatch = tracing::dispatcher::get_default(Clone::clone);
            for _ in 0..2 {
                let dispatch = dispatch.clone();
                let worker = thread::Builder::new()
                    .name("flight-worker".to_owned())
                    .spawn(move || {
                        tracing::dispatcher::with_default(&dispatch, || {
                            tracing::trace!(
                                target: "kithara_flight_test_probe",
                                probe = "worker_split_marker",
                                seq = 1u64,
                            );
                            thread::current().id()
                        })
                    })
                    .expect("recorder worker starts");
                ids.push(worker.join().expect("recorder worker joins"));
            }
        });

        let entries = probes_tail()
            .into_iter()
            .filter(|line| line.contains("worker_split_marker"))
            .collect::<Vec<_>>();

        assert_ne!(ids[0], ids[1]);
        assert_eq!(entries.len(), 2, "{entries:?}");
        for id in ids {
            assert!(
                entries
                    .iter()
                    .any(|line| line.contains(&format!("thread={id:?}"))),
                "{entries:?}"
            );
        }
        assert!(
            entries
                .iter()
                .all(|line| line.contains("thread_name=Some(\"flight-worker\")")),
            "{entries:?}"
        );
    }

    #[kithara::test]
    fn recorded_span_fields_follow_updates() {
        let _guard = with_recorder(|| {
            let span = tracing::debug_span!(
                target: "kithara_flight_test",
                "worker_lane",
                lane = 19u64,
                ready = tracing::field::Empty,
            );
            let _entered = span.enter();
            tracing::trace!(
                target: "kithara_flight_test_probe",
                probe = "updated_span_marker",
                seq = 1u64,
            );
            span.record("ready", true);
            tracing::trace!(
                target: "kithara_flight_test_probe",
                probe = "updated_span_marker",
                seq = 2u64,
            );
        });

        let entries = probes_tail()
            .into_iter()
            .filter(|line| line.contains("updated_span_marker"))
            .collect::<Vec<_>>();

        assert_eq!(entries.len(), 2, "{entries:?}");
        assert!(entries[0].contains("worker_lane{lane=19}"), "{entries:?}");
        assert!(
            entries[1].contains("worker_lane{lane=19 ready=true}"),
            "{entries:?}"
        );
    }

    #[kithara::test]
    fn a_truncated_parent_span_keeps_lane_repeat_keys_distinct() {
        let context = "\u{1f980}".repeat(MAX_EVENT_BYTES);
        let _guard = with_recorder(|| {
            let _parent = tracing::debug_span!(
                target: "kithara_flight_test",
                "long_span_key_marker",
                context = context.as_str(),
            )
            .entered();
            for lane in [23u64, 24] {
                let _span = tracing::debug_span!(
                    target: "kithara_flight_test",
                    "worker_lane",
                    lane,
                )
                .entered();
                tracing::trace!(
                    target: "kithara_flight_test_probe",
                    probe = "bounded_context_key",
                    seq = 1u64,
                );
            }
        });

        let entries = probes_tail()
            .into_iter()
            .filter(|line| line.contains("long_span_key_marker"))
            .collect::<Vec<_>>();

        assert_eq!(entries.len(), 2, "{entries:?}");
        assert!(entries.iter().all(|line| {
            line.len() <= MAX_EVENT_BYTES + '\u{2026}'.len_utf8() && line.ends_with('\u{2026}')
        }));
        assert_eq!(
            without_timestamps(&entries[0]),
            without_timestamps(&entries[1])
        );
    }

    #[kithara::test]
    fn a_long_span_keeps_lane_fields_after_truncation_distinct() {
        let context = "\u{1f980}".repeat(MAX_EVENT_BYTES);
        let _guard = with_recorder(|| {
            for lane in [23u64, 24] {
                let _span = tracing::debug_span!(
                    target: "kithara_flight_test",
                    "long_lane_key_marker",
                    context = context.as_str(),
                    lane,
                )
                .entered();
                tracing::trace!(
                    target: "kithara_flight_test_probe",
                    probe = "bounded_lane_key",
                    seq = 1u64,
                );
            }
        });

        let entries = probes_tail()
            .into_iter()
            .filter(|line| line.contains("long_lane_key_marker"))
            .collect::<Vec<_>>();

        assert_eq!(entries.len(), 2, "{entries:?}");
        assert!(entries.iter().all(|line| {
            line.len() <= MAX_EVENT_BYTES + '\u{2026}'.len_utf8() && line.ends_with('\u{2026}')
        }));
        assert_eq!(
            without_timestamps(&entries[0]),
            without_timestamps(&entries[1])
        );
    }

    #[kithara::test]
    fn a_long_span_keeps_recorded_lane_updates_distinct() {
        let context = "\u{1f980}".repeat(MAX_EVENT_BYTES);
        let _guard = with_recorder(|| {
            let span = tracing::debug_span!(
                target: "kithara_flight_test",
                "long_lane_update_marker",
                context = context.as_str(),
                lane = tracing::field::Empty,
            );
            let _entered = span.enter();
            for lane in [23u64, 24] {
                span.record("lane", lane);
                tracing::trace!(
                    target: "kithara_flight_test_probe",
                    probe = "bounded_lane_update",
                    seq = 1u64,
                );
            }
        });

        let entries = probes_tail()
            .into_iter()
            .filter(|line| line.contains("long_lane_update_marker"))
            .collect::<Vec<_>>();

        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!(
            without_timestamps(&entries[0]),
            without_timestamps(&entries[1])
        );
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
    fn an_event_field_is_formatted_once() {
        struct Counted<'a>(&'a Cell<usize>);

        impl fmt::Debug for Counted<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.set(self.0.get() + 1);
                f.write_str("counted")
            }
        }

        let calls = Cell::new(0);
        let _guard = FLIGHT_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            debug!(target: "kithara_flight_test", value = ?Counted(&calls), "format once probe");
        });

        assert_eq!(calls.get(), 1);
        let line = tail()
            .into_iter()
            .rev()
            .find(|line| line.contains("format once probe"))
            .expect("the event must be in the tail");
        assert!(line.contains("value=counted"), "{line}");
    }

    #[kithara::test]
    fn fields_differing_after_the_retained_line_do_not_fold() {
        let prefix = "x".repeat(MAX_EVENT_BYTES * 2);
        let _guard = with_recorder(|| {
            for ending in ["left", "right"] {
                let payload = format!("{prefix}{ending}");
                debug!(target: "kithara_flight_test", payload = payload.as_str(), "full field identity");
            }
        });

        let events: Vec<_> = tail()
            .into_iter()
            .filter(|line| line.contains("full field identity"))
            .collect();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(
            events.iter().all(|line| !line.contains("(x2)")),
            "{events:?}"
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
        assert!(line.len() <= MAX_EVENT_BYTES + '\u{2026}'.len_utf8());
        assert!(line.ends_with('\u{2026}'), "{line}");
    }
}
